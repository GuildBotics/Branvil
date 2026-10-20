"""Exercise the development scripts without a build, browser, or network."""

import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
HELPER = r'''
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import time

tool, *args = sys.argv[1:]
event = {
    "tool": tool,
    "args": args,
    "pid": os.getpid(),
    "token_present": "BRANVIL_TOKEN" in os.environ,
    "url_present": "BRANVIL_URL" in os.environ,
}
if tool == "cargo":
    target = Path(args[args.index("--target-dir") + 1])
    event["marked"] = (target / ".build-conditions").exists()
with open(os.environ["DEV_TEST_EVENTS"], "a", encoding="utf-8") as out:
    out.write(json.dumps(event) + "\n")
if tool == "rustc":
    print(os.environ["DEV_TEST_RUSTC"])
if tool == "lsof":
    pids = Path(os.environ["DEV_TEST_SERVICE_PIDS"])
    if not pids.exists():
        sys.exit(1)
    print(pids.read_text(encoding="utf-8"), end="")
if tool == "service":
    def stop(*_):
        Path(os.environ["DEV_TEST_SERVING"]).unlink()
        with open(os.environ["DEV_TEST_EVENTS"], "a", encoding="utf-8") as out:
            out.write(json.dumps({"tool": "service", "args": ["stopped"]}) + "\n")
        sys.exit(0)
    signal.signal(signal.SIGTERM, stop)
    Path(os.environ["DEV_TEST_SERVING"]).touch()
    pids = Path(os.environ["DEV_TEST_SERVICE_PIDS"])
    pids.with_suffix(".tmp").write_text(f"{os.getpid()}\n", encoding="utf-8")
    pids.with_suffix(".tmp").rename(pids)
    while True:
        time.sleep(0.01)
if tool == "cargo":
    if args[0] == "clean":
        shutil.rmtree(target)
    elif args[0] == "build":
        # Like Cargo, tag the target directory as a cache only when creating it.
        if not target.exists():
            target.mkdir(parents=True)
            (target / "CACHEDIR.TAG").touch()
        binary = target / "debug/branvil"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_text(
            "#!/bin/sh\nexec " + shlex.join([sys.executable, sys.argv[0], "branvil"]) + ' "$@"\n',
            encoding="utf-8",
        )
        binary.chmod(0o755)
        hold = os.environ.get("DEV_TEST_HOLD_REBUILD")
        builds = sum(
            json.loads(line)["args"][:1] == ["build"]
            for line in open(os.environ["DEV_TEST_EVENTS"], encoding="utf-8")
            if '"cargo"' in line
        )
        if hold and builds > 1:
            Path(hold + ".held").touch()
            while Path(hold).exists():
                time.sleep(0.01)
        sys.exit(int(os.environ.get("DEV_TEST_BUILD_EXIT", "0")))
if tool == "npm" and args[-1] == "ci":
    marker = Path("web/node_modules/.package-lock.json")
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text("installed", encoding="utf-8")
if tool == "branvil":
    command = args[0]
    if command == "init":
        data = Path(args[args.index("--data-dir") + 1])
        data.mkdir(parents=True, exist_ok=True)
        (data / "branvil.redb").touch()
    elif command == "serve":
        if "DEV_TEST_SERVE_EXIT" in os.environ:
            sys.exit(int(os.environ["DEV_TEST_SERVE_EXIT"]))
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
        serving = Path(os.environ["DEV_TEST_SERVING"])
        serving.touch()
        try:
            print("branvil listening on http://127.0.0.1:7878 (base URL http://127.0.0.1:7878)", flush=True)
            while not Path(os.environ["DEV_TEST_RELEASE"]).exists():
                time.sleep(0.01)
        finally:
            serving.unlink()
    elif command == "member":
        if not Path(os.environ["DEV_TEST_SERVING"]).exists():
            print("Branvil is not reachable", file=sys.stderr)
            sys.exit(1)
        print(json.dumps({"items": [
            {"name": "bot", "kind": "agent"},
            {"name": "keeper", "kind": "owner"},
        ]}, indent=2))
    elif command == "backup":
        if os.environ.get("DEV_TEST_RESTART_SERVICE"):
            # The owner restarts the service while the update runs.
            pids = Path(os.environ["DEV_TEST_SERVICE_PIDS"])
            os.kill(int(pids.read_text(encoding="utf-8")), signal.SIGKILL)
            pids.unlink()
            subprocess.Popen([sys.executable, sys.argv[0], "service"], start_new_session=True)
            while not pids.exists():
                time.sleep(0.01)
        to = Path(args[args.index("--to") + 1])
        to.mkdir(parents=True)
        (to / "manifest.json").write_text("{}", encoding="utf-8")
        print(f"backup written to {to}")
    elif command == "admin":
        db = Path(args[args.index("--data-dir") + 1]) / "branvil.redb"
        if args[1] == "upgrade":
            copy = os.environ.get("DEV_TEST_UPGRADE_COPY")
            if copy:
                Path(copy).touch()
                print(f"upgraded {db} from schema version 10 to 11: test")
                print(f"the database as it was: {copy}")
            else:
                print(f"{db} already has schema version 11")
        elif args[1] == "check":
            sys.exit(int(os.environ.get("DEV_TEST_CHECK_EXIT", "0")))
    elif command == "open":
        if "--print" in args:
            print("http://example.invalid/-/enter#test-only-link")
            sys.exit(int(os.environ.get("DEV_TEST_PRINT_EXIT", "0")))
        sys.exit(int(os.environ.get("DEV_TEST_BROWSER_EXIT", "0")))
'''


CARGO_TOML = """[package]
name = "branvil"
version = "0.1.0"

[dependencies]
serde = "1"

[profile.dev]
debug = "line-tables-only"

[profile.dev.package."*"]
debug = false
"""


@unittest.skipUnless(os.name == "posix", "The scripts require a POSIX shell")
class ScriptTestCase(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="branvil-dev-test-")
        self.root = Path(self.temp.name)
        self.repo = self.root / "checkout with spaces"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "web").mkdir()
        for script in ["dev.sh", "clean-stale-target.sh", "update.sh"]:
            shutil.copy(ROOT / "scripts" / script, self.repo / "scripts" / script)
        self.manifest = self.repo / "Cargo.toml"
        self.manifest.write_text(CARGO_TOML, encoding="utf-8")
        self.target = self.repo / "target"
        self.lock = self.repo / "web/package-lock.json"
        self.lock.write_text("{}", encoding="utf-8")
        os.utime(self.lock, (1000, 1000))
        self.tools = self.root / "tools"
        self.tools.mkdir()
        helper = self.root / "helper.py"
        helper.write_text(HELPER, encoding="utf-8")
        for tool in ["npm", "cargo", "git", "rustc", "lsof"]:
            path = self.tools / tool
            path.write_text(
                "#!/bin/sh\nexec "
                + shlex.join([sys.executable, str(helper), tool])
                + ' "$@"\n',
                encoding="utf-8",
            )
            path.chmod(0o755)
        self.log = self.root / "events.jsonl"
        self.env = {
            **os.environ,
            "PATH": str(self.tools) + os.pathsep + os.environ["PATH"],
            "DEV_TEST_EVENTS": str(self.log),
            "DEV_TEST_RUSTC": "rustc 1.96.0 (test)\nrelease: 1.96.0",
            "DEV_TEST_SERVICE_PIDS": str(self.root / "service.pids"),
            "DEV_TEST_SERVING": str(self.root / "serving"),
        }

    def tearDown(self):
        self.temp.cleanup()

    def events(self, tool=None):
        if not self.log.exists():
            return []
        events = [
            json.loads(line)
            for line in self.log.read_text(encoding="utf-8").splitlines(keepends=True)
            if line.endswith("\n")
        ]
        return [event for event in events if tool is None or event["tool"] == tool]

    def cargo_commands(self):
        return [event["args"][0] for event in self.events("cargo")]


class CleanStaleTargetTests(ScriptTestCase):
    def clean(self, *args):
        subprocess.run(
            ["bash", str(self.repo / "scripts/clean-stale-target.sh"), *args, str(self.target)],
            cwd=self.root,
            env=self.env,
            check=True,
            capture_output=True,
        )

    def build(self):
        self.target.mkdir()
        self.clean("--mark")

    def test_target_without_a_mark_is_cleaned_away(self):
        (self.target / "debug").mkdir(parents=True)
        (self.target / "debug/stale").touch()
        self.clean()
        self.assertEqual(self.cargo_commands(), ["clean"])
        clean = self.events("cargo")[0]["args"]
        self.assertEqual(clean[clean.index("--target-dir") + 1], str(self.target))
        self.assertEqual(clean[clean.index("--manifest-path") + 1], str(self.manifest))
        self.assertFalse(self.target.exists(), "Cargo has to create the target")

    def test_missing_target_is_left_to_cargo(self):
        for args in [(), ("--mark",)]:
            with self.subTest(args=args):
                self.clean(*args)
                self.assertEqual(self.cargo_commands(), [])
                self.assertFalse(self.target.exists(), "Cargo has to create the target")

    def test_mark_records_the_conditions(self):
        self.build()
        self.assertEqual(self.cargo_commands(), [])
        mark = (self.target / ".build-conditions").read_text(encoding="utf-8")
        self.assertIn("rustc 1.96.0 (test)", mark)
        self.assertIn('[profile.dev.package."*"]\ndebug = false', mark)
        self.assertNotIn("[dependencies]", mark)

    def test_conditions_decide_whether_to_clean(self):
        for change, should_clean in [
            ("nothing", False),
            ("rustc", True),
            ("profile", True),
            ("dependency", False),
        ]:
            with self.subTest(change=change):
                shutil.rmtree(self.target, ignore_errors=True)
                self.manifest.write_text(CARGO_TOML, encoding="utf-8")
                self.env["DEV_TEST_RUSTC"] = "rustc 1.96.0 (test)\nrelease: 1.96.0"
                self.build()
                (self.target / "kept").touch()
                if change == "rustc":
                    self.env["DEV_TEST_RUSTC"] = "rustc 1.97.0 (test)\nrelease: 1.97.0"
                elif change == "profile":
                    self.manifest.write_text(
                        CARGO_TOML.replace("debug = false", "debug = true"),
                        encoding="utf-8",
                    )
                elif change == "dependency":
                    self.manifest.write_text(
                        CARGO_TOML.replace('serde = "1"', 'serde = "1.0.200"'),
                        encoding="utf-8",
                    )
                before = len(self.events("cargo"))
                self.clean()
                self.assertEqual(
                    [event["args"][0] for event in self.events("cargo")[before:]],
                    ["clean"] if should_clean else [],
                )
                self.assertEqual((self.target / "kept").exists(), not should_clean)
                if should_clean:
                    self.build()
                self.clean()
                self.assertEqual(
                    len(self.events("cargo")),
                    before + (1 if should_clean else 0),
                    "A second run under the same conditions must not clean",
                )


class LauncherTestCase(ScriptTestCase):
    script = "dev.sh"
    running = "Branvil is running."

    def setUp(self):
        super().setUp()
        self.runs = []

    def tearDown(self):
        for proc, _, _ in self.runs:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait(timeout=5)
        super().tearDown()

    def wait_for(self, proc, predicate):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if predicate():
                return
            self.assertIsNone(proc.poll(), "Launcher exited before the expected event")
            time.sleep(0.01)
        self.fail("Launcher did not reach the expected event")

    def start(self, *args, **extra_env):
        index = len(self.runs)
        output = self.root / f"output-{index}.txt"
        release = self.root / f"release-{index}"
        with output.open("w", encoding="utf-8") as out:
            proc = subprocess.Popen(
                ["bash", str(self.repo / "scripts" / self.script), *args],
                cwd=self.root,
                env={
                    **self.env,
                    "BRANVIL_DATA_DIR": str(self.repo / "data with spaces"),
                    "BRANVIL_TOKEN": "test-unrelated-credential",
                    "BRANVIL_URL": "http://example.invalid/unrelated",
                    "DEV_TEST_RELEASE": str(release),
                    **extra_env,
                },
                stdout=out,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        self.runs.append((proc, output, release))
        self.wait_for(
            proc,
            lambda: self.running in output.read_text(encoding="utf-8"),
        )
        return proc, output, release

    def finish(self, run):
        proc, _, release = run
        release.touch()
        self.assertEqual(proc.wait(timeout=5), 0)

    def opens(self):
        return [event for event in self.events("branvil") if event["args"][0] == "open"]


class DevScriptTests(LauncherTestCase):

    def test_stale_target_is_cleaned_before_the_build_and_marked_after_it(self):
        for state in ["stale", "missing"]:
            with self.subTest(state=state):
                shutil.rmtree(self.target, ignore_errors=True)
                if state == "stale":
                    (self.target / "debug").mkdir(parents=True)
                    (self.target / "debug/stale").touch()
                before = len(self.events("cargo"))
                self.finish(self.start("--no-open"))
                self.finish(self.start("--no-open"))
                self.assertEqual(
                    [(e["args"][0], e["marked"]) for e in self.events("cargo")[before:]],
                    ([("clean", False)] if state == "stale" else [])
                    + [("build", False), ("build", True)],
                    "Clean before the first build only, and mark after each build",
                )
                self.assertFalse((self.target / "debug/stale").exists())
                self.assertTrue((self.target / "CACHEDIR.TAG").exists())
                self.assertTrue((self.target / ".build-conditions").is_file())

    def test_failed_build_is_marked_and_ends_the_launcher(self):
        result = subprocess.run(
            ["bash", str(self.repo / "scripts/dev.sh"), "--no-open"],
            cwd=self.root,
            env={**self.env, "DEV_TEST_BUILD_EXIT": "101"},
            capture_output=True,
        )
        self.assertEqual(result.returncode, 101)
        self.assertEqual(self.cargo_commands(), ["build"])
        self.assertTrue((self.target / ".build-conditions").is_file())
        self.assertEqual(self.events("branvil"), [])

    def test_build_only_builds_and_exits(self):
        result = subprocess.run(
            ["bash", str(self.repo / "scripts/dev.sh"), "--build-only"],
            cwd=self.root,
            env=self.env,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(self.cargo_commands(), ["build"])
        self.assertEqual(
            [call["args"] for call in self.events("npm")],
            [["--prefix", "web", "ci"], ["--prefix", "web", "run", "build"]],
        )
        self.assertEqual(self.events("branvil"), [], "Neither init nor serve")

    def test_dependency_installation_tracks_the_lockfile(self):
        modules = self.repo / "web/node_modules"
        for state, should_install in [
            ("missing-directory", True),
            ("missing-marker", True),
            ("current", False),
            ("changed-lock", True),
        ]:
            with self.subTest(state=state):
                shutil.rmtree(modules, ignore_errors=True)
                os.utime(self.lock, (1000, 1000))
                if state != "missing-directory":
                    modules.mkdir()
                if state in ("current", "changed-lock"):
                    marker = modules / ".package-lock.json"
                    marker.write_text("installed", encoding="utf-8")
                    os.utime(marker, (2000, 2000))
                if state == "changed-lock":
                    self.lock.write_text('{"updated": true}', encoding="utf-8")
                    os.utime(self.lock, (3000, 3000))
                before = len(self.events("npm"))
                self.finish(self.start("--no-open"))
                calls = self.events("npm")[before:]
                self.assertEqual(
                    [call["args"] for call in calls],
                    ([['--prefix', 'web', 'ci']] if should_install else [])
                    + [['--prefix', 'web', 'run', 'build']],
                )

    def test_browser_failure_prints_link_with_same_local_credentials(self):
        run = self.start(DEV_TEST_BROWSER_EXIT="7")
        self.wait_for(run[0], lambda: len(self.opens()) == 2)
        self.finish(run)
        first, fallback = self.opens()
        self.assertNotIn("--print", first["args"])
        self.assertIn("--print", fallback["args"])
        for call in (first, fallback):
            self.assertFalse(call["token_present"])
            self.assertFalse(call["url_present"])
            self.assertEqual(call["args"][-1], str(self.repo / "data with spaces"))
        output = run[1].read_text(encoding="utf-8")
        self.assertIn("http://example.invalid/-/enter#test-only-link", output)
        self.assertIn("60 seconds", output)

    def test_failed_print_fallback_keeps_server_running_until_stopped(self):
        run = self.start(DEV_TEST_BROWSER_EXIT="7", DEV_TEST_PRINT_EXIT="8")
        proc = run[0]
        self.wait_for(proc, lambda: len(self.opens()) == 2)
        self.finish(run)
        server = next(
            event for event in self.events("branvil") if event["args"][0] == "serve"
        )
        with self.assertRaises(ProcessLookupError):
            os.kill(server["pid"], 0)

    def test_successful_open_does_not_print_another_link(self):
        run = self.start()
        self.wait_for(run[0], lambda: len(self.opens()) == 1)
        self.finish(run)
        self.assertEqual(len(self.opens()), 1)
        self.assertNotIn("--print", self.opens()[0]["args"])

    def test_termination_after_browser_failure_stops_the_child(self):
        run = self.start(DEV_TEST_BROWSER_EXIT="7")
        proc = run[0]
        self.wait_for(proc, lambda: len(self.opens()) == 2)
        proc.terminate()
        self.assertEqual(proc.wait(timeout=5), 143)
        server = next(
            event for event in self.events("branvil") if event["args"][0] == "serve"
        )
        with self.assertRaises(ProcessLookupError):
            os.kill(server["pid"], 0)

    def test_no_open_skips_both_browser_and_print(self):
        self.finish(self.start("--no-open", DEV_TEST_BROWSER_EXIT="7"))
        self.assertEqual(self.opens(), [])


class GitServer:
    """git http-backend behind HTTP Basic, as Branvil's Smart HTTP asks for it."""

    def __init__(self, root, env, user, token):
        expected = "Basic " + base64.b64encode(f"{user}:{token}".encode()).decode()
        self.users = []
        users = self.users

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                self.backend()

            do_POST = do_GET

            def backend(self):
                given = self.headers.get("Authorization")
                if given:
                    users.append(base64.b64decode(given.split()[1]).decode().split(":")[0])
                if given != expected:
                    self.send_response(401)
                    self.send_header("WWW-Authenticate", 'Basic realm="test"')
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                path, _, query = self.path.partition("?")
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                out = subprocess.run(
                    ["git", "http-backend"],
                    input=body,
                    env={
                        **env,
                        "GIT_PROJECT_ROOT": str(root),
                        "GIT_HTTP_EXPORT_ALL": "1",
                        "REQUEST_METHOD": self.command,
                        "PATH_INFO": path,
                        "QUERY_STRING": query,
                        "CONTENT_TYPE": self.headers.get("Content-Type", ""),
                        "CONTENT_LENGTH": str(len(body)),
                        "HTTP_CONTENT_ENCODING": self.headers.get("Content-Encoding", ""),
                        "GIT_PROTOCOL": self.headers.get("Git-Protocol", ""),
                        "REMOTE_USER": user,
                    },
                    capture_output=True,
                    check=True,
                ).stdout
                head, _, payload = out.partition(b"\r\n\r\n")
                status, headers = 200, []
                for line in head.decode().split("\r\n"):
                    name, _, value = line.partition(": ")
                    if name.lower() == "status":
                        status = int(value.split()[0])
                    else:
                        headers.append((name, value))
                self.send_response(status)
                for header in headers:
                    self.send_header(*header)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


class UpdateScriptTests(LauncherTestCase):
    script = "update.sh"
    running = "Update finished"
    STAMP = "branvil-update-20260101T000000Z"

    def setUp(self):
        super().setUp()
        # Real Git, kept away from the user's configuration.
        (self.tools / "git").unlink()
        self.env.update(
            HOME=str(self.root / "home"),
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_TERMINAL_PROMPT="0",
            GIT_AUTHOR_NAME="Test Author",
            GIT_AUTHOR_EMAIL="author@example.invalid",
            GIT_COMMITTER_NAME="Test Author",
            GIT_COMMITTER_EMAIL="author@example.invalid",
        )
        self.backups = self.root / "home/branvil-backups"
        self.data = self.repo / "data with spaces"
        (self.data / "secrets").mkdir(parents=True)
        (self.data / "branvil.redb").touch()
        (self.data / "secrets/owner.token").write_text("test-owner-token\n", encoding="utf-8")
        (self.repo / ".gitignore").write_text(
            "/target/\n/data with spaces/\n/web/node_modules/\n", encoding="utf-8"
        )
        origin = self.root / "origin.git"
        self.git("init", "--bare", "-b", "main", str(origin), cwd=self.root)
        self.git("init", "-b", "main")
        self.git("add", "-A")
        self.git("commit", "-m", "base")
        self.git("push", str(origin), "main")
        self.server = GitServer(self.root, self.env, "keeper", "test-owner-token")
        self.addCleanup(self.server.close)
        self.git("remote", "add", "origin", f"{self.server.url}/origin.git")
        # A helper of the user's that the update must not use.
        self.helper_used = self.root / "helper-used"
        self.git(
            "config",
            "credential.helper",
            f"!touch {shlex.quote(str(self.helper_used))}; false",
        )
        upstream = self.root / "upstream"
        self.git("clone", str(origin), str(upstream), cwd=self.root)
        (upstream / "NEWS").write_text("merged\n", encoding="utf-8")
        self.git("add", "NEWS", cwd=upstream)
        self.git("commit", "-m", "merged on main", cwd=upstream)
        self.git("push", "origin", "main", cwd=upstream)
        self.origin_main = self.git("rev-parse", "HEAD", cwd=upstream)
        self.git("switch", "-c", "work")
        self.base = self.git("rev-parse", "HEAD")
        # The build the service runs before the update.
        subprocess.run(
            [str(self.tools / "cargo"), "build", "--target-dir", str(self.target)],
            env=self.env,
            check=True,
        )
        self.log.unlink()

    def git(self, *args, cwd=None):
        return subprocess.run(
            ["git", *args],
            cwd=cwd or self.repo,
            env=self.env,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def start_service(self):
        service = subprocess.Popen(
            [sys.executable, str(self.root / "helper.py"), "service"], env=self.env
        )
        threading.Thread(target=service.wait, daemon=True).start()
        self.addCleanup(lambda: service.poll() is None and service.kill())
        self.wait_for(service, lambda: Path(self.env["DEV_TEST_SERVICE_PIDS"]).exists())
        return service

    def update(self, **extra_env):
        """Runs an update that ends before scripts/dev.sh takes over."""
        return subprocess.run(
            ["bash", str(self.repo / "scripts/update.sh"), "--no-open"],
            cwd=self.root,
            env={**self.env, "BRANVIL_DATA_DIR": str(self.data), **extra_env},
            capture_output=True,
            text=True,
            timeout=30,
        )

    def steps(self):
        steps = []
        for event in self.events():
            tool, args = event["tool"], event["args"]
            if tool == "branvil" and args[0] == "member":
                continue  # who the owner is, and whether the service answers
            if tool == "branvil":
                steps.append(" ".join(args[:2]) if args[0] == "admin" else args[0])
            elif tool == "cargo" and args[0] == "build":
                steps.append("build")
            elif tool == "service" and args:
                steps.append("stopped")
        return steps

    def assert_untouched(self, service):
        self.assertIsNone(service.poll(), "The service must keep running")
        self.assertEqual(self.git("rev-parse", "HEAD"), self.base)
        self.assertEqual(self.steps(), [])
        self.assertFalse(self.backups.exists())

    def test_update_backs_up_stops_fast_forwards_upgrades_and_starts(self):
        service = self.start_service()
        run = self.start("--no-open")
        self.finish(run)
        self.assertEqual(
            self.steps(),
            ["backup", "stopped", "build", "admin upgrade", "admin check", "build", "serve"],
        )
        self.assertIsNotNone(service.poll())
        self.assertEqual(
            {e["args"][-1] for e in self.events("lsof")}, {str(self.data / "branvil.redb")}
        )
        backup = next(e for e in self.events("branvil") if e["args"][0] == "backup")
        self.assertFalse(backup["token_present"])
        self.assertFalse(backup["url_present"])
        self.assertEqual(backup["args"][backup["args"].index("--data-dir") + 1], str(self.data))
        [made] = self.backups.iterdir()
        self.assertRegex(made.name, r"^branvil-update-\d{8}T\d{6}Z$")
        self.assertEqual(self.git("branch", "--show-current"), "main")
        self.assertEqual(self.git("rev-parse", "HEAD"), self.origin_main)
        serve = next(e for e in self.events("branvil") if e["args"][0] == "serve")
        self.assertEqual(serve["args"][-1], str(self.data))
        self.assertEqual(self.opens(), [], "--no-open goes to scripts/dev.sh")
        self.assertEqual(set(self.server.users), {"keeper"}, "Fetched as the owner")
        self.assertFalse(self.helper_used.exists(), "The user's credential helper")
        output = run[1].read_text(encoding="utf-8")
        self.assertNotIn("test-owner-token", output)

    def test_only_the_newest_three_of_its_own_backups_are_kept(self):
        def made(name, complete=True):
            path = self.backups / name
            path.mkdir(parents=True)
            if complete:
                (path / "manifest.json").write_text("{}", encoding="utf-8")
            return path

        oldest = made("branvil-update-20260101T000000Z")
        older = made("branvil-update-20260201T000000Z")
        old = made("branvil-update-20260301T000000Z")
        failed = made("branvil-update-20260401T000000Z", complete=False)
        others = [made("before-schema-11"), made("branvil-update-latest")]
        notes = self.backups / "branvil-update-20250101T000000Z.txt"
        notes.write_text("not a backup", encoding="utf-8")
        self.start_service()
        self.finish(self.start("--no-open"))
        new = [
            p for p in self.backups.iterdir()
            if p not in (older, old, *others, notes)
        ]
        self.assertEqual(len(new), 1)
        self.assertTrue((new[0] / "manifest.json").is_file())
        for path in (older, old, *others, notes):
            self.assertTrue(path.exists(), path)
        for path in (oldest, failed):
            self.assertFalse(path.exists(), path)

    def test_no_running_service_ends_the_update_before_anything(self):
        result = self.update()
        self.assertEqual(result.returncode, 1)
        self.assertIn("Branvil is not running", result.stderr)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.base)
        self.assertEqual(self.steps(), [])
        self.assertFalse(self.backups.exists())

    def test_uncommitted_changes_end_the_update_before_anything(self):
        service = self.start_service()
        (self.repo / "Cargo.toml").write_text("changed", encoding="utf-8")
        result = self.update()
        self.assertEqual(result.returncode, 1)
        self.assertIn("uncommitted changes", result.stderr)
        self.assertIn("Cargo.toml", result.stderr)
        self.assert_untouched(service)

    def test_a_main_that_cannot_fast_forward_stops_while_the_service_runs(self):
        service = self.start_service()
        self.git("switch", "main")
        (self.repo / "LOCAL").write_text("local\n", encoding="utf-8")
        self.git("add", "LOCAL")
        self.git("commit", "-m", "local only")
        local = self.git("rev-parse", "HEAD")
        result = self.update()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Update stopped while updating the checkout", result.stderr)
        self.assertIn("still running", result.stderr)
        self.assertIn(f"Checkout before the update: {local}", result.stderr)
        self.assertIsNone(service.poll())
        self.assertEqual(self.git("rev-parse", "HEAD"), local)
        self.assertEqual(self.steps(), ["backup"])

    def test_a_current_database_is_left_as_it_is(self):
        self.start_service()
        run = self.start("--no-open")
        self.finish(run)
        output = run[1].read_text(encoding="utf-8")
        self.assertIn("already has schema version 11", output)
        self.assertNotIn("the database as it was", output)
        self.assertEqual(sorted(p.name for p in self.data.iterdir()), ["branvil.redb", "secrets"])
        self.assertIn("serve", self.steps())

    def test_a_service_that_does_not_start_tells_how_to_go_back(self):
        self.start_service()
        result = self.update(DEV_TEST_SERVE_EXIT="23")
        self.assertEqual(result.returncode, 23)
        self.assertEqual(self.steps()[-1], "serve")
        [backup] = self.backups.iterdir()
        for text in [
            "Update stopped while starting the service",
            "Branvil is stopped.",
            f"Checkout before the update: {self.base}",
            f"Backup taken before the update: {backup}",
        ]:
            self.assertIn(text, result.stderr)

    def test_ctrl_c_after_the_start_stops_the_service_without_a_report(self):
        self.start_service()
        proc, output, _ = self.start("--no-open")
        proc.send_signal(signal.SIGINT)
        self.assertEqual(proc.wait(timeout=10), 130)
        self.assertFalse(Path(self.env["DEV_TEST_SERVING"]).exists(), "serve stopped")
        self.assertNotIn("Update stopped", output.read_text(encoding="utf-8"))

    def test_the_service_running_at_the_stop_is_the_one_stopped(self):
        first = self.start_service()
        self.finish(self.start("--no-open", DEV_TEST_RESTART_SERVICE="1"))
        self.assertIsNotNone(first.poll())
        self.assertEqual(self.steps()[:2], ["backup", "stopped"], "The restarted one")

    def test_ctrl_c_during_the_rebuild_ends_after_the_running_step(self):
        self.start_service()
        hold = self.root / "hold"
        hold.touch()
        proc = subprocess.Popen(
            ["bash", str(self.repo / "scripts/update.sh"), "--no-open"],
            cwd=self.root,
            env={**self.env, "BRANVIL_DATA_DIR": str(self.data), "DEV_TEST_HOLD_REBUILD": str(hold)},
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        self.addCleanup(lambda: proc.poll() is None and os.killpg(proc.pid, signal.SIGKILL))
        self.wait_for(proc, lambda: Path(str(hold) + ".held").exists())
        proc.send_signal(signal.SIGINT)
        time.sleep(1)
        self.assertIsNone(proc.poll(), "dev.sh waits for its running step")
        hold.unlink()
        _, stderr = proc.communicate(timeout=10)
        self.assertEqual(proc.returncode, 130)
        self.assertNotIn("serve", self.steps())
        self.assertIn("Update stopped while starting the service", stderr)
        self.assertIn("Branvil is stopped.", stderr)

    def test_an_unreadable_owner_token_stops_before_the_fetch(self):
        service = self.start_service()
        (self.data / "secrets/owner.token").unlink()
        result = self.update()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Update stopped while updating the checkout", result.stderr)
        self.assertEqual(self.server.users, [], "No request without the token")
        self.assertIsNone(service.poll())

    def test_a_failed_check_tells_where_it_stopped_and_how_to_go_back(self):
        self.start_service()
        copy = self.data / "branvil-schema-10-test.redb"
        result = self.update(DEV_TEST_UPGRADE_COPY=str(copy), DEV_TEST_CHECK_EXIT="1")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(
            self.steps(), ["backup", "stopped", "build", "admin upgrade", "admin check"]
        )
        [backup] = self.backups.iterdir()
        for text in [
            "Update stopped while checking the data directory",
            "Branvil is stopped.",
            f"Checkout before the update: {self.base}",
            f"Backup taken before the update: {backup}",
            f"Database before the upgrade: {copy}",
            f"scripts/dev.sh --data-dir {self.data}",
            "docs/backup-restore.md",
        ]:
            self.assertIn(text, result.stderr)


if __name__ == "__main__":
    unittest.main()
