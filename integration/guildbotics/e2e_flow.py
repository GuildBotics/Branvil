"""The end-to-end flow of a ticket through Branvil, with GuildBotics'
``branvil`` provider as the agent's side, the real CI runner (microVM
executor), and the real GUI for the owner, in a repository without an
upstream (pull requests are merged in Branvil; publishing to an upstream host
is covered by ``tests/upstream.rs``):

1. the owner creates the workspace ``acme`` and a ticket of ``acme/demo``,
   assigns the agent ``aiko``, and puts it in the ready lane of the
   workspace's board;
2. the provider's ``candidates()`` returns it (``ready_lane``) and ``move()``
   puts it in progress;
3. ``aiko`` pushes ``ticket/1`` with Git (token through ``GIT_ASKPASS``)
   and the provider opens the pull request that closes the ticket;
4. the required check ``test`` runs in a microVM (``[ci] runner = "auto"``);
   the provider's ``readiness()`` goes from pending to ready;
5. the owner enters the GUI with ``branvil open --print``, reviews the check,
   its log, and the diff, and merges (Playwright, ``web/e2e/flow.spec.ts``);
6. the ticket is closed and in the done lane; ``closed_since()`` lists the
   ticket and the merged pull request, and ``main`` moved to the head.

Run it through ``scripts/e2e-flow.sh``. It prints one ``FLOW`` line per step
with its duration, and a JSON summary at the end. Everything lives in a
temporary directory that is removed afterwards (``--keep`` keeps it; the only
token in it is the owner's, in the data directory's ``secrets/owner.token``).
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any
from urllib.parse import quote

import httpx

IMAGE = "docker.io/library/alpine:3.22"
PIPELINE = f"""[[jobs]]
name = "test"
image = "{IMAGE}"
commands = ["sh scripts/test.sh"]
timeout_secs = 120
"""
TEST_SH = """set -e
if [ -f greeting.txt ]; then grep -qx 'hello' greeting.txt; fi
echo "tests passed on $BRANVIL_SHA"
"""
LOG_LINE = "tests passed on"
ASKPASS = """#!/bin/sh
case "$1" in
  Username*) printf '%s\\n' "$BRANVIL_GIT_USER" ;;
  *) printf '%s\\n' "$BRANVIL_GIT_TOKEN" ;;
esac
"""
SCOPE, REPO = "acme", "acme/demo"
BRANCH = "ticket/1"


def seg(path: str) -> str:
    return quote(path, safe="")


class Flow:
    def __init__(self, root: Path, binary: str, web: Path) -> None:
        self.root, self.binary, self.web = root, binary, web
        self.started = time.monotonic()
        self.last = self.started
        self.steps: list[dict[str, Any]] = []
        self.process: subprocess.Popen[str] | None = None

    def step(self, name: str, **facts: Any) -> None:
        now = time.monotonic()
        entry = {"step": name, "ms": round((now - self.last) * 1000), **facts}
        self.steps.append(entry)
        print(
            f"FLOW {name}: {entry['ms']} ms {json.dumps(facts) if facts else ''}",
            flush=True,
        )
        self.last = now

    # -- the service ---------------------------------------------------------

    def start(self) -> None:
        data = self.root / "data"
        env = self._env()
        subprocess.run(
            [
                self.binary,
                "init",
                "--owner",
                "alice",
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
        self.data = data
        self.owner = (data / "secrets" / "owner.token").read_text("utf-8").strip()
        config = data / "branvil.toml"
        text = config.read_text(encoding="utf-8")
        text = text.replace('runner = "off"', 'runner = "auto"', 1)
        text = text.replace(
            "[ci.executor]\n",
            f'[ci.executor]\nwork_dir = "{self.root / "runner"}"\n',
            1,
        )
        config.write_text(text, encoding="utf-8")
        self.log = self.root / "serve.log"
        self.process = subprocess.Popen(
            [self.binary, "serve", "--data-dir", str(data)],
            env=env,
            stdout=subprocess.PIPE,
            stderr=self.log.open("ab"),
            stdin=subprocess.DEVNULL,
            text=True,
        )
        first = self.process.stdout.readline()
        self.url = re.match(r"branvil listening on (\S+)", first).group(1)
        self.http = httpx.Client(
            base_url=f"{self.url}/api/v1", timeout=30, trust_env=False
        )

    def stop(self) -> None:
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(15)
            except subprocess.TimeoutExpired:
                self.process.kill()

    def _env(self) -> dict[str, str]:
        return {
            "PATH": os.environ["PATH"],
            "HOME": os.environ[
                "BRANVIL_FLOW_REAL_HOME"
            ],  # the runner finds msb and its image cache
            "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
            "BRANVIL_LOG": "branvil=debug",
        }

    def api(
        self,
        method: str,
        path: str,
        token: str,
        body: Any = None,
        expect=(200, 201, 204),
    ) -> Any:
        r = self.http.request(
            method, path, json=body, headers={"Authorization": f"Bearer {token}"}
        )
        assert r.status_code in expect, f"{method} {path}: {r.status_code} {r.text}"
        return r.json() if r.content else None

    def member(self, name: str, kind: str) -> str:
        self.api("POST", "/members", self.owner, {"name": name, "kind": kind})
        return self.api(
            "POST", f"/members/{name}/tokens", self.owner, {"label": "flow"}
        )["token"]

    # -- Git -------------------------------------------------------------------

    def git(self, *args: str, user: str, token: str, cwd: Path | None = None) -> str:
        home = self.root / "git-home"
        if not home.exists():
            home.mkdir()
            (home / "gitconfig").write_text("", encoding="utf-8")
            (home / "askpass.sh").write_text(ASKPASS, encoding="utf-8")
            (home / "askpass.sh").chmod(0o700)
        env = {
            "PATH": os.environ["PATH"],
            "HOME": str(home),
            "LANG": "C",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": str(home / "gitconfig"),
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_ASKPASS": str(home / "askpass.sh"),
            "BRANVIL_GIT_USER": user,
            "BRANVIL_GIT_TOKEN": token,
            "GIT_AUTHOR_NAME": user,
            "GIT_AUTHOR_EMAIL": f"{user}@example.invalid",
            "GIT_COMMITTER_NAME": user,
            "GIT_COMMITTER_EMAIL": f"{user}@example.invalid",
        }
        done = subprocess.run(
            ["git", "-c", "credential.helper=", "-c", "core.askPass=", *args],
            cwd=cwd or self.root,
            env=env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
        )
        assert done.returncode == 0, f"git {' '.join(args)}: {done.stderr}"
        return done.stdout.strip()


def provider(flow: Flow, token: str) -> tuple[Any, Any]:
    from guildbotics.entities.team import Person, Project, Team
    from guildbotics.integrations.factory import ServiceIntegrationFactory

    person = Person(
        person_id="aiko",
        name="Aiko",
        person_type="agent",
        account_info={"branvil_username": "aiko"},
    )
    os.environ[person.to_person_env_key("BRANVIL_TOKEN")] = token
    config = {"name": "branvil", "workspace": SCOPE, "base_url": flow.url}
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
    logger = logging.getLogger("flow")
    return (
        factory.create_code_hosting_service(logger, person, team),
        factory.create_ticket_service(logger, person, team),
    )


async def run(flow: Flow) -> dict[str, Any]:
    flow.start()
    flow.step("service-started", url=flow.url)
    aiko = flow.member("aiko", "agent")
    flow.api("POST", "/workspaces", flow.owner, {"name": SCOPE})
    flow.api("POST", "/repos", flow.owner, {"path": REPO, "labels": ["bug"]})
    seed = flow.root / "seed"
    flow.git("init", "-q", "-b", "main", str(seed), user="alice", token=flow.owner)
    (seed / "README.md").write_text("# demo\n", encoding="utf-8")
    (seed / ".branvil").mkdir()
    (seed / ".branvil" / "ci.toml").write_text(PIPELINE, encoding="utf-8")
    (seed / "scripts").mkdir()
    (seed / "scripts" / "test.sh").write_text(TEST_SH, encoding="utf-8")
    flow.git("add", ".", user="alice", token=flow.owner, cwd=seed)
    flow.git(
        "commit",
        "-q",
        "-m",
        "Seed with a pipeline",
        user="alice",
        token=flow.owner,
        cwd=seed,
    )
    remote = f"{flow.url}/git/{REPO}.git"
    flow.git(
        "push",
        "-q",
        remote,
        "HEAD:refs/heads/main",
        user="alice",
        token=flow.owner,
        cwd=seed,
    )
    repo = flow.api("GET", f"/repos/{seg(REPO)}", flow.owner)
    flow.api(
        "PATCH",
        f"/repos/{seg(REPO)}",
        flow.owner,
        {"revision": repo["revision"], "required_checks": ["test"]},
    )
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        diagnostics = flow.api("GET", "/admin/diagnostics", flow.owner)
        if diagnostics["runner"]["state"] == "ready":
            break
        time.sleep(0.25)
    assert diagnostics["runner"]["state"] == "ready", diagnostics["runner"]
    flow.step("project-set-up", runner=diagnostics["runner"]["detail"])

    # 1. The owner files the ticket and hands it to aiko.
    ticket = flow.api(
        "POST",
        f"/repos/{seg(REPO)}/tickets",
        flow.owner,
        {
            "title": "Greet the user",
            "body": "Add greeting.txt saying hello.",
            "labels": ["bug"],
            "lane": "new",
        },
    )
    path = f"/repos/{seg(REPO)}/tickets/{ticket['key']}"
    flow.api("PUT", f"{path}/assignee", flow.owner, {"name": "aiko"})
    flow.api("PUT", f"{path}/board", flow.owner, {"lane": "ready"})
    flow.step("ticket-filed", ticket=ticket["url"])

    code, tickets = provider(flow, aiko)
    try:
        # 2. The agent's provider picks it and starts work.
        candidates = await tickets.candidates()
        assert [(c.url, c.trigger_reason) for c in candidates] == [
            (ticket["url"], "ready_lane")
        ], candidates
        task = candidates[0]
        assert await tickets.move(task, "in_progress") is True
        lane = (await tickets.read("tickets", ticket["url"])).items[0]["lane"]
        assert lane == "in_progress", lane
        flow.step("candidate-taken", trigger=task.trigger_reason, lane=lane)

        # 3. aiko pushes the branch and opens the pull request.
        work = flow.root / "aiko"
        clone_url = await code.clone_url(REPO)
        flow.git("clone", "-q", clone_url, str(work), user="aiko", token=aiko)
        flow.git("switch", "-q", "-c", BRANCH, user="aiko", token=aiko, cwd=work)
        (work / "greeting.txt").write_text("hello\n", encoding="utf-8")
        flow.git("add", "greeting.txt", user="aiko", token=aiko, cwd=work)
        flow.git(
            "commit", "-q", "-m", "Greet the user", user="aiko", token=aiko, cwd=work
        )
        code.check_push(REPO)
        flow.git("push", "-q", "origin", BRANCH, user="aiko", token=aiko, cwd=work)
        head = flow.git("rev-parse", "HEAD", user="aiko", token=aiko, cwd=work)
        flow.step("branch-pushed", head=head)
        created = await code.create_pull_request(
            REPO,
            BRANCH,
            "main",
            "Greet the user",
            f"Closes #{ticket['key']}.",
            draft=False,
            ticket_url=ticket["url"],
            closes=True,
        )
        pull = created.target.url
        flow.step("pull-request-opened", pull=pull, created=created.created)

        # 4. The real CI runs; readiness goes from pending to ready.
        seen: list[str] = []
        states: list[str] = []
        deadline = time.monotonic() + 300
        while time.monotonic() < deadline:
            readiness = await code.readiness(pull)
            said = "/".join(
                [readiness.readiness, *(b.code for b in readiness.completion_blockers)]
            )
            if not seen or seen[-1] != said:
                seen.append(said)
            state = ",".join(
                f"{c.name}:{c.status}/{c.conclusion}" for c in readiness.checks
            )
            if not states or states[-1] != state:
                states.append(state)
            if readiness.readiness == "ready":
                break
            time.sleep(0.2)
        assert seen[-1] == "ready", (seen, states, readiness)
        assert seen[0] == "blocked/checks_pending", seen
        flow.step("ci-passed", readiness=seen, checks=states, rollup=readiness.rollup)
        checks = flow.api("GET", f"/repos/{seg(REPO)}/pulls/1/checks", flow.owner)
        job = flow.api(
            "GET",
            f"/repos/{seg(REPO)}/jobs/{checks['checks'][0]['attempt_id']}",
            flow.owner,
        )
        log = flow.api("GET", f"/repos/{seg(REPO)}/jobs/{job['id']}/log", flow.owner)[
            "log"
        ]
        assert f"{LOG_LINE} {head}" in log, log
        startup = re.search(r"sandbox started in (\d+) ms", log)
        flow.step(
            "ci-attempt",
            queued_to_started_ms=_ms(job["created_at"], job["started_at"]),
            run_ms=_ms(job["started_at"], job["finished_at"]),
            sandbox_first_output_ms=int(startup.group(1)) if startup else None,
        )

        # 5. The owner reviews and merges in the GUI.
        gui = subprocess.run(
            [
                str(flow.web / "node_modules" / ".bin" / "playwright"),
                "test",
                "e2e/flow.spec.ts",
            ],
            cwd=flow.web,
            env={
                **os.environ,
                "HOME": os.environ["BRANVIL_FLOW_REAL_HOME"],
                "BRANVIL_E2E_BASE_URL": flow.url,
                "BRANVIL_E2E_BIN": flow.binary,
                "BRANVIL_E2E_DATA_DIR": str(flow.data),
                "BRANVIL_E2E_FLOW_PULL": pull.removeprefix(flow.url),
                "BRANVIL_E2E_FLOW_LOG": f"{LOG_LINE} {head}",
                "BRANVIL_E2E_CHANNEL": os.environ.get("BRANVIL_E2E_CHANNEL", "chrome"),
            },
            capture_output=True,
            text=True,
        )
        print(gui.stdout[-4000:], gui.stderr[-2000:], sep="\n")
        assert gui.returncode == 0, "the GUI step failed"
        gui_marks = dict(re.findall(r"FLOW-GUI (\S+) (\d+) ms", gui.stdout))
        flow.step(
            "reviewed-and-merged-in-gui",
            gui_ms={k: int(v) for k, v in gui_marks.items()},
        )

        # 6. The ticket is done; the activity window lists the ticket and the PR.
        merged = (await code.read("pull_requests", pull)).items[0]
        read = (await tickets.read("tickets", ticket["url"])).items[0]
        assert (merged["merged"], merged["state"]) == (True, "closed"), merged
        assert (read["state"], read["lane"]) == ("closed", "done"), read
        from datetime import UTC, datetime, timedelta

        now = datetime.now(UTC)
        closed = await tickets.closed_since(
            now - timedelta(hours=1), now + timedelta(minutes=1)
        )
        kinds = sorted((item.kind, item.url, item.merged) for item in closed)
        assert kinds == sorted(
            [("ticket", ticket["url"], False), ("pull_request", pull, True)]
        ), kinds
        assert await tickets.candidates() == []

        tip = flow.api("GET", f"/repos/{seg(REPO)}/branches/main", flow.owner)["sha"]
        assert tip == head, (tip, head)  # fast-forward
        flow.step("ticket-done", closed_since=[f"{k}:{u}" for k, u, _ in kinds], main=tip)
    finally:
        await code.aclose()
        await tickets.aclose()
    return {
        "total_ms": round((time.monotonic() - flow.started) * 1000),
        "steps": flow.steps,
    }


def _ms(start: str | None, end: str | None) -> int | None:
    from datetime import datetime

    if not (start and end):
        return None
    parse = lambda s: datetime.fromisoformat(s.replace("Z", "+00:00"))  # noqa: E731
    return round((parse(end) - parse(start)).total_seconds() * 1000)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bin", required=True, help="the branvil binary")
    parser.add_argument(
        "--web", required=True, help="Branvil's web directory (Playwright)"
    )
    parser.add_argument(
        "--keep", action="store_true", help="keep the temporary directory"
    )
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="branvil-flow-"))
    # GuildBotics' modules read nothing of the user's home.
    os.environ["BRANVIL_FLOW_REAL_HOME"] = os.environ["HOME"]
    os.environ["HOME"] = str(root / "home")
    (root / "home").mkdir()
    os.chdir(root)
    flow = Flow(root, args.bin, Path(args.web).resolve())
    try:
        summary = asyncio.run(run(flow))
    except BaseException:
        print(f"service log: {root / 'serve.log'} (kept)", file=sys.stderr)
        flow.stop()
        raise
    flow.stop()
    print("FLOW-SUMMARY " + json.dumps(summary))
    if args.keep:
        print(f"kept {root}")
    else:
        shutil.rmtree(root, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
