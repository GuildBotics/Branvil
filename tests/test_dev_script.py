"""Exercise the development launcher without a build, browser, or network."""

import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
HELPER = r'''
import json
import os
from pathlib import Path
import signal
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
with open(os.environ["DEV_TEST_EVENTS"], "a", encoding="utf-8") as out:
    out.write(json.dumps(event) + "\n")
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
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
        print("branvil listening on http://127.0.0.1:7878 (base URL http://127.0.0.1:7878)", flush=True)
        while not Path(os.environ["DEV_TEST_RELEASE"]).exists():
            time.sleep(0.01)
    elif command == "open":
        if "--print" in args:
            print("http://example.invalid/-/enter#test-only-link")
            sys.exit(int(os.environ.get("DEV_TEST_PRINT_EXIT", "0")))
        sys.exit(int(os.environ.get("DEV_TEST_BROWSER_EXIT", "0")))
'''


@unittest.skipUnless(os.name == "posix", "The launcher requires a POSIX shell")
class DevScriptTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="branvil-dev-test-")
        self.root = Path(self.temp.name)
        self.repo = self.root / "checkout with spaces"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "web").mkdir()
        shutil.copyfile(ROOT / "scripts/dev.sh", self.repo / "scripts/dev.sh")
        self.lock = self.repo / "web/package-lock.json"
        self.lock.write_text("{}", encoding="utf-8")
        os.utime(self.lock, (1000, 1000))
        self.tools = self.root / "tools"
        self.tools.mkdir()
        helper = self.root / "helper.py"
        helper.write_text(HELPER, encoding="utf-8")
        binary = self.repo / "target/debug/branvil"
        binary.parent.mkdir(parents=True)
        for path, tool in [
            (self.tools / "npm", "npm"),
            (self.tools / "cargo", "cargo"),
            (self.tools / "git", "git"),
            (binary, "branvil"),
        ]:
            path.write_text(
                "#!/bin/sh\nexec "
                + shlex.join([sys.executable, str(helper), tool])
                + ' "$@"\n',
                encoding="utf-8",
            )
            path.chmod(0o755)
        self.log = self.root / "events.jsonl"
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
                ["bash", str(self.repo / "scripts/dev.sh"), *args],
                cwd=self.root,
                env={
                    **os.environ,
                    "PATH": str(self.tools) + os.pathsep + os.environ["PATH"],
                    "BRANVIL_DATA_DIR": str(self.repo / "data with spaces"),
                    "BRANVIL_TOKEN": "test-unrelated-credential",
                    "BRANVIL_URL": "http://example.invalid/unrelated",
                    "DEV_TEST_EVENTS": str(self.log),
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
            lambda: "Branvil is running." in output.read_text(encoding="utf-8"),
        )
        return proc, output, release

    def finish(self, run):
        proc, _, release = run
        release.touch()
        self.assertEqual(proc.wait(timeout=5), 0)

    def opens(self):
        return [event for event in self.events("branvil") if event["args"][0] == "open"]

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


if __name__ == "__main__":
    unittest.main()
