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


def alive(path):
    """Whether file `path` names a running process."""
    try:
        os.kill(int(path.read_text(encoding="utf-8")), 0)
        return True
    except (OSError, ValueError):
        return False


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
if tool == "osascript":
    hold("NOTIFY")  # recorded above, before it holds
if tool == "rustc":
    print(env["DEV_TEST_RUSTC"])
if tool == "git":
    while args[0] == "-c":
        args = args[2:]
    if args[0] == "rev-parse":
        print(env.get("DEV_TEST_HEAD", "abc"))
    if args[0] == "status":
        print(env.get("DEV_TEST_STATUS", ""), end="")
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
        if not setting("DEV_TEST_BUILD_NO_BINARY"):
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
    # The schema version of this build, and the one of the database (its
    # content, 11 when empty).
    schema = setting("DEV_TEST_SCHEMA") or "11"
    data = Path(args[args.index("--data-dir") + 1]) if "--data-dir" in args else Path("/nonexistent")

    def database():
        db = data / "branvil.redb"
        return (db.read_text(encoding="utf-8").strip() if db.exists() else "") or "11"
    if command == "init":
        data = Path(args[args.index("--data-dir") + 1])
        data.mkdir(parents=True, exist_ok=True)
        (data / "branvil.redb").touch()
    elif command == "serve":
        if setting("DEV_TEST_SERVE_EXIT"):
            sys.exit(int(setting("DEV_TEST_SERVE_EXIT")))
        if database() != schema:
            print(f"branvil: the database has schema version {database()}", file=sys.stderr)
            sys.exit(1)

        def stop(*_):
            serving.unlink(missing_ok=True)
            if pids.exists() and pids.read_text(encoding="utf-8") == f"{os.getpid()}\n":
                pids.unlink()
            record({"tool": "branvil", "args": ["stopped"], "pid": os.getpid(), "binary": binary})
            sys.exit(0)

        signal.signal(signal.SIGTERM, signal.SIG_IGN if setting("DEV_TEST_SERVE_IGNORE_TERM") else stop)
        hold("SERVE")
        serving.write_text(str(os.getpid()), encoding="utf-8")
        # Its pid, for the tests.
        written = pids.with_suffix(f".{os.getpid()}.tmp")
        written.write_text(f"{os.getpid()}\n", encoding="utf-8")
        written.rename(pids)
        print("branvil listening on http://127.0.0.1:7878 (base URL http://127.0.0.1:7878)", flush=True)
        release = env.get("DEV_TEST_RELEASE")
        while not (release and Path(release).exists()):
            time.sleep(0.01)
        stop()
    elif command == "member":
        if not alive(serving):
            print("Branvil is not reachable", file=sys.stderr)
            sys.exit(1)
        print(json.dumps({"items": [
            {"name": "bot", "kind": "agent"},
            {"name": "keeper", "kind": "owner"},
        ]}, indent=2))
    elif command == "backup":
        if setting("DEV_TEST_BACKUP_EXIT"):
            sys.exit(int(setting("DEV_TEST_BACKUP_EXIT")))
        to = Path(args[args.index("--to") + 1])
        to.mkdir(parents=True)
        (to / "branvil.redb").write_text(database(), encoding="utf-8")
        (to / "manifest.json").write_text("{}", encoding="utf-8")
        print(f"backup written to {to}")
    elif command == "restore":
        data.mkdir(parents=True)
        frm = Path(args[args.index("--from") + 1])
        shutil.copy(frm / "branvil.redb", data / "branvil.redb")
        print(f"restored into {data}")
    elif command == "admin":
        db = data / "branvil.redb"
        # On the copy of the rehearsal, steps and failures have their own names.
        step = ("REHEARSAL_" if data.name == "rehearsal" else "") + args[1].upper()
        hold(step)
        if args[1] == "upgrade":
            # As branvil admin upgrade: 0 left as it is, 3 upgraded, 1 failed.
            if database() == schema:
                print(f"{db} already has schema version {schema}")
            elif int(database()) != int(schema) - 1 or setting(f"DEV_TEST_{step}_EXIT"):
                print(f"branvil: cannot upgrade {db} (schema version {database()})", file=sys.stderr)
                sys.exit(1)
            else:
                print(f"upgraded {db} to schema version {schema}: test")
                print(f"the database as it was: {db.parent}/branvil-schema-{database()}-test.redb")
                db.write_text(schema, encoding="utf-8")
                hold(step + "D")
                sys.exit(3)
        elif args[1] == "check" and setting(f"DEV_TEST_{step}_EXIT"):
            sys.exit(int(setting(f"DEV_TEST_{step}_EXIT")))
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
        for script in ["dev.sh", "clean-stale-target.sh", "lib.sh", "serve.sh", "update.sh"]:
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
        for tool in ["npm", "cargo", "git", "rustc", "sync", "osascript"]:
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
            except (ProcessLookupError, PermissionError):  # only zombies left: EPERM on macOS
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

    def test_install_takes_only_a_clean_checkout_at_its_commit(self):
        for name, extra, error in [
            ("builds/def", {}, "HEAD is abc"),
            ("builds/abc", {"DEV_TEST_STATUS": " M Cargo.toml\n"}, "uncommitted changes"),
        ]:
            with self.subTest(name=name):
                result = subprocess.run(
                    ["bash", str(self.repo / "scripts/dev.sh"), "--install", name],
                    cwd=self.root,
                    env={**self.env, **extra},
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(result.returncode, 1)
                self.assertIn(error, result.stderr)
                self.assertEqual(self.events("cargo"), [], "Refused before building")
                self.assertFalse((self.repo / "builds").exists())

    def test_installing_an_existing_build_does_nothing(self):
        run = lambda: subprocess.run(
            ["bash", str(self.repo / "scripts/dev.sh"), "--install", "builds/abc"],
            cwd=self.root, env=self.env, capture_output=True,
        ).returncode
        self.assertEqual(run(), 0)
        built = len(self.events())
        (self.repo / "builds/abc/kept").touch()
        self.assertEqual(run(), 0)
        self.assertEqual([e["tool"] for e in self.events()[built:]], ["git", "git"],
                         "Only the checkout is read")
        self.assertTrue((self.repo / "builds/abc/kept").exists())

    def test_an_install_that_fails_partway_leaves_no_build(self):
        result = subprocess.run(
            ["bash", str(self.repo / "scripts/dev.sh"), "--install", "builds/abc"],
            cwd=self.root,
            env={**self.env, "DEV_TEST_BUILD_NO_BINARY": "1"},
            capture_output=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.repo / "builds/abc").exists())

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
    except PermissionError:  # a group of zombies only, on macOS
        return pid < 0
    return False


RUNNING = {"checks": [], "checks_expected": True}
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
        self.runs_watcher = proc
        if running:
            self.wait_until(self.service_pid, proc)
        return proc, output

    def control(self, **settings):
        Path(self.env["DEV_TEST_CONTROL"]).write_text(json.dumps(settings), encoding="utf-8")

    def service_pid(self):
        """The pid of the fake branvil serve that has the database open."""
        pids = Path(self.env["DEV_TEST_SERVICE_PIDS"])
        pid = int(pids.read_text(encoding="utf-8")) if pids.exists() else None
        return pid if pid is not None and not gone(pid) else None


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

    def admins(self, what):
        """The builds that ran branvil admin `what` on the database itself."""
        return [e["binary"] for e in self.events("branvil")
                if e["args"][:2] == ["admin", what] and e["args"][-1] == str(self.data)]

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
        """Waits until a round of scripts/update.sh that fetched after
        now has ended, so it saw everything set before: the round after it has
        fetched too. (The rounds run one at a time and start on the ticks of
        scripts/serve.sh, so the time between two of them is the same in
        every round and can be shorter than any polling interval.)"""
        fetches = self.server.fetches + 2
        self.wait_until(lambda: self.server.fetches >= fetches)

    def finished(self, proc, output, count=1):
        self.wait_until(
            lambda: output.read_text(encoding="utf-8").count("Update finished") >= count, proc
        )

    def assert_runs(self, sha, pid=None, timeout=30):
        self.wait_until(lambda: self.current() == sha)
        self.wait_until(
            lambda: self.service_pid() is not None and self.serves()[-1] == self.binary(sha),
            timeout=timeout,
        )
        if pid is not None:
            self.assertEqual(self.service_pid(), pid)

    def restart(self, *args):
        """Stops scripts/serve.sh with TERM and starts it again."""
        self.runs_watcher.terminate()
        self.assertEqual(self.runs_watcher.wait(timeout=30), 143)
        return self.watch(*args, running=False)

    def database(self):
        return (self.data / "branvil.redb").read_text(encoding="utf-8").strip() or "11"

    def kill_watcher(self, proc):
        """SIGKILL to scripts/serve.sh and everything it started."""
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=5)

    BOOT = ["build", "admin upgrade", "serve"]
    UPDATE = ["backup", "build", "restore", "admin upgrade", "admin check", "stopped",
              "admin upgrade", "serve"]

    def test_head_is_installed_and_served_and_started_again_when_it_dies(self):
        proc, output = self.watch("--interval", "3600")
        self.assertEqual(self.current(), self.base)
        self.assertIsNone(self.attempted())
        [build] = self.events("cargo")
        self.assertIn("--release", build["args"])
        self.assertEqual(self.steps(), self.BOOT, "Upgraded before it serves")
        self.assertEqual(self.serves(), [self.binary(self.base)])
        [serve] = [e for e in self.events("branvil") if e["args"][0] == "serve"]
        self.assertEqual(serve["args"][-1], str(self.data))
        first = self.service_pid()
        os.kill(first, signal.SIGKILL)
        self.wait_until(lambda: self.service_pid() not in (None, first), proc)
        self.assertEqual(self.steps(), self.BOOT + ["admin upgrade", "serve"])
        log = output.read_text(encoding="utf-8")
        self.assertIn(f"branvil serve of {self.base} exited with status 137", log)

    def test_a_build_that_keeps_exiting_at_its_start_is_stopped(self):
        proc, output = self.watch("--interval", "3600")
        self.control(DEV_TEST_SERVE_EXIT="1")
        os.kill(self.service_pid(), signal.SIGKILL)
        guard = f"{self.base} stopped while starting (it exited within 10 seconds 3 times)"
        self.wait_until(lambda: any(guard in n for n in self.notifications()), proc)
        self.assertIsNone(self.current())
        [message] = [n for n in self.notifications() if guard in n]
        self.assertIn(message, output.read_text(encoding="utf-8"))
        starts = len(self.serves())
        self.assertLessEqual(starts, 3)
        # A start again installs nothing and starts nothing.
        proc, output = self.restart("--interval", "3600")
        time.sleep(2.5)
        self.assertIsNone(proc.poll())
        self.assertNotIn("starting", output.read_text(encoding="utf-8"))
        self.assertEqual(self.cargo_commands(), ["build"])
        self.control()
        (self.builds / "current").write_text(f"{self.base}\n", encoding="utf-8")
        self.assert_runs(self.base)

    def test_the_crash_count_starts_again_with_another_build(self):
        proc, _ = self.watch("--interval", "3600")
        for _ in range(2):
            pid = self.service_pid()
            os.kill(pid, signal.SIGKILL)
            self.wait_until(lambda: self.service_pid() not in (None, pid), proc)
        shutil.copytree(self.builds / self.base, self.builds / "other")
        self.control(DEV_TEST_SERVE_EXIT_other="1")
        (self.builds / "current").write_text("other\n", encoding="utf-8")
        self.wait_until(lambda: any("other stopped while starting" in n for n in self.notifications()), proc)
        self.assertEqual(self.serves().count(self.binary("other")), 3)

    def test_a_failed_first_install_is_tried_again_at_the_next_start(self):
        self.control(DEV_TEST_BUILD_EXIT="101")
        proc, output = self.watch(running=False)
        self.wait_until(self.notifications, proc)
        self.assertIn("Could not install the checkout's HEAD", self.notifications()[0])
        self.assertFalse(self.builds.exists())
        self.control()
        self.restart("--interval", "3600")
        self.assert_runs(self.base)
        self.assertEqual(self.cargo_commands(), ["build", "build"])

    def test_a_first_install_interrupted_by_term_is_done_again_at_the_next_start(self):
        hold = self.root / "hold"
        hold.touch()
        self.control(DEV_TEST_HOLD_BUILD=str(hold))
        proc, _ = self.watch(running=False)
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        proc.terminate()
        time.sleep(0.2)
        hold.unlink()
        self.assertEqual(proc.wait(timeout=10), 143)
        self.assertFalse(self.builds.exists(), "Not half made")
        self.watch("--interval", "3600")
        self.assertEqual(self.cargo_commands(), ["build", "build"])
        self.assert_runs(self.base)
        self.assertEqual(sorted(p.name for p in self.repo.iterdir() if "builds" in p.name),
                         ["builds"])

    def test_a_checkout_with_uncommitted_changes_is_not_installed_first(self):
        (self.repo / "Cargo.toml").write_text("changed", encoding="utf-8")
        proc, _ = self.watch(running=False)
        self.wait_until(self.notifications, proc)
        self.assertIn("Could not install the checkout's HEAD", self.notifications()[0])
        self.assertEqual(self.cargo_commands(), [])
        self.assertFalse(self.builds.exists())

    def test_naming_another_build_switches_to_it(self):
        self.watch("--interval", "3600")
        first = self.service_pid()
        shutil.copytree(self.builds / self.base, self.builds / "other")
        (self.builds / "current").write_text("other\n", encoding="utf-8")
        self.assert_runs("other")
        self.assertTrue(gone(first))
        self.assertEqual(self.admins("upgrade")[-1], self.binary("other"))

    def test_update_sh_is_not_for_hand_use(self):
        for args in [[], ["--now"], ["--data-dir"]]:
            with self.subTest(args=args):
                result = subprocess.run(["bash", str(self.repo / "scripts/update.sh"), *args],
                                        cwd=self.root, env=self.env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
                self.assertIn("run by scripts/serve.sh only", result.stderr)
        self.assertEqual(self.events(), [])

    def test_nothing_is_done_while_main_is_the_running_build(self):
        self.watch()
        self.rounds_pass()
        self.assertEqual(self.server.check_requests, [])
        self.assertEqual(self.steps(), self.BOOT)

    def test_a_restart_checks_at_once(self):
        self.watch("--interval", "3600")
        new = self.merge(SUCCESS)
        proc, output = self.restart("--interval", "3600")
        self.finished(proc, output)
        self.assert_runs(new)

    def test_unfinished_checks_are_waited_for_then_the_update_runs(self):
        new = self.merge({"checks": [], "checks_expected": True})
        proc, output = self.watch()
        self.rounds_pass()
        self.server.checks[new] = {
            "checks": [{"name": "test", "status": "in_progress", "conclusion": None}],
            "checks_expected": True,
        }
        self.rounds_pass()
        self.assertEqual(self.steps(), self.BOOT, "No update while checks run")
        self.assertIsNone(self.attempted())
        self.server.checks[new] = SUCCESS
        self.finished(proc, output)
        self.assertEqual(self.steps(), self.BOOT + self.UPDATE,
                         "Built and rehearsed before the service stops")
        self.assert_runs(new)
        self.assertEqual(self.serves(), [self.binary(self.base), self.binary(new)])
        self.assertEqual(self.admins("upgrade")[-1], self.binary(new))
        self.assertEqual(self.admins("check"), [], "Nothing changed the database itself")
        backup = next(e for e in self.events("branvil") if e["args"][0] == "backup")
        self.assertEqual(backup["binary"], self.binary(self.base), "Backed up by the old build")
        self.assertFalse(backup["token_present"])
        self.assertFalse(backup["url_present"])
        self.assertEqual(backup["args"][backup["args"].index("--data-dir") + 1], str(self.data))
        [made] = self.backups.iterdir()
        self.assertRegex(made.name, r"^branvil-update-\d{8}T\d{6}Z$")
        restore = next(e for e in self.events("branvil") if e["args"][0] == "restore")
        self.assertEqual(restore["binary"], self.binary(self.base), "Restored by the old build")
        self.assertEqual(restore["args"][restore["args"].index("--from") + 1], str(made))
        self.assertFalse((self.builds / "rehearsal").exists(), "The copy is removed")
        self.assertEqual(self.installed(), sorted([self.base, new]), "The previous build stays")
        self.assertEqual(self.git("branch", "--show-current"), "main")
        self.assertEqual(self.git("rev-parse", "HEAD"), new)
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
        self.finished(proc, output, 2)
        self.assert_runs(newer)
        self.assertEqual(self.installed(), sorted([new, newer]))

    def test_an_upgrade_that_changes_the_database_is_checked_before_it_serves(self):
        proc, output = self.watch()
        new = self.merge(RUNNING)
        self.control(**{f"DEV_TEST_SCHEMA_{new}": "12"})
        self.server.checks[new] = SUCCESS
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(self.database(), "12")
        self.assertEqual(self.steps()[-3:], ["admin upgrade", "admin check", "serve"])
        self.assertEqual(self.admins("check"), [self.binary(new)])
        self.assertIn(f"the database as it was: {self.data}/branvil-schema-11-test.redb",
                      output.read_text(encoding="utf-8"))

    def test_a_check_cut_off_after_the_upgrade_runs_at_the_next_start(self):
        proc, output = self.watch()
        hold = self.root / "hold"
        hold.touch()
        new = self.merge(RUNNING)
        self.control(**{f"DEV_TEST_SCHEMA_{new}": "12", f"DEV_TEST_HOLD_UPGRADED_{new}": str(hold)})
        self.server.checks[new] = SUCCESS
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        self.kill_watcher(proc)
        hold.unlink()
        self.assertEqual(self.database(), "12", "Upgraded before the kill")
        proc, output = self.watch(running=False)
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(self.admins("check"), [self.binary(new)])
        self.assertEqual(self.steps()[-3:], ["admin upgrade", "admin check", "serve"])

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
        self.assertEqual(self.steps(), self.BOOT)
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
        self.assertEqual(self.steps(), self.BOOT)
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

    def check_checkout(self, change):
        """A checkout the update cannot fast-forward to origin/main."""
        new = self.merge(RUNNING)
        if change != "uncommitted":
            self.git("switch", "main")
            if change == "ahead":
                self.git("pull", "--ff-only", str(self.root / "git/acme/demo.git"), "main")
            (self.repo / "LOCAL").write_text("local\n", encoding="utf-8")
            self.git("add", "LOCAL")
            self.git("commit", "-m", "local only")
        head = self.git("rev-parse", "HEAD")
        self.watch("--interval", "3600")
        if change == "uncommitted":
            (self.repo / "Cargo.toml").write_text("changed", encoding="utf-8")
        self.server.checks[new] = SUCCESS
        proc, _ = self.restart("--interval", "3600")
        self.wait_until(self.notifications, proc)
        [message] = self.notifications()
        self.assertIn(f"The update to {'origin/main' if change == 'uncommitted' else new} "
                      "stopped while checking the checkout", message)
        self.assert_runs(head)
        self.assertNotIn("backup", self.steps())
        self.assertIsNone(self.attempted())
        self.assertEqual(self.git("rev-parse", "HEAD"), head)

    def test_a_checkout_with_uncommitted_changes_is_not_updated(self):
        self.check_checkout("uncommitted")

    def test_a_main_ahead_of_origin_main_is_not_updated(self):
        self.check_checkout("ahead")

    def test_a_main_beside_origin_main_is_not_updated(self):
        self.check_checkout("diverged")

    def test_a_failed_build_keeps_the_service_running_and_is_not_tried_again(self):
        proc, output = self.watch()
        pid = self.service_pid()
        self.control(DEV_TEST_BUILD_EXIT="101")
        failing = self.merge(SUCCESS)
        self.wait_until(self.notifications, proc)
        self.rounds_pass()
        self.assertEqual(self.steps(), self.BOOT + ["backup", "build"])
        self.assert_runs(self.base, pid)
        self.assertEqual(self.attempted(), failing)
        [message] = self.notifications()
        self.assertIn(f"The update to {failing} stopped while installing", message)
        self.assertIn(f"Branvil still runs {self.base}", message)
        self.control()
        new = self.merge(SUCCESS)
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(self.installed(), sorted([self.base, new]))

    def test_an_update_killed_alone_is_tried_again(self):
        proc, output = self.watch()
        hold = self.root / "hold"
        hold.touch()
        self.control(DEV_TEST_HOLD_BUILD=str(hold))
        new = self.merge(SUCCESS)
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        builder = self.events("cargo")[-1]["pid"]
        # scripts/update.sh is the parent of scripts/dev.sh, the parent of cargo.
        parent = lambda pid: int(subprocess.run(["ps", "-o", "ppid=", "-p", str(pid)],
                                                capture_output=True, text=True).stdout)
        os.kill(parent(parent(builder)), signal.SIGKILL)
        self.control()
        hold.unlink()
        self.wait_until(lambda: gone(builder))
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(self.steps().count("backup"), 2, "The killed run, then the retry")
        self.assertEqual(self.notifications(), [])

    def test_an_update_cut_off_by_a_kill_goes_on_at_the_next_start(self):
        for failure, runs, attempted in [("", "other", None),
                                         (" checking the data directory", self.base, "other")]:
            with self.subTest(failure=failure):
                # Not a commit to update to: "other" is no commit of origin.
                self.server.checks[self.base] = RUNNING
                self.watch("--interval", "3600")
                self.runs_watcher.terminate()
                self.runs_watcher.wait(timeout=30)
                # As a kill left them: the update recorded, builds/current not
                # yet switched, or its failure recorded but not concluded.
                shutil.rmtree(self.builds / "other", ignore_errors=True)
                shutil.copytree(self.builds / self.base, self.builds / "other")
                (self.builds / "update").write_text(f"other {self.base} -{failure}\n", encoding="utf-8")
                (self.builds / "current").write_text(f"{self.base}\n", encoding="utf-8")
                self.watch("--interval", "3600")
                self.assert_runs(runs)
                self.wait_until(lambda: not (self.builds / "update").exists())
                self.assertEqual(self.attempted(), attempted)
                self.assertEqual(len(self.notifications()), 1 if attempted else 0)
                self.runs_watcher.terminate()
                self.runs_watcher.wait(timeout=30)

    def check_conclusion_cut_off(self, hold_setting):
        proc, _ = self.watch()
        hold = self.root / "hold"
        hold.touch()
        new = self.merge(RUNNING)
        self.control(**{f"DEV_TEST_SERVE_EXIT_{new}": "23", hold_setting: str(hold)})
        self.server.checks[new] = SUCCESS
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        self.kill_watcher(proc)
        hold.unlink()
        self.watch(running=False)
        self.assert_runs(self.base)
        self.wait_until(lambda: not (self.builds / "update").exists())
        self.rounds_pass()
        self.assertEqual(len(self.notifications()), 1)
        self.assertIn(f"Branvil runs {self.base} again", self.notifications()[0])
        self.assertEqual(self.attempted(), new)

    def test_a_conclusion_cut_off_before_it_notifies_notifies_once(self):
        # The previous build's admin upgrade, which decides where it goes back.
        self.check_conclusion_cut_off(f"DEV_TEST_HOLD_UPGRADE_{self.base}")

    def test_a_conclusion_cut_off_after_it_notified_does_not_notify_again(self):
        self.check_conclusion_cut_off("DEV_TEST_HOLD_NOTIFY")

    def test_an_update_that_changes_serve_sh_runs_the_new_one(self):
        proc, output = self.watch()
        script = self.upstream / "scripts/serve.sh"
        text = script.read_text(encoding="utf-8")
        marker = '  cd "$repo_root"\n'
        self.assertEqual(text.count(marker), 1)
        script.write_text(text.replace(marker, marker + '  log "this is the new scripts/serve.sh"\n'),
                          encoding="utf-8")
        self.git("add", "scripts/serve.sh", cwd=self.upstream)
        new = self.merge(SUCCESS)
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertIn("this is the new scripts/serve.sh", output.read_text(encoding="utf-8"))

    def test_every_state_file_is_written_whole_and_on_the_disk(self):
        lib = (self.repo / "scripts/lib.sh").read_text(encoding="utf-8")
        put = lib[lib.index("put() {"):]
        self.assertLess(put.index("sync"), put.index("mv -f"), "On the disk before the rename")
        for script in ["serve.sh", "update.sh"]:
            text = (self.repo / "scripts" / script).read_text(encoding="utf-8")
            # (builds/serve.out is the serve's standard output, no state.)
            self.assertNotRegex(text, r'>\s*"\$builds/(?!serve\.out)', f"{script} writes under builds/ itself")
        self.watch("--interval", "3600")
        self.assertGreaterEqual(len(self.events("sync")), 1)

    def test_a_failed_backup_is_tried_again_and_notified_once(self):
        proc, output = self.watch()
        self.control(DEV_TEST_BACKUP_EXIT="1")
        new = self.merge(SUCCESS)
        self.wait_until(self.notifications, proc)
        self.rounds_pass()
        self.assertGreaterEqual(self.steps().count("backup"), 2, "Tried again")
        [message] = self.notifications()
        self.assertIn(f"The update to {new} stopped while backing up", message)
        self.assertIsNone(self.attempted())
        self.control()
        self.finished(proc, output)
        self.assert_runs(new)

    def test_a_new_build_slower_to_start_than_a_minute_is_waited_for(self):
        proc, output = self.watch()
        hold = self.root / "hold"
        hold.touch()
        new = self.merge(RUNNING)
        self.control(**{f"DEV_TEST_HOLD_SERVE_{new}": str(hold)})
        self.server.checks[new] = SUCCESS
        self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
        time.sleep(65)
        self.assertNotIn("Update finished", output.read_text(encoding="utf-8"))
        self.assertIsNone(self.attempted())
        hold.unlink()
        self.finished(proc, output)
        self.assert_runs(new)
        self.assertEqual(self.notifications(), [])

    def test_a_serve_that_ignores_term_is_killed(self):
        proc, output = self.watch("--interval", "3600")
        first = self.service_pid()
        shutil.copytree(self.builds / self.base, self.builds / "other")
        self.control(**{f"DEV_TEST_SERVE_IGNORE_TERM_{self.base}": "1"})
        # Started again with the setting.
        os.kill(first, signal.SIGKILL)
        self.wait_until(lambda: self.service_pid() not in (None, first), proc)
        stubborn = self.service_pid()
        (self.builds / "current").write_text("other\n", encoding="utf-8")
        self.assert_runs("other", timeout=60)  # after the 30 seconds it gives TERM
        self.assertTrue(gone(stubborn))
        self.assertIn("did not stop within 30 seconds of TERM; killing it",
                      output.read_text(encoding="utf-8"))

    def test_leftovers_are_removed_at_the_start(self):
        self.watch("--interval", "3600")
        leftovers = [self.builds / "rehearsal", self.builds / f"{self.base}.new"]
        for path in leftovers:
            path.mkdir()
        files = [self.builds / "current.123.tmp", self.data / "branvil-schema-10-x.redb.partial"]
        for path in files:
            path.write_text("partial", encoding="utf-8")
        self.restart("--interval", "3600")
        self.assert_runs(self.base)
        for path in leftovers + files:
            self.assertFalse(path.exists(), path)
        self.assertEqual(self.installed(), [self.base])

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
        self.wait_until(lambda: gone(-proc.pid))  # nothing of it is left
        self.assertEqual(self.current(), self.base)
        self.assertIsNone(self.attempted(), "Tried again at the next start")
        self.assertEqual(self.notifications(), [])
        self.assertIn("stopped while installing", output.read_text(encoding="utf-8"))

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
        new = self.merge(SUCCESS)
        proc, output = self.watch()
        self.finished(proc, output)
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

    def check_outcome(self, stage, upgraded, how):
        """An update (with a new build of the database's version, or of the
        next one: `upgraded`) that fails at `stage`, or whose scripts/serve.sh
        gets a TERM there, or is killed with all it started, and is started
        again."""
        proc, output = self.watch()
        pid = self.service_pid()
        hold = self.root / "hold"
        new = self.merge(RUNNING)
        settings = {f"DEV_TEST_SCHEMA_{new}": "12"} if upgraded else {}
        if how == "fails":
            settings.update({
                "install": {"DEV_TEST_BUILD_EXIT": "101"},
                "rehearsal upgrade": {f"DEV_TEST_REHEARSAL_UPGRADE_EXIT_{new}": "1"},
                "rehearsal check": {f"DEV_TEST_REHEARSAL_CHECK_EXIT_{new}": "1"},
                "upgrade": {f"DEV_TEST_UPGRADE_EXIT_{new}": "1"},
                "check": {f"DEV_TEST_CHECK_EXIT_{new}": "1"},
                "start": {f"DEV_TEST_SERVE_EXIT_{new}": "23"},
            }[stage])
        else:
            hold.touch()
            step = {"install": "BUILD", "rehearsal": "REHEARSAL_UPGRADE", "upgrade": "UPGRADE",
                    "upgraded": "UPGRADED", "check": "CHECK", "start": "SERVE"}[stage]
            settings[f"DEV_TEST_HOLD_{step}" + ("" if stage == "install" else f"_{new}")] = str(hold)
        self.control(**settings)
        self.server.checks[new] = SUCCESS
        if how != "fails":
            self.wait_until(lambda: Path(str(hold) + ".held").exists(), proc)
            if how == "term":
                proc.terminate()
                time.sleep(0.2)
                hold.unlink()
                self.assertEqual(proc.wait(timeout=30), 143)
            else:
                self.kill_watcher(proc)
                hold.unlink()
            # Nothing was concluded: the start again finishes the update.
            proc, output = self.watch(running=False)
            self.finished(proc, output)
            self.assert_runs(new)
            self.assertIsNone(self.attempted())
            self.assertEqual(self.notifications(), [])
            self.assertEqual(self.database(), "12" if upgraded else "11")
            return
        self.wait_until(self.notifications, proc)
        self.assertEqual(self.attempted(), new)
        [message] = self.notifications()
        self.assertIn(message, output.read_text(encoding="utf-8"))
        if stage in ("install", "rehearsal upgrade", "rehearsal check"):
            self.assertIn(f"Branvil still runs {self.base}", message)
            self.assert_runs(self.base, pid)
            self.assertNotIn("stopped", self.steps(), "Never stopped")
        elif stage == "check" or (stage == "start" and upgraded):
            self.assertIn("after the data directory was upgraded; Branvil is stopped", message)
            self.assertIsNone(self.current())
            time.sleep(1.5)
            self.assertIsNone(self.service_pid(), "Stopped")
        else:
            self.assertIn(f"Branvil runs {self.base} again", message)
            self.assert_runs(self.base)
            self.assertEqual(self.database(), "11")


# How an update ends at each stage, when that stage fails, or when
# scripts/serve.sh gets a TERM there or is killed with all it started (and is
# started again), with a new build of the database's version or of the next
# one: (stage, upgraded, how).
OUTCOMES = [
    ("install", False, "fails"),
    ("rehearsal upgrade", True, "fails"),
    ("rehearsal check", False, "fails"),
    ("upgrade", True, "fails"),  # an upgrade fails only when there is one to do
    ("check", True, "fails"),  # the database itself is checked only when upgraded
    ("start", False, "fails"),
    ("start", True, "fails"),
] + [
    (stage, upgraded, how)
    for how in ["term", "killed"]
    for stage, upgraded in [
        ("install", False), ("rehearsal", False), ("upgrade", False), ("upgrade", True),
        ("check", True), ("start", False), ("start", True),
    ]
] + [("upgraded", True, "killed")]
for _row in OUTCOMES:
    setattr(
        ServeScriptTests,
        "test_outcome_{}_{}_{}".format(_row[0].replace(" ", "_"),
                                       "upgraded" if _row[1] else "same_version", _row[2]),
        lambda self, row=_row: self.check_outcome(*row),
    )


if __name__ == "__main__":
    unittest.main()
