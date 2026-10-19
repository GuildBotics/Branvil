"""A pytest plugin that runs GuildBotics' port contracts for the ``branvil``
provider against a real ``branvil serve`` instead of the Python double.

GuildBotics' ``tests/guildbotics/integrations/contracts/`` run the same steps
against every provider of its factory; for ``branvil`` its harness
(``providers._branvil``) answers from ``BranvilDouble``. With ``BRANVIL_LIVE=1``
this plugin replaces that harness, for every test, with one that starts a
real service:

- ``branvil init`` and ``branvil serve`` (``BRANVIL_BIN``) on a free loopback
  port, with the data directory in the test's temporary directory;
- the owner ``alice`` (``branvil init``; its token from the data directory's
  ``secrets/owner.token``), the agent ``aiko``, and a CI runner member;
- the workspace ``acme`` (the scope, whose board the tickets are taken from)
  with repository ``acme/demo`` (label ``bug``) and a seeded ``main`` that
  holds a pipeline (``.branvil/ci.toml``, one job ``test`` that declares the
  artifact ``report``); outside the scope the workspace ``other`` with
  ``other/demo`` holding one ticket with one comment by ``alice``.

The harness operations work on the real service: ``branch``/``advance``/
``delete_branch`` push with a real Git client (Smart HTTP, the member's token
through ``GIT_ASKPASS``); ``assign`` assigns ``aiko`` and puts the card in the
ready lane through the API; ``artifact`` uploads through the runner API as a
fake runner (claim the commit's attempt, or retry it, upload, finish).

CI: because ``main`` has a pipeline, every pull request head gets a run whose
job is queued. A fake runner concludes every queued attempt ``success`` right
before the provider reads a pull request's checks (a hook in the provider's
transport), so readiness reflects CI that ran and passed; nothing is ever
reported as success without an attempt of that exact commit finishing.

``writes`` lists the write requests that reached the service and that it
accepted (a non-GET answered 2xx), as ``"<method> <path>"``, the way the
double counts them: a write refused by the provider's scope gate never
reaches the service, and one the service refuses (``409 head_moved``,
``422 unsupported``, ``404``) changes nothing there.

Client-side Git never reads the user's configuration: ``GIT_CONFIG_NOSYSTEM``,
an empty ``GIT_CONFIG_GLOBAL``, ``GIT_TERMINAL_PROMPT=0``, ``credential.helper``
cleared, a ``HOME`` of its own, credentials only from environment variables
read by an askpass script.
"""

from __future__ import annotations

import logging
import os
import re
import shutil
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any
from urllib.parse import quote

import httpx
import pytest

LIVE = os.environ.get("BRANVIL_LIVE") == "1"

OWNER = "alice"
MEMBER = "aiko"
RUNNER = "runner1"
SCOPE = "acme"
REPO = f"{SCOPE}/demo"
THEIRS = "other"
THEIR_REPO = f"{THEIRS}/demo"
LABEL = "bug"
IMAGE = "docker.io/library/alpine:3.22"
PIPELINE = f"""[[jobs]]
name = "test"
image = "{IMAGE}"
commands = ["true"]
artifacts = [{{ name = "report", paths = ["out/"] }}]
"""
_CHECKS = re.compile(r"/api/v1/repos/[^/]+/pulls/\d+/checks")
_ASKPASS = """#!/bin/sh
case "$1" in
  Username*) printf '%s\\n' "$BRANVIL_GIT_USER" ;;
  *) printf '%s\\n' "$BRANVIL_GIT_TOKEN" ;;
esac
"""

#: The services started by the current test, stopped at its teardown.
_RUNNING: list[Service] = []
#: How many contract tests ran and were skipped.
_OUTCOMES = {"passed": 0, "skipped": 0}


def seg(path: str) -> str:
    return quote(path, safe="")


class Service:
    """A real ``branvil serve`` with its data directory under ``root``."""

    def __init__(self, root: Path) -> None:
        binary = os.environ.get("BRANVIL_BIN") or shutil.which("branvil")
        assert binary, "set BRANVIL_BIN to the branvil binary"
        self.root = root
        root.mkdir(parents=True, exist_ok=True)
        env = {
            "PATH": os.environ["PATH"],
            "HOME": str(root),
            "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
            "BRANVIL_LOG": "branvil=debug",
        }
        data = root / "data"
        subprocess.run(
            [
                binary,
                "init",
                "--owner",
                OWNER,
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
                str(data),
            ],
            env=env,
            capture_output=True,
            text=True,
            check=True,
        )
        self.owner = (data / "secrets" / "owner.token").read_text("utf-8").strip()
        self.log = root / "serve.log"
        self._process = subprocess.Popen(
            [binary, "serve", "--data-dir", str(data)],
            env=env,
            stdout=subprocess.PIPE,
            stderr=self.log.open("ab"),
            stdin=subprocess.DEVNULL,
            text=True,
        )
        assert self._process.stdout is not None
        first = self._process.stdout.readline()
        found = re.match(r"branvil listening on (\S+)", first)
        assert found, f"unexpected first line {first!r}; see {self.log}"
        self.url = found.group(1)
        self.http = httpx.Client(
            base_url=f"{self.url}/api/v1", timeout=30, trust_env=False
        )
        _RUNNING.append(self)

    def stop(self) -> None:
        self.http.close()
        if self._process.poll() is None:
            self._process.terminate()
            try:
                self._process.wait(10)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait()

    def call(
        self,
        method: str,
        path: str,
        token: str,
        body: Any = None,
        *,
        expect: tuple[int, ...] = (200, 201, 204),
        **kwargs: Any,
    ) -> httpx.Response:
        response = self.http.request(
            method,
            path,
            json=body,
            headers={"Authorization": f"Bearer {token}", **kwargs.pop("headers", {})},
            **kwargs,
        )
        assert response.status_code in expect, (
            f"{method} {path}: {response.status_code} {response.text}"
        )
        return response

    def json(self, method: str, path: str, token: str, body: Any = None) -> Any:
        response = self.call(method, path, token, body)
        return response.json() if response.content else None

    def member(self, name: str, kind: str) -> str:
        """Create a member; returns a new token of it."""
        self.json("POST", "/members", self.owner, {"name": name, "kind": kind})
        issued = self.json(
            "POST", f"/members/{name}/tokens", self.owner, {"label": "t"}
        )
        return str(issued["token"])


class Git:
    """A working copy of ``REPO`` whose pushes are a member's."""

    def __init__(self, service: Service, root: Path) -> None:
        self.service = service
        self.home = root / "git-home"
        self.home.mkdir(parents=True)
        (self.home / "gitconfig").write_text("", encoding="utf-8")
        self.askpass = self.home / "askpass.sh"
        self.askpass.write_text(_ASKPASS, encoding="utf-8")
        self.askpass.chmod(0o700)
        self.work = root / "work"
        self.remote = f"{service.url}/git/{REPO}.git"
        self._revision = 0

    def run(self, *args: str, user: str, token: str) -> str:
        env = {
            "PATH": os.environ["PATH"],
            "HOME": str(self.home),
            "LANG": "C",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": str(self.home / "gitconfig"),
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_ASKPASS": str(self.askpass),
            "BRANVIL_GIT_USER": user,
            "BRANVIL_GIT_TOKEN": token,
            "GIT_AUTHOR_NAME": user,
            "GIT_AUTHOR_EMAIL": f"{user}@example.invalid",
            "GIT_COMMITTER_NAME": user,
            "GIT_COMMITTER_EMAIL": f"{user}@example.invalid",
        }
        done = subprocess.run(
            ["git", "-c", "credential.helper=", "-c", "core.askPass=", *args],
            cwd=self.work if self.work.exists() else self.home,
            env=env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
        )
        assert done.returncode == 0, f"git {' '.join(args)}: {done.stderr}"
        return done.stdout.strip()

    def commit_on(self, start: str, branch: str, user: str, token: str) -> str:
        """Commit a change of ``a.py`` (the file the contracts comment on)
        on ``start`` and push it as ``branch``."""
        self.run("checkout", "-q", "--detach", start, user=user, token=token)
        self._revision += 1
        lines = [f"# {branch} {self._revision}"] + [f"x{n} = {n}" for n in range(1, 10)]
        (self.work / "a.py").write_text("\n".join(lines) + "\n", encoding="utf-8")
        self.run("add", "a.py", user=user, token=token)
        self.run(
            "commit", "-q", "-m", f"{branch} {self._revision}", user=user, token=token
        )
        sha = self.run("rev-parse", "HEAD", user=user, token=token)
        self.run(
            "push",
            "-q",
            self.remote,
            f"{sha}:refs/heads/{branch}",
            user=user,
            token=token,
        )
        return sha


class FakeRunner:
    """The CI runner, as far as the contracts need one: it concludes queued
    attempts ``success`` and uploads the artifacts it was asked to keep."""

    def __init__(self, service: Service, token: str) -> None:
        self.service = service
        self.token = token
        #: Artifacts to upload when an attempt of the commit runs.
        self.pending: dict[str, dict[str, bytes]] = {}
        #: The commits whose attempts ran, newest last.
        self.ran: list[str] = []

    def drain(self) -> None:
        while True:
            claimed = self.service.call(
                "POST",
                "/runner/claim",
                self.token,
                {"executor": "microsandbox", "executor_state": "ready"},
            )
            if claimed.status_code == httpx.codes.NO_CONTENT:
                return
            attempt = claimed.json()
            lease = {"X-Branvil-Lease": attempt["lease"]}
            declared = {a["name"] for a in attempt["job"]["artifacts"]}
            for name, content in self.pending.pop(attempt["sha"], {}).items():
                assert name in declared, f"the pipeline declares no artifact {name}"
                self.service.call(
                    "PUT",
                    f"/runner/attempts/{attempt['attempt_id']}/artifacts/{name}",
                    self.token,
                    content=content,
                    headers=lease,
                )
            self.service.json(
                "POST",
                f"/runner/attempts/{attempt['attempt_id']}/finish",
                self.token,
                {"lease": attempt["lease"], "conclusion": "success"},
            )
            self.ran.append(attempt["sha"])

    def keep(self, sha: str, name: str, content: bytes, owner: str) -> None:
        """Keep ``content`` as the artifact ``name`` of ``sha``: by the commit's
        queued attempt, or by a retry of its latest one."""
        self.pending.setdefault(sha, {})[name] = content
        self.drain()
        if sha not in self.pending:
            return
        for pull in self.service.json(
            "GET", f"/repos/{seg(REPO)}/pulls?state=open&limit=100", owner
        )["items"]:
            if pull["head_sha"] != sha:
                continue
            checks = self.service.json(
                "GET", f"/repos/{seg(REPO)}/pulls/{pull['number']}/checks", owner
            )
            for check in checks["checks"]:
                if check.get("attempt_id") and check["status"] == "completed":
                    self.service.json(
                        "POST",
                        f"/repos/{seg(REPO)}/jobs/{check['attempt_id']}/retry",
                        owner,
                    )
            break
        self.drain()
        assert sha not in self.pending, f"no CI attempt of {sha} ran"


class _Counting(httpx.AsyncBaseTransport):
    """The provider's transport: lists the writes the service accepted, and
    lets the fake runner finish queued CI before checks are read."""

    def __init__(
        self,
        inner: httpx.AsyncBaseTransport,
        writes: list[str],
        before_checks: Callable[[], None],
    ) -> None:
        self.inner = inner
        self.writes = writes
        self.before_checks = before_checks

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        path = request.url.raw_path.decode("ascii").partition("?")[0]
        if request.method == "GET" and _CHECKS.fullmatch(path):
            self.before_checks()
        response = await self.inner.handle_async_request(request)
        if request.method != "GET" and response.is_success:
            self.writes.append(f"{request.method} {path}")
        return response

    async def aclose(self) -> None:
        await self.inner.aclose()


def live_harness(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Any:
    from guildbotics.entities.task import Task
    from guildbotics.entities.team import Person, Project, Team
    from guildbotics.integrations.branvil import api
    from guildbotics.integrations.factory import ServiceIntegrationFactory
    from tests.guildbotics.integrations.contracts.providers import Harness

    service = Service(tmp_path / "branvil")
    owner = service.owner
    aiko = service.member(MEMBER, "agent")
    runner = FakeRunner(service, service.member(RUNNER, "runner"))
    for workspace in (SCOPE, THEIRS):
        service.json("POST", "/workspaces", owner, {"name": workspace})
    service.json("POST", "/repos", owner, {"path": REPO, "labels": [LABEL]})
    service.json("POST", "/repos", owner, {"path": THEIR_REPO})
    theirs = service.json(
        "POST",
        f"/repos/{seg(THEIR_REPO)}/tickets",
        owner,
        {"title": "Theirs", "body": "", "labels": []},
    )
    their_comment = service.json(
        "POST",
        f"/repos/{seg(THEIR_REPO)}/tickets/{theirs['key']}/comments",
        owner,
        {"body": "Hi"},
    )["id"]

    git = Git(service, tmp_path / "git")
    git.run("init", "-q", "-b", "main", str(git.work), user=OWNER, token=owner)
    (git.work / "README.md").write_text("seed\n", encoding="utf-8")
    (git.work / ".branvil").mkdir()
    (git.work / ".branvil" / "ci.toml").write_text(PIPELINE, encoding="utf-8")
    git.run("add", ".", user=OWNER, token=owner)
    git.run("commit", "-q", "-m", "seed", user=OWNER, token=owner)
    git.run("push", "-q", git.remote, "HEAD:refs/heads/main", user=OWNER, token=owner)
    tips = {"main": git.run("rev-parse", "HEAD", user=OWNER, token=owner)}

    def branch(name: str) -> None:
        tips[name] = git.commit_on(tips["main"], name, MEMBER, aiko)

    def advance(name: str) -> None:
        tips[name] = git.commit_on(tips[name], name, MEMBER, aiko)

    def delete_branch(name: str) -> None:
        git.run(
            "push", "-q", git.remote, f":refs/heads/{name}", user=MEMBER, token=aiko
        )
        del tips[name]

    def assign(url: str) -> None:
        repo, _, number = url.removeprefix(f"{service.url}/").rpartition("/-/tickets/")
        ticket = f"/repos/{seg(repo)}/tickets/{number}"
        service.json("PUT", f"{ticket}/assignee", owner, {"name": MEMBER})
        service.json("PUT", f"{ticket}/board", owner, {"lane": "ready"})

    writes: list[str] = []
    connect = api.connect

    def connected(*args: Any, **kwargs: Any) -> httpx.AsyncClient:
        client = connect(*args, **kwargs)
        client._transport = _Counting(client._transport, writes, runner.drain)
        return client

    monkeypatch.setattr(api, "connect", connected)
    monkeypatch.setenv(f"{MEMBER.upper()}_BRANVIL_TOKEN", aiko)
    person = Person(
        person_id=MEMBER,
        name="Aiko",
        person_type="agent",
        account_info={"branvil_username": MEMBER},
    )
    config = {"name": "branvil", "workspace": SCOPE, "base_url": service.url}
    team = Team(
        project=Project(
            name="demo",
            services={
                "code_hosting_service": config,
                "ticket_service": config,
            },
        ),
        members=[person],
    )
    factory = ServiceIntegrationFactory()
    logger = logging.getLogger(__name__)
    return Harness(
        name="branvil",
        code=factory.create_code_hosting_service(logger, person, team),
        tickets=factory.create_ticket_service(logger, person, team),
        repository=REPO,
        tracker=REPO,
        elsewhere=THEIR_REPO,
        their_tracker=THEIR_REPO,
        branch=branch,
        advance=advance,
        delete_branch=delete_branch,
        assign=assign,
        artifact=lambda sha, name, content: runner.keep(sha, name, content, owner),
        theirs=Task(
            id=theirs["url"],
            url=theirs["url"],
            title="Theirs",
            description="",
            status=Task.READY,
        ),
        their_comment=their_comment,
        missing_ticket=f"{service.url}/{REPO}/-/tickets/404",
        features=frozenset({"request_changes", "artifacts"}),
        writes=writes,
    )


# -- pytest hooks ----------------------------------------------------------------


@pytest.fixture(autouse=True)
def _branvil_live(monkeypatch: pytest.MonkeyPatch):
    if not LIVE:
        yield
        return
    from tests.guildbotics.integrations.contracts import providers

    monkeypatch.setattr(providers, "_branvil", live_harness)
    try:
        yield
    finally:
        while _RUNNING:
            _RUNNING.pop().stop()


def pytest_runtest_logreport(report: pytest.TestReport) -> None:
    if not LIVE:
        return
    if report.skipped:
        _OUTCOMES["skipped"] += 1
    elif report.when == "call" and report.passed:
        _OUTCOMES["passed"] += 1


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    if LIVE and exitstatus == 0 and (_OUTCOMES["skipped"] or not _OUTCOMES["passed"]):
        # A contract that did not run against the service is not a pass.
        session.exitstatus = pytest.ExitCode.TESTS_FAILED


def pytest_terminal_summary(terminalreporter: Any) -> None:
    if LIVE:
        terminalreporter.write_line(
            f"branvil live: {_OUTCOMES['passed']} passed against a real branvil serve, "
            f"{_OUTCOMES['skipped']} skipped"
        )
