"""Exercise the development scripts without a build, browser, or network."""

import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import plistlib
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
import sys
import time

tool, *args = sys.argv[1:]
env = dict(os.environ)
control = Path(env.get("DEV_TEST_CONTROL", "/nonexistent"))
if control.exists():
    env.update(json.loads(control.read_text(encoding="utf-8")))
# The fake branvil of builds/<commit> tells its commit by its path.
binary = env.get("DEV_TEST_BINARY", "")
build = Path(binary).parent.name


def setting(name):
    """A setting of the test, or of the build that runs (NAME_<commit>)."""
    return env.get(f"{name}_{build}", env.get(name))


def hold(step):
    """Waits at `step` while the file that DEV_TEST_HOLD_<step> names exists."""
    path = setting(f"DEV_TEST_HOLD_{step}")
    if path:
        Path(path + ".held").touch()
        while Path(path).exists():
            time.sleep(0.01)


def record(event):
    with open(env["DEV_TEST_EVENTS"], "a", encoding="utf-8") as out:
        out.write(json.dumps(event) + "\n")


event = {
    "tool": tool,
    "args": args,
    "pid": os.getpid(),
    "binary": binary,
    "token_present": "BRANVIL_TOKEN" in env,
    "url_present": "BRANVIL_URL" in env,
}
if tool == "cargo":
    target = Path(args[args.index("--target-dir") + 1])
    event["marked"] = (target / ".build-conditions").exists()
record(event)
if tool == "rustc":
    print(env["DEV_TEST_RUSTC"])
if tool == "lsof":
    # The live processes of the fake branvil serve and admin.
    pids = Path(env["DEV_TEST_SERVICE_PIDS"])
    live = []
    for holder in [pids, pids.with_suffix(".admin")]:
        for pid in holder.read_text(encoding="utf-8").split() if holder.exists() else []:
            try:
                os.kill(int(pid), 0)
                live.append(pid)
            except ProcessLookupError:
                pass
    if not live:
        sys.exit(1)
    print("\n".join(live))
if tool == "cargo":
    if args[0] == "clean":
        shutil.rmtree(target)
    elif args[0] == "build":
        # Like Cargo, tag the target directory as a cache only when creating it.
        if not target.exists():
            target.mkdir(parents=True)
            (target / "CACHEDIR.TAG").touch()
        made = target / ("release" if "--release" in args else "debug") / "branvil"
        made.parent.mkdir(parents=True, exist_ok=True)
        made.write_text(
            '#!/bin/sh\nexport DEV_TEST_BINARY="$0"\nexec '
            + shlex.join([sys.executable, sys.argv[0], "branvil"]) + ' "$@"\n',
            encoding="utf-8",
        )
        made.chmod(0o755)
        hold("BUILD")
        sys.exit(int(setting("DEV_TEST_BUILD_EXIT") or "0"))
if tool == "npm" and args[-1] == "ci":
    marker = Path("web/node_modules/.package-lock.json")
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text("installed", encoding="utf-8")
if tool == "branvil":
    command = args[0]
    serving = Path(env["DEV_TEST_SERVING"])
    pids = Path(env["DEV_TEST_SERVICE_PIDS"])
    if command == "init":
        data = Path(args[args.index("--data-dir") + 1])
        data.mkdir(parents=True, exist_ok=True)
        (data / "branvil.redb").touch()
    elif command == "serve":
        if setting("DEV_TEST_SERVE_EXIT"):
            sys.exit(int(setting("DEV_TEST_SERVE_EXIT")))

        def stop(*_):
            serving.unlink(missing_ok=True)
            if pids.exists() and pids.read_text(encoding="utf-8") == f"{os.getpid()}\n":
                pids.unlink()
            record({"tool": "branvil", "args": ["stopped"], "pid": os.getpid(), "binary": binary})
            sys.exit(0)

        signal.signal(signal.SIGTERM, stop)
        hold("SERVE")
        serving.touch()
        # What the fake lsof reports as having the database open.
        written = pids.with_suffix(f".{os.getpid()}.tmp")
        written.write_text(f"{os.getpid()}\n", encoding="utf-8")
        written.rename(pids)
        print("branvil listening on http://127.0.0.1:7878 (base URL http://127.0.0.1:7878)", flush=True)
        release = env.get("DEV_TEST_RELEASE")
        while not (release and Path(release).exists()):
            time.sleep(0.01)
        stop()
    elif command == "member":
        if not serving.exists():
            print("Branvil is not reachable", file=sys.stderr)
            sys.exit(1)
        print(json.dumps({"items": [
            {"name": "bot", "kind": "agent"},
            {"name": "keeper", "kind": "owner"},
        ]}, indent=2))
    elif command == "backup":
        to = Path(args[args.index("--to") + 1])
        to.mkdir(parents=True)
        (to / "manifest.json").write_text("{}", encoding="utf-8")
        print(f"backup written to {to}")
    elif command == "admin":
        db = Path(args[args.index("--data-dir") + 1]) / "branvil.redb"
        # It has the database open while it runs.
        pids.with_suffix(".admin").write_text(f"{os.getpid()}\n", encoding="utf-8")
        hold(args[1].upper())
        if args[1] == "upgrade":
            if setting("DEV_TEST_UPGRADE_EXIT"):
                print("upgrade failed: test", file=sys.stderr)
                sys.exit(int(setting("DEV_TEST_UPGRADE_EXIT")))
            copy = setting("DEV_TEST_UPGRADE_COPY")
            if setting("DEV_TEST_UPGRADE_OUTPUT"):
                print(setting("DEV_TEST_UPGRADE_OUTPUT"))
            elif copy:
                Path(copy).touch()
                print(f"upgraded {db} from schema version 10 to 11: test")
                print(f"the database as it was: {copy}")
            else:
                print(f"{db} already has schema version 11")
        elif args[1] == "check" and setting("DEV_TEST_CHECK_EXIT"):
            sys.exit(int(setting("DEV_TEST_CHECK_EXIT")))
    elif command == "open":
        if "--print" in args:
            print("http://example.invalid/-/enter#test-only-link")
            sys.exit(int(env.get("DEV_TEST_PRINT_EXIT", "0")))
        sys.exit(int(env.get("DEV_TEST_BROWSER_EXIT", "0")))
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
        # Registered first, so it runs after every process is stopped.
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "checkout with spaces"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "web").mkdir()
        for script in ["dev.sh", "clean-stale-target.sh", "notify.sh", "serve.sh", "update.sh"]:
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
        # Every tool that could reach a real service or a real process is fake.
        for tool in ["npm", "cargo", "git", "rustc", "lsof", "osascript"]:
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

    def spawn(self, script, *args, output, **extra_env):
        """Starts a script in a session of its own, stopped as a whole at the end."""
        with output.open("w", encoding="utf-8") as out:
            proc = subprocess.Popen(
                ["bash", str(self.repo / "scripts" / script), *args],
                cwd=self.root,
                env={
                    **self.env,
                    "BRANVIL_DATA_DIR": str(self.repo / "data with spaces"),
                    "BRANVIL_TOKEN": "test-unrelated-credential",
                    "BRANVIL_URL": "http://example.invalid/unrelated",
                    **extra_env,
                },
                stdout=out,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        self.addCleanup(self.stop_group, proc)
        return proc

    @staticmethod
    def stop_group(proc):
        """Stops every process of the script's group, also after the script ended:
        its children (a server, an update, a build) can outlive it."""
        for sig in (signal.SIGTERM, signal.SIGKILL):
            deadline = time.monotonic() + 5
            try:
                os.killpg(proc.pid, sig)
                while time.monotonic() < deadline:
                    proc.poll()
                    os.killpg(proc.pid, 0)
                    time.sleep(0.02)
            except ProcessLookupError:
                break
        proc.wait(timeout=5)

    def wait_until(self, predicate, proc=None, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            if proc is not None:
                self.assertIsNone(proc.poll(), "The script exited before the expected event")
            time.sleep(0.02)
        self.fail("The script did not reach the expected event")


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


class DevScriptTests(ScriptTestCase):
    def setUp(self):
        super().setUp()
        self.runs = 0

    def start(self, *args, **extra_env):
        self.runs += 1
        output = self.root / f"output-{self.runs}.txt"
        release = self.root / f"release-{self.runs}"
        proc = self.spawn(
            "dev.sh", *args, output=output, DEV_TEST_RELEASE=str(release), **extra_env
        )
        self.wait_until(
            lambda: "Branvil is running." in output.read_text(encoding="utf-8"), proc, 10
        )
        return proc, output, release

    def finish(self, run):
        proc, _, release = run
        release.touch()
        self.assertEqual(proc.wait(timeout=5), 0)

    def opens(self):
        return [event for event in self.events("branvil") if event["args"][0] == "open"]

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

    def test_the_launcher_runs_a_debug_build(self):
        self.finish(self.start("--no-open"))
        [build] = self.events("cargo")
        self.assertNotIn("--release", build["args"])
        [serve] = [e for e in self.events("branvil") if e["args"][0] == "serve"]
        self.assertEqual(serve["binary"], str(self.target / "debug/branvil"))

    def test_install_builds_a_release_and_copies_it(self):
        result = subprocess.run(
            ["bash", str(self.repo / "scripts/dev.sh"), "--install", "builds/abc"],
            cwd=self.root,
            env=self.env,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0)
        [build] = self.events("cargo")
        self.assertIn("--release", build["args"])
        self.assertEqual(
            [call["args"] for call in self.events("npm")],
            [["--prefix", "web", "ci"], ["--prefix", "web", "run", "build"]],
        )
        installed = self.repo / "builds/abc/branvil"
        self.assertEqual(
            installed.read_text(encoding="utf-8"),
            (self.target / "release/branvil").read_text(encoding="utf-8"),
        )
        self.assertTrue(os.access(installed, os.X_OK))
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
        self.wait_until(lambda: len(self.opens()) == 2, run[0], 10)
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
        self.wait_until(lambda: len(self.opens()) == 2, run[0], 10)
        self.finish(run)
        server = next(
            event for event in self.events("branvil") if event["args"][0] == "serve"
        )
        with self.assertRaises(ProcessLookupError):
            os.kill(server["pid"], 0)

    def test_successful_open_does_not_print_another_link(self):
        run = self.start()
        self.wait_until(lambda: len(self.opens()) == 1, run[0], 10)
        self.finish(run)
        self.assertEqual(len(self.opens()), 1)
        self.assertNotIn("--print", self.opens()[0]["args"])

    def test_termination_after_browser_failure_stops_the_child(self):
        run = self.start(DEV_TEST_BROWSER_EXIT="7")
        proc = run[0]
        self.wait_until(lambda: len(self.opens()) == 2, proc, 10)
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
    """A Branvil stand-in: git http-backend behind HTTP Basic, as Branvil's Smart
    HTTP asks for it, and the checks of commits for a Bearer token."""

    CHECKS = "/api/v1/repos/acme%2Fdemo/commits/"

    def __init__(self, root, env, user, token):
        basic = "Basic " + base64.b64encode(f"{user}:{token}".encode()).decode()
        bearer = f"Bearer {token}"
        self.users = []
        self.fetches = 0
        self.checks = {}
        self.check_requests = []
        server = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                if self.path.startswith(server.CHECKS):
                    self.checks()
                else:
                    self.backend()

            do_POST = do_GET

            def answer(self, status, payload=b"", headers=()):
                self.send_response(status)
                for header in headers:
                    self.send_header(*header)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def checks(self):
                sha = self.path[len(server.CHECKS):].removesuffix("/checks")
                server.check_requests.append(sha)
                if self.headers.get("Authorization") != bearer:
                    return self.answer(401)
                answer = server.checks.get(sha, 404)
                if isinstance(answer, int):  # an error status
                    return self.answer(answer, b'{"error": {"code": "test"}}')
                self.answer(200, json.dumps(answer).encode())

            def backend(self):
                given = self.headers.get("Authorization")
                if given:
                    server.users.append(base64.b64decode(given.split()[1]).decode().split(":")[0])
                if given != basic:
                    return self.answer(401, headers=[("WWW-Authenticate", 'Basic realm="test"')])
                path, _, query = self.path.partition("?")
                if path.endswith("/info/refs"):
                    server.fetches += 1
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
                self.answer(status, payload, headers)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_port}"
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()


def gone(pid):
    """Whether process `pid` (or group -`pid`) has ended and been reaped."""
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    return False


SUCCESS = {"checks": [{"name": "test", "status": "completed", "conclusion": "success"}],
           "checks_expected": True}


class ServeScriptTests(ScriptTestCase):
    """scripts/serve.sh running the installed build and scripts/update.sh."""

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
            DEV_TEST_CONTROL=str(self.root / "control.json"),
        )
        self.backups = self.root / "home/branvil-backups"
        self.builds = self.repo / "builds"
        self.data = self.repo / "data with spaces"
        (self.data / "secrets").mkdir(parents=True)
        (self.data / "branvil.redb").touch()
        (self.data / "secrets/owner.token").write_text("test-owner-token\n", encoding="utf-8")
        (self.repo / ".gitignore").write_text(
            "/target/\n/builds/\n/data with spaces/\n/web/node_modules/\n", encoding="utf-8"
        )
        origin = self.root / "git/acme/demo.git"
        self.git("init", "--bare", "-b", "main", str(origin), cwd=self.root)
        self.git("init", "-b", "main")
        self.git("add", "-A")
        self.git("commit", "-m", "base")
        self.git("push", str(origin), "main")
        self.server = GitServer(self.root, self.env, "keeper", "test-owner-token")
        self.addCleanup(self.server.close)
        self.git("remote", "add", "origin", f"{self.server.url}/git/acme/demo.git")
        # A helper of the user's that the update must not use.
        self.helper_used = self.root / "helper-used"
        self.git(
            "config",
            "credential.helper",
            f"!touch {shlex.quote(str(self.helper_used))}; false",
        )
        self.upstream = self.root / "upstream"
        self.git("clone", str(origin), str(self.upstream), cwd=self.root)
        self.git("switch", "-c", "work")
        self.base = self.git("rev-parse", "HEAD")
        self.outputs = 0

    def git(self, *args, cwd=None):
        return subprocess.run(
            ["git", *args],
            cwd=cwd or self.repo,
            env=self.env,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def merge(self, checks=None):
        """Merges a commit on origin's main; returns its sha. `checks` is what
        GET .../commits/{sha}/checks answers (an int: that error status)."""
        news = self.upstream / "NEWS"
        news.write_text(news.read_text(encoding="utf-8") + "merged\n" if news.exists() else "merged\n",
                        encoding="utf-8")
        self.git("add", "NEWS", cwd=self.upstream)
        self.git("commit", "-m", "merged on main", cwd=self.upstream)
        self.git("push", "origin", "main", cwd=self.upstream)
        sha = self.git("rev-parse", "HEAD", cwd=self.upstream)
        if checks is not None:
            self.server.checks[sha] = checks
        return sha

    def output(self, name):
        self.outputs += 1
        return self.root / f"{name}-{self.outputs}.txt"

    def watch(self, *args, running=True, **extra_env):
        """Starts scripts/serve.sh and waits until its service runs."""
        output = self.output("serve")
        proc = self.spawn("serve.sh", "--interval", "1", *args, output=output, **extra_env)
        if running:
            self.wait_until(self.service_pid, proc)
        return proc, output

    def update(self, *args, **extra_env):
        """Runs scripts/update.sh to its end; returns its exit status and output."""
        output = self.output("update")
        proc = self.spawn("update.sh", *args, output=output, **extra_env)
        return proc.wait(timeout=180), output.read_text(encoding="utf-8")

    def control(self, **settings):
        Path(self.env["DEV_TEST_CONTROL"]).write_text(json.dumps(settings), encoding="utf-8")

    def service_pid(self):
        pids = Path(self.env["DEV_TEST_SERVICE_PIDS"])
        return int(pids.read_text(encoding="utf-8")) if pids.exists() else None


    def current(self):
        path = self.builds / "current"
        return path.read_text(encoding="utf-8").strip() if path.exists() else None

    def attempted(self):
        path = self.builds / "attempted"
        return path.read_text(encoding="utf-8").strip() if path.exists() else None

    def binary(self, sha):
        return str(self.builds / sha / "branvil")

    def installed(self):
        return sorted(p.name for p in self.builds.iterdir() if p.is_dir())

    def serves(self):
        return [e["binary"] for e in self.events("branvil") if e["args"][0] == "serve"]

    def notifications(self):
        return [e["args"][-1] for e in self.events("osascript")]

    def steps(self):
        steps = []
        for event in self.events():
            tool, args = event["tool"], event["args"]
            if tool == "branvil" and args[0] != "member":
                steps.append(" ".join(args[:2]) if args[0] == "admin" else args[0])
            elif tool == "cargo" and args[0] == "build":
                steps.append("build")
        return steps

    def rounds_pass(self):
        """Waits until a round of scripts/update.sh --auto that fetched after
        now has ended: it saw everything set before."""
        fetches = self.server.fetches + 1
        self.wait_until(lambda: self.server.fetches >= fetches)
        self.wait_until(lambda: not (self.builds / "updating").exists())

    def finished(self, proc, output):
        self.wait_until(lambda: "Update finished" in output.read_text(encoding="utf-8"), proc)

    def assert_runs(self, sha, pid=None):
        self.assertEqual(self.current(), sha)
        self.wait_until(
            lambda: self.service_pid() is not None and self.serves()[-1] == self.binary(sha)
        )
        if pid is not None:
            self.assertEqual(self.service_pid(), pid)

    def test_head_is_installed_and_served_and_started_again_when_it_dies(self):
        proc, output = self.watch("--interval", "3600")
        self.assertEqual(self.current(), self.base)
        self.assertIsNone(self.attempted())
        [build] = self.events("cargo")
        self.assertIn("--release", build["args"])
        self.assertEqual(self.serves(), [self.binary(self.base)])
        [serve] = [e for e in self.events("branvil") if e["args"][0] == "serve"]
        self.assertEqual(serve["args"][-1], str(self.data))
        first = self.service_pid()
        os.kill(first, signal.SIGKILL)
        self.wait_until(lambda: self.service_pid() not in (None, first), proc)
        self.assertEqual(self.serves(), [self.binary(self.base)] * 2)
        log = output.read_text(encoding="utf-8")
        self.assertIn(f"branvil serve of {self.base} exited with status 137", log)
        self.assertEqual(self.cargo_commands(), ["build"], "Installed once")

    def test_a_build_that_keeps_exiting_at_its_start_is_stopped(self):
        proc, output = self.watch("--interval", "3600")
        self.control(DEV_TEST_SERVE_EXIT="1")
        os.kill(self.service_pid(), signal.SIGKILL)
        self.wait_until(self.notifications, proc)
        self.assertIsNone(self.current())
        [message] = self.notifications()
        self.assertIn(f"branvil serve of {self.base} exited at its start 3 times", message)
        self.assertIn(message, output.read_text(encoding="utf-8"))
        starts = len(self.serves())
        self.assertLessEqual(starts, 3)
        time.sleep(2)
        self.assertEqual(len(self.serves()), starts, "Not started again")
        # A start again installs nothing and starts nothing.
        proc.terminate()
        self.assertEqual(proc.wait(timeout=10), 143)
        proc, output = self.watch("--interval", "3600", running=False)
        time.sleep(2.5)
        self.assertIsNone(proc.poll())
        self.assertEqual(output.read_text(encoding="utf-8"), "")
        self.assertEqual(self.cargo_commands(), ["build"])
        self.assertEqual(len(self.serves()), starts)
        self.control()
        (self.builds / "current").write_text(f"{self.base}\n", encoding="utf-8")
        self.wait_until(lambda: len(self.serves()) > starts, proc)
        self.assert_runs(self.base)

    def test_a_failed_first_install_is_tried_again_at_the_next_start(self):
        self.control(DEV_TEST_BUILD_EXIT="101")
        proc, output = self.watch(running=False)
        self.wait_until(self.notifications, proc)
        self.assertIn("Could not install the checkout's HEAD", self.notifications()[0])
        self.assertFalse(self.builds.exists())
        proc.terminate()
        self.assertEqual(proc.wait(timeout=10), 143)
        self.control()
        self.watch("--interval", "3600")
        self.assertEqual(self.cargo_commands(), ["build", "build"])
        self.assert_runs(self.base)
        self.assertIsNone(self.attempted())

    def test_nothing_starts_while_an_admin_command_has_the_database_open(self):
        self.watch("--interval", "3600")
        hold = self.root / "hold"
        hold.touch()
        new = self.merge()
        self.control(**{f"DEV_TEST_HOLD_UPGRADE_{new}": str(hold)})
        proc = self.spawn("update.sh", output=self.output("update"))
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        # Only the update is killed; its admin upgrade goes on.
        os.kill(proc.pid, signal.SIGKILL)
        proc.wait(timeout=5)
        starts = len(self.serves())
        time.sleep(2.5)
        self.assertEqual(len(self.serves()), starts, "Not started beside admin upgrade")
        hold.unlink()
        self.assert_runs(self.base)
        self.assertEqual(self.notifications(), [])

    def test_a_second_signal_does_not_cut_the_report_short(self):
        self.watch("--interval", "3600")
        hold = self.root / "hold"
        hold.touch()
        new = self.merge()
        self.control(**{f"DEV_TEST_SERVE_EXIT_{new}": "23",
                        f"DEV_TEST_HOLD_SERVE_{self.base}": str(hold)})
        output = self.output("update")
        proc = self.spawn("update.sh", output=output)
        # The report waits for the previous build, held at its start.
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        proc.terminate()
        proc.send_signal(signal.SIGINT)
        time.sleep(0.5)
        hold.unlink()
        self.assertEqual(proc.wait(timeout=30), 1)
        [message] = self.notifications()
        self.assertIn(f"not upgraded; Branvil runs {self.base} again", message)
        self.assertFalse((self.builds / "updating").exists())
        self.assertEqual(self.attempted(), new)

    def test_naming_another_build_switches_to_it(self):
        self.watch("--interval", "3600")
        first = self.service_pid()
        shutil.copytree(self.builds / self.base, self.builds / "other")
        (self.builds / "current").write_text("other\n", encoding="utf-8")
        self.wait_until(lambda: self.service_pid() not in (None, first))
        self.assertTrue(gone(first))
        self.assertEqual(self.serves()[-1], self.binary("other"))

    def test_nothing_is_done_while_main_is_the_running_build(self):
        self.watch()
        self.rounds_pass()
        self.assertEqual(self.server.check_requests, [])
        self.assertEqual(self.steps(), ["build", "serve"])

    def test_unfinished_checks_are_waited_for_then_the_update_runs(self):
        new = self.merge({"checks": [], "checks_expected": True})
        proc, output = self.watch()
        self.rounds_pass()
        self.server.checks[new] = {
            "checks": [{"name": "test", "status": "in_progress", "conclusion": None}],
            "checks_expected": True,
        }
        self.rounds_pass()
        self.assertEqual(self.steps(), ["build", "serve"], "No update while checks run")
        self.assertIsNone(self.attempted())
        self.server.checks[new] = SUCCESS
        self.finished(proc, output)
        self.assertEqual(
            self.steps(),
            ["build", "serve", "backup", "build", "stopped",
             "admin upgrade", "admin check", "serve"],
            "Built before the service stops",
        )
        self.assert_runs(new)
        self.assertEqual(self.serves(), [self.binary(self.base), self.binary(new)])
        upgrade, check = [e for e in self.events("branvil") if e["args"][0] == "admin"]
        self.assertEqual(upgrade["binary"], self.binary(new))
        self.assertEqual(check["binary"], self.binary(new))
        backup = next(e for e in self.events("branvil") if e["args"][0] == "backup")
        self.assertEqual(backup["binary"], self.binary(self.base), "Backed up by the old build")
        self.assertFalse(backup["token_present"])
        self.assertFalse(backup["url_present"])
        self.assertEqual(backup["args"][backup["args"].index("--data-dir") + 1], str(self.data))
        self.assertEqual(self.installed(), sorted([self.base, new]), "The previous build stays")
        [made] = self.backups.iterdir()
        self.assertRegex(made.name, r"^branvil-update-\d{8}T\d{6}Z$")
        self.assertEqual(self.git("branch", "--show-current"), "main")
        self.assertEqual(self.git("rev-parse", "HEAD"), new)
        self.assertEqual(
            {e["args"][-1] for e in self.events("lsof")}, {str(self.data / "branvil.redb")}
        )
        self.assertEqual(set(self.server.users), {"keeper"}, "Fetched as the owner")
        self.assertFalse(self.helper_used.exists(), "The user's credential helper")
        self.assertNotIn("test-owner-token", output.read_text(encoding="utf-8"))
        self.assertNotIn("test-owner-token", self.log.read_text(encoding="utf-8"))
        self.assertEqual(self.notifications(), [])
        requests = len(self.server.check_requests)
        self.rounds_pass()
        self.assertEqual(len(self.server.check_requests), requests, "Not read again")
        # The next update keeps the build it replaces, and only that one.
        newer = self.merge(SUCCESS)
        self.wait_until(
            lambda: output.read_text(encoding="utf-8").count("Update finished") == 2, proc
        )
        self.assert_runs(newer)
        self.assertEqual(self.installed(), sorted([new, newer]))

    def test_failed_checks_are_not_installed_nor_read_again(self):
        new = self.merge({
            "checks": [
                {"name": "test", "status": "completed", "conclusion": "success"},
                {"name": "lint", "status": "completed", "conclusion": "failure"},
            ],
            "checks_expected": True,
        })
        self.watch()
        self.wait_until(lambda: self.attempted() == new)
        self.rounds_pass()
        self.assertEqual(self.server.check_requests, [new])
        self.assertEqual(self.steps(), ["build", "serve"])
        self.assertEqual(self.current(), self.base)
        self.assertEqual(self.notifications(), [])

    def test_a_commit_without_expected_checks_is_installed(self):
        new = self.merge({"checks": [], "checks_expected": False})
        proc, output = self.watch()
        self.finished(proc, output)
        self.assert_runs(new)

    def test_failures_before_the_decision_are_notified_once_each(self):
        new = self.merge(500)
        proc, output = self.watch()
        for answer in [500, 401, 404, {"checks_expected": True}]:
            self.server.checks[new] = answer
            self.rounds_pass()
        log = output.read_text(encoding="utf-8")
        for text in ["HTTP 500", "HTTP 401", "HTTP 404", "TypeError"]:
            self.assertIn(text, log)
        [message] = self.notifications()
        self.assertIn(f"stopped while reading the checks of {new}", message)
        self.assertEqual(self.steps(), ["build", "serve"])
        self.assertIsNone(self.attempted(), "Read again in the next round")
        token = self.data / "secrets/owner.token"
        token.unlink()
        self.wait_until(lambda: len(self.notifications()) == 2, proc)
        self.assertIn("stopped while fetching origin", self.notifications()[1])
        token.write_text("test-owner-token\n", encoding="utf-8")
        self.server.checks[new] = SUCCESS
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(len(self.notifications()), 2)
        self.assertFalse((self.builds / "notified").exists())

    def test_a_failed_build_keeps_the_service_running_and_is_not_tried_again(self):
        proc, output = self.watch()
        pid = self.service_pid()
        self.control(DEV_TEST_BUILD_EXIT="101")
        failing = self.merge(SUCCESS)
        self.wait_until(self.notifications, proc)
        self.rounds_pass()
        self.assertEqual(self.steps(), ["build", "serve", "backup", "build"])
        self.assert_runs(self.base, pid)
        self.assertEqual(self.attempted(), failing)
        [message] = self.notifications()
        self.assertIn(f"The update to {failing} stopped while installing", message)
        self.assertIn(f"Branvil still runs {self.base}", message)
        self.assertIn(message, output.read_text(encoding="utf-8"))
        self.control()
        new = self.merge(SUCCESS)
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(self.installed(), sorted([self.base, new]))

    def test_a_killed_update_is_tried_again(self):
        proc, output = self.watch()
        hold = self.root / "hold"
        hold.touch()
        self.control(DEV_TEST_HOLD_BUILD=str(hold))
        new = self.merge(SUCCESS)
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        os.kill(int((self.builds / "updating").read_text(encoding="utf-8")), signal.SIGKILL)
        self.control()
        builder = self.events("cargo")[-1]["pid"]
        hold.unlink()
        self.wait_until(lambda: gone(builder))
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(self.steps().count("backup"), 2, "The killed run, then the retry")
        self.assertEqual(self.notifications(), [])

    def test_a_failed_check_after_an_upgrade_stays_stopped(self):
        new = self.merge(SUCCESS)
        copy = self.data / "branvil-schema-10-test.redb"
        proc, output = self.watch(**{
            f"DEV_TEST_UPGRADE_COPY_{new}": str(copy),
            f"DEV_TEST_CHECK_EXIT_{new}": "1",
        })
        self.wait_until(self.notifications, proc)
        self.wait_until(lambda: not (self.builds / "updating").exists())
        self.assertEqual(
            self.steps(),
            ["build", "serve", "backup", "build", "stopped", "admin upgrade", "admin check"],
        )
        self.assertIsNone(self.current())
        self.assertIsNone(self.service_pid())
        [message] = self.notifications()
        self.assertIn("while checking the data directory", message)
        self.assertIn("upgraded; Branvil is stopped", message)
        [backup] = self.backups.iterdir()
        log = output.read_text(encoding="utf-8")
        for text in [
            f"Backup taken before the update: {backup}",
            f"Database before the upgrade: {copy}",
            f"echo {self.base} >{self.builds}/current",
            f"echo {new} >{self.builds}/current",
            "docs/backup-restore.md",
        ]:
            self.assertIn(text, log)
        self.assertEqual(self.installed(), sorted([self.base, new]), "Kept to go back")
        fetches = self.server.fetches
        time.sleep(2.5)
        self.assertEqual(self.server.fetches, fetches, "No update round while stopped")
        self.assertEqual(self.serves(), [self.binary(self.base)])
        (self.builds / "current").write_text(f"{self.base}\n", encoding="utf-8")
        self.assert_runs(self.base)

    def test_term_stops_the_service_and_a_running_update(self):
        proc, output = self.watch()
        pid = self.service_pid()
        hold = self.root / "hold"
        hold.touch()
        self.control(DEV_TEST_HOLD_BUILD=str(hold))
        self.merge(SUCCESS)
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        proc.terminate()
        self.wait_until(lambda: gone(pid))
        time.sleep(0.5)
        self.assertIsNone(proc.poll(), "It waits for the update's running step")
        hold.unlink()
        self.assertEqual(proc.wait(timeout=10), 143)
        self.assertTrue(gone(-proc.pid), "Nothing of it is left")
        self.assertNotIn("admin upgrade", self.steps())
        self.assertEqual(self.current(), self.base)
        self.assertIsNone(self.attempted(), "Tried again at the next start")
        self.assertIn("stopped while installing", output.read_text(encoding="utf-8"))

    def test_plist_runs_this_script_with_the_options_and_path(self):
        result = subprocess.run(
            ["bash", str(self.repo / "scripts/serve.sh"), "--plist", "--data-dir", "a & b"],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            check=True,
        )
        plist = plistlib.loads(result.stdout)
        self.assertEqual(plist["Label"], "com.guildbotics.branvil")
        self.assertEqual(
            plist["ProgramArguments"],
            ["/bin/bash", str(self.repo / "scripts/serve.sh"),
             "--data-dir", str(self.repo / "a & b"), "--interval", "300"],
        )
        self.assertEqual(plist["WorkingDirectory"], str(self.repo))
        self.assertEqual(plist["EnvironmentVariables"], {"PATH": self.env["PATH"]})
        self.assertTrue(plist["RunAtLoad"])
        self.assertTrue(plist["KeepAlive"])
        self.assertEqual(plist["ExitTimeOut"], 120)
        self.assertEqual(plist["StandardOutPath"], plist["StandardErrorPath"])
        self.assertEqual(self.events(), [], "Only printed")

    def check_outcome(self, stage, applied, how, runs):
        """An update run by hand that fails at `stage`, or is interrupted there
        with TERM, after an upgrade that changed the database (`applied`) or
        did not. `runs` is the build builds/current then names: the previous
        one, the new one, or None (Branvil stays stopped)."""
        self.watch("--interval", "3600")
        pid = self.service_pid()
        new = self.merge()
        settings = {}
        if applied:
            settings[f"DEV_TEST_UPGRADE_COPY_{new}"] = str(self.data / "copy.redb")
        hold = self.root / "hold"
        if how == "fails":
            name, value = {
                "install": ("DEV_TEST_BUILD_EXIT", "101"),
                "upgrade": (f"DEV_TEST_UPGRADE_EXIT_{new}", "1"),
                "check": (f"DEV_TEST_CHECK_EXIT_{new}", "1"),
                "start": (f"DEV_TEST_SERVE_EXIT_{new}", "23"),
            }[stage]
            settings[name] = value
        else:
            hold.touch()
            step = {"install": "BUILD", "upgrade": "UPGRADE", "check": "CHECK", "start": "SERVE"}
            suffix = "" if stage == "install" else f"_{new}"
            settings[f"DEV_TEST_HOLD_{step[stage]}{suffix}"] = str(hold)
        self.control(**settings)
        output = self.output("update")
        proc = self.spawn("update.sh", output=output)
        if how == "interrupted":
            self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
            proc.terminate()
            time.sleep(0.2)
            hold.unlink()
        status = proc.wait(timeout=120)
        log = output.read_text(encoding="utf-8")
        self.assertNotEqual(status, 0, log)
        self.assertEqual(status == 143, how == "interrupted", log)
        expected = {"previous": self.base, "new": new, None: None}[runs]
        self.assertEqual(self.current(), expected, log)
        if expected is None:
            time.sleep(1.5)
            self.assertIsNone(self.service_pid(), "Stopped")
        else:
            self.assert_runs(expected, pid if stage == "install" else None)
        self.assertFalse((self.builds / "hold").exists())
        self.assertEqual(self.attempted(), new if how == "fails" else None, log)
        # An interruption at the upgrade takes effect after the check.
        words = {
            "install": "installing builds/",
            "upgrade": "checking" if how == "interrupted" else "upgrading",
            "check": "checking the data directory",
            "start": "starting the new build",
        }[stage]
        [message] = self.notifications()
        self.assertIn(f"stopped while {words}", message)

    # Updates run by hand, while scripts/serve.sh runs (it checks only hourly).

    def test_an_upgrade_report_other_than_left_as_it_is_counts_as_an_upgrade(self):
        self.watch("--interval", "3600")
        new = self.merge()
        for output in [f"{self.data}/branvil.redb already has schema version 11 (test)",
                       "upgraded in a way this script does not know"]:
            with self.subTest(output=output):
                self.control(**{f"DEV_TEST_UPGRADE_OUTPUT_{new}": output,
                                f"DEV_TEST_CHECK_EXIT_{new}": "1"})
                status, log = self.update()
                self.assertEqual(status, 1, log)
                self.assertIsNone(self.current(), "Stopped: the database may be upgraded")
                self.assertIn("Database before the upgrade: not reported", log)
                (self.builds / "current").write_text(f"{self.base}\n", encoding="utf-8")
                self.assert_runs(self.base)

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
        self.watch("--interval", "3600")
        new = self.merge()
        status, output = self.update()
        self.assertEqual(status, 0, output)
        self.assert_runs(new)
        made_now = [
            p for p in self.backups.iterdir()
            if p not in (older, old, *others, notes)
        ]
        self.assertEqual(len(made_now), 1)
        self.assertTrue((made_now[0] / "manifest.json").is_file())
        for path in (older, old, *others, notes):
            self.assertTrue(path.exists(), path)
        for path in (oldest, failed):
            self.assertFalse(path.exists(), path)
        self.assertEqual(self.server.check_requests, [], "A manual update reads no checks")

    def test_a_main_that_is_the_running_build_is_left_alone(self):
        self.watch("--interval", "3600")
        pid = self.service_pid()
        status, output = self.update()
        self.assertEqual(status, 0, output)
        self.assertIn(f"Branvil already runs {self.base}; nothing to update.", output)
        self.assert_runs(self.base, pid)
        self.assertEqual(self.steps(), ["build", "serve"])
        self.assertEqual(self.git("branch", "--show-current"), "work")
        self.assertFalse(self.backups.exists())
        self.assertEqual(self.notifications(), [])

    def test_no_running_service_ends_the_update_before_anything(self):
        status, output = self.update()
        self.assertEqual(status, 1)
        self.assertIn("Branvil is not running", output)
        self.assertIn("scripts/serve.sh", output)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.base)
        self.assertEqual(self.steps(), [])
        self.assertFalse(self.backups.exists())

    def test_uncommitted_changes_end_the_update_before_anything(self):
        self.watch("--interval", "3600")
        pid = self.service_pid()
        self.merge()
        (self.repo / "Cargo.toml").write_text("changed", encoding="utf-8")
        status, output = self.update()
        self.assertEqual(status, 1)
        self.assertIn("uncommitted changes", output)
        self.assertIn("Cargo.toml", output)
        self.assert_runs(self.base, pid)
        self.assertEqual(self.git("rev-parse", "HEAD"), self.base)
        self.assertEqual(self.steps(), ["build", "serve"])
        self.assertFalse(self.backups.exists())
        self.assertFalse((self.builds / "updating").exists())

    def test_another_running_update_is_waited_for(self):
        self.watch("--interval", "3600")
        self.merge()
        lock = self.builds / "updating"
        lock.write_text(f"{os.getpid()}\n", encoding="utf-8")
        status, output = self.update()
        self.assertEqual(status, 1)
        self.assertIn(f"Another update is running (pid {os.getpid()})", output)
        self.assertEqual(self.steps(), ["build", "serve"])
        # One whose process is gone does not count.
        lock.write_text("999999\n", encoding="utf-8")
        status, output = self.update()
        self.assertEqual(status, 0, output)
        self.assertFalse(lock.exists())

    def test_a_main_that_cannot_fast_forward_stops_while_the_service_runs(self):
        self.watch("--interval", "3600")
        pid = self.service_pid()
        self.git("switch", "main")
        (self.repo / "LOCAL").write_text("local\n", encoding="utf-8")
        self.git("add", "LOCAL")
        self.git("commit", "-m", "local only")
        local = self.git("rev-parse", "HEAD")
        new = self.merge()
        status, output = self.update()
        self.assertNotEqual(status, 0)
        self.assertIn(f"The update to {new} stopped while updating the checkout", output)
        self.assertIn(f"Branvil still runs {self.base}", output)
        self.assert_runs(self.base, pid)
        self.assertEqual(self.git("rev-parse", "HEAD"), local)
        self.assertEqual(self.steps(), ["build", "serve", "backup"])

    def test_an_unreadable_owner_token_stops_before_the_fetch(self):
        self.watch("--interval", "3600")
        pid = self.service_pid()
        (self.data / "secrets/owner.token").unlink()
        status, output = self.update()
        self.assertNotEqual(status, 0)
        self.assertIn("stopped while fetching origin", output)
        self.assertEqual(self.server.users, [], "No request without the token")
        self.assert_runs(self.base, pid)


# What builds/current names after an update ends at each stage, by how it ends
# and whether admin upgrade had changed the database: (stage, applied, how, runs).
OUTCOMES = [
    ("install", False, "fails", "previous"),
    ("install", False, "interrupted", "previous"),
    ("upgrade", False, "fails", "previous"),
    ("upgrade", False, "interrupted", "previous"),
    ("upgrade", True, "interrupted", "new"),
    ("check", False, "fails", "previous"),
    ("check", True, "fails", None),
    ("check", False, "interrupted", "previous"),
    ("check", True, "interrupted", "new"),
    ("start", False, "fails", "previous"),
    ("start", True, "fails", None),
    ("start", False, "interrupted", "previous"),
    ("start", True, "interrupted", "new"),
]
for _stage, _applied, _how, _runs in OUTCOMES:
    setattr(
        ServeScriptTests,
        f"test_outcome_{_stage}_{'upgraded' if _applied else 'not_upgraded'}_{_how}",
        lambda self, row=(_stage, _applied, _how, _runs): self.check_outcome(*row),
    )


if __name__ == "__main__":
    unittest.main()
