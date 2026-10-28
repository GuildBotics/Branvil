# Branvil

Branvil is a lightweight local code hosting service for GuildBotics: Git
repositories over Smart HTTP in workspaces, each workspace with one kanban
board for the tickets of its repositories, pull requests with reviews and a
serial landing queue, and a CI that runs each job in a microVM. It is one Rust
process that keeps metadata in redb and Git data in bare repositories, and
listens on loopback only; CI jobs run in a separate `branvil runner` process.

Status: the service core (Git hosting, workspaces, tickets and boards, pull requests,
reviews, merge, recovery), CI (pipelines, queue, runner, microVM executor),
following an upstream on GitHub and publishing pull requests to it,
`branvil tunnel`, and backup/restore are implemented and tested. The GuildBotics `branvil` provider lives in GuildBotics
(`guildbotics/integrations/branvil/`); this repository checks it against a
real service (see "Checking with GuildBotics"). See
[`docs/architecture.md`](docs/architecture.md) for what exists.

## Important Notice (Disclaimer)

- This software is in alpha. Its data format, HTTP API, configuration, and
  command line will change incompatibly without notice.
- The author and distributor assume no responsibility for malfunctions of this
  software or damages caused thereby. Use it at your own risk.

## Requirements

- Rust 1.90 or later (developed with 1.96, edition 2024)
- Git 2.38 or later on `PATH` (the service runs `git http-backend`,
  `update-ref`, `merge-tree --write-tree`, `diff`, …, and the CI runner checks
  the source out with it; developed with 2.54)

Verified on macOS 26 (arm64). Linux and Windows are not verified.

For CI: the microsandbox runtime `msb` 0.6.17 (with its `libkrunfw`) and a
hypervisor (macOS on Apple Silicon: built in; Linux: KVM; Windows: Windows
Hypervisor Platform, preview). GuildBotics deploys this runtime, and the runner
uses that copy; see [`docs/ci.md`](docs/ci.md).

## Build

```sh
cargo build --release
```

The binary is `target/release/branvil`. The browser GUI from `web/dist` is
embedded when it exists at build time; without it the service runs and answers
web pages with a "GUI is not built" notice.

## First run

```sh
# Data directory, configuration (branvil.toml), and the owner: the one person
# who uses Branvil. The owner's token goes to ./data/secrets/owner.token.
branvil init --data-dir ./data --owner alice

# Start the service. Ctrl-C stops it: open GUI event streams end at once,
# other requests get 5 seconds to finish, and the stop waits for the
# repository operations already started (a merge, recording a push); Git
# never gets the Ctrl-C, and a second Ctrl-C does nothing. `kill -9` ends it
# at once and is safe: the next start waits for the Git left behind. SIGTERM
# does the same once the running CI jobs have finished (docs/ci.md,
# "Stopping the service").
branvil serve --data-dir ./data

# Open the GUI in the browser; there is no sign-in screen.
branvil open --data-dir ./data
```

Without `--data-dir` (or `BRANVIL_DATA_DIR`), the data directory is the OS
convention for `branvil` (macOS: `~/Library/Application Support/branvil`). The
configuration file is `<data dir>/branvil.toml` unless `--config` names
another. `branvil init` writes `listen = "127.0.0.1:7878"` and
`base_url = "http://127.0.0.1:7878"`; `--listen` and `--base-url` change them.
The base URL is permanent: every URL Branvil returns (tickets, pull requests,
clone URLs) is built from it, so keep it the same on every machine and over
time.

`branvil serve` prints `branvil listening on http://127.0.0.1:7878 (base URL …)`
once it accepts requests. It refuses non-loopback listen addresses.

Branvil has one human, the owner; everyone else is an agent (GuildBotics'
AI members) or a CI runner, each with its own token. Whoever can run commands
as the Hub machine's OS user is the owner: the owner's token is the `0600` file
`<data dir>/secrets/owner.token`, and the CLI on the Hub uses it by default.
`branvil open` asks the service for a single-use link (valid for 60 seconds)
and opens it with the system's browser; the GUI exchanges it for a session
cookie that lasts 30 days (`limits.session_hours`). `branvil open --print`
prints the link instead (for a headless Hub, or a Hub shared with other OS
users: the command line of the browser launcher is visible in `ps`). On other
machines, a local app such as GuildBotics Desktop gets an owner token of its
own once the owner allows it on Branvil's consent page (`/-/authorize`), and
opens the GUI with an entry link the same way (`docs/architecture.md`, "Local
apps"). There is no sign-out: to end every session, stop the service, delete
`secrets/owner.token`, and start it again (a new owner token is written and
the old one stops working; tokens given to local apps stay until revoked under
Settings → Members → owner → Tokens).

## Set up a project

The `member`, `workspace`, `repo`, `backup`, and `open` commands talk to
the running service. They take the base URL from the configuration (or `--url`
/ `BRANVIL_URL`) and a token from `--token`, `BRANVIL_TOKEN`, or else the
owner's token file of the data directory.

```sh
export BRANVIL_DATA_DIR=./data

branvil workspace create acme
branvil repo create acme/demo --label bug --label feature
branvil member create aiko --kind agent
branvil member token aiko          # prints aiko's token once
```

A workspace holds repositories (`acme/demo`: the workspace and a name) and
has one board. A repository holds its tickets and numbers them from 1 (`#1`),
apart from its pull requests (`!1`); a ticket carries only labels its
repository defines. Every ticket is on its workspace's board, in a lane: at
the end of New unless it is created in another lane. A merge closes only
tickets of the pull request's own repository. On the ticket page, a ticket
gets a parent ticket (it is not completed while a child is open) and tickets
it is blocked by (it is not moved to In progress while one of them is open),
among the tickets of its workspace.

On an open pull request, **Related tickets → Edit related tickets** adds
tickets from the same workspace, removes links, and chooses **Close on merge**
for each ticket of the PR's repository. Tickets of other repositories are
references only. Saving checks the revision the editor opened on; if someone
changed the PR meanwhile, cancel (discarding the unsaved edits), open the editor
again, and reapply the changes to the latest links. The editor shows known ticket
titles and marks closed tickets in the choices. Closed and merged PRs show the
related tickets section only when links exist.

The sidebar lists the task flow before the task board. **Settings → Display**
(under **All of Branvil**) can hide or show the task board's sidebar link (shown
by default). This
preference is saved in the current browser and leaves direct board URLs usable.
When the board is hidden, opening `/`, switching from a board or ticket to
another workspace, or creating a workspace opens its task flow instead.

What a member may do follows from its kind. The owner may do everything. An
agent reads everything and works in every repository: tickets, comments,
reviews, pull requests, pushes to unprotected branches. Creating workspaces
and repositories, settings and upstreams, members and their accounts, backups,
merging, publishing, and marking a draft pull request ready are the owner's. A
merge or a publish needs a passed landing gate at the front of its base
branch's queue, the repository's required checks on the head, and its
required approvals (Settings → Repositories → a repository → CI; 1 unless
changed, 0 needs none): members other than the author whose last approve or
request changes approves the current head (a comment keeps it) and who were
not asked to review again since. The
author can only comment on their own pull request. With **Land automatically**
on the same tab, the repository merges (or, with an upstream, publishes once)
by itself an open pull request that is not a draft as soon as it meets them.
The queue and gates also run with this setting off. Only the first pull request
in each repository/base queue gets a gate: the head itself when it contains
the base, otherwise a merge commit of base and head whose required checks
must succeed. The head and its approvals stay unchanged. The next pull request
waits until the first merges or leaves the queue, including while GitHub's
merge is pending. The pull request list and each pull request's page show
where it is on its way into the base and whose turn it is. Keeping a pull
request a draft holds it. A runner uses only the runner API.

Under Settings → Members → a member → Reviewers, the owner chooses who is
asked to review every pull request that member opens (owners and agents other
than the member). Changing them leaves pull requests already open as they are;
on a pull request's page the requests are changed one by one.

A ticket has at most one assignee: the agent that works on it. GuildBotics'
provider takes a ticket as an agent's work when that agent is its assignee, so
assigning another agent replaces the assignee instead of adding a second one.
The Interactive switch on a ticket's page marks it as worked on by the owner
with an agent at hand, and the board badges its card. GuildBotics' provider
then never takes up the ticket, nor the agent's own pull requests that link
it; reviewing those pull requests goes on as usual. The descendants of an
interactive ticket are interactive too, whatever their own switches say; their
cards' badges name that ticket.

## Git

Clone URL: `<base URL>/git/<repo>.git`, for example
`http://127.0.0.1:7878/git/acme/demo.git`. Fetch and push both use HTTP Basic:
the username is the member name, the password is one of the member's tokens.
Give them to Git through a credential helper or `GIT_ASKPASS` rather than in
the URL (a URL with credentials ends up in shell history and may be stored by
the system credential helper). The tests in `tests/` use an askpass script that
reads the name and token from environment variables.

The default branch is protected: it changes only through a merged pull
request. The first push of the default branch into an empty repository is
allowed for the owner.

## Several machines

Branvil runs once, on the GuildBotics Hub machine, listening on loopback. Every
machine uses the same base URL (`http://127.0.0.1:7878`), so URLs recorded in
GuildBotics work everywhere. On every other machine, `branvil tunnel` forwards
that local port to the Hub over the SSH connection you already use for the Hub:

```sh
branvil tunnel --via hub            # a Host from ~/.ssh/config, or user@host
branvil tunnel --via hub --port 7878 --remote-port 7878
```

It runs `ssh -N -L 127.0.0.1:7878:127.0.0.1:7878 -o ExitOnForwardFailure=yes
-o ServerAliveInterval=15 -o BatchMode=yes -- hub` with the system OpenSSH
client, prints `connecting` / `up` / `down … reconnecting in …`, and starts ssh
again with a growing delay (1 s doubling to 60 s) whenever it exits. Keys, host
keys, and host names come from your OpenSSH configuration; Branvil stores none
of them. `BatchMode=yes` means ssh never prompts: use a key without a
passphrase or one loaded in `ssh-agent`, and accept the Hub's host key once
with a plain `ssh hub` first. Keep `--port` equal to the base URL's port:
Branvil refuses other `Host` ports. When the tunnel is down, clients get a
connection error; nothing else changes.

### Keeping the tunnel running

The following were written for this build but **not executed** on these
systems; adjust paths (`which branvil`) and the destination.

macOS (launchd), `~/Library/LaunchAgents/com.guildbotics.branvil-tunnel.plist`,
then `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.guildbotics.branvil-tunnel.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.guildbotics.branvil-tunnel</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/branvil</string>
    <string>tunnel</string>
    <string>--via</string>
    <string>hub</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/tmp/branvil-tunnel.log</string>
</dict>
</plist>
```

Linux (systemd user unit), `~/.config/systemd/user/branvil-tunnel.service`,
then `systemctl --user daemon-reload && systemctl --user enable --now branvil-tunnel`
(and `loginctl enable-linger $USER` to run it without a login session):

```ini
[Unit]
Description=Branvil SSH tunnel to the Hub
After=network-online.target

[Service]
ExecStart=%h/.cargo/bin/branvil tunnel --via hub
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

The unit needs `ssh-agent` or a key without a passphrase; a user unit does not
see the `SSH_AUTH_SOCK` of a desktop session unless you import it
(`systemctl --user import-environment SSH_AUTH_SOCK`).

Windows (Task Scheduler), in PowerShell, using the OpenSSH client that ships
with Windows:

```powershell
$action  = New-ScheduledTaskAction -Execute "$env:USERPROFILE\.cargo\bin\branvil.exe" -Argument "tunnel --via hub"
$trigger = New-ScheduledTaskTrigger -AtLogOn
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit 0
Register-ScheduledTask -TaskName "Branvil tunnel" -Action $action -Trigger $trigger -Settings $settings
```

## CI

Put the pipeline in the repository as `.branvil/ci.toml`. Every commit runs
its own copy, so a pull request that changes it runs the changed pipeline as
its checks; review that change like any other part of the diff:

```toml
[[jobs]]
name = "test"
image = "docker.io/library/alpine:3.22"
commands = ["./scripts/test.sh"]
timeout_secs = 600
artifacts = [{ name = "report", paths = ["out/"] }]
```

Then require the job for merging (`PATCH /api/v1/repos/{repo}`
`{"required_checks": ["test"]}`) and start a runner. The runner is a separate
process; it needs a runner member's token and the images in its cache (jobs
never pull; which version of a tag runs is the one the cache holds, so pulling
or loading the tag again moves the jobs to its new version):

```sh
branvil runner register runner1                                # on the Hub: prints runner1's token once
branvil runner pull docker.io/library/alpine:3.22
branvil runner doctor                                          # msb found? executor ready?
BRANVIL_TOKEN=<runner token> branvil runner                    # runs until stopped
```

Or let the service start and supervise one: under Settings → CI in the GUI
the owner switches the runner on, sets the job resources, and pulls an image or
loads one from a `docker save` archive on the Branvil machine (the page shows
the tag to write as `image`); a repository's CI tab allows its jobs the
network. The same settings are `runner = "auto"` and `[ci.executor]` in
`branvil.toml`.

Each job runs in its own microVM as a user that is not root (uid 1000), with
a Git checkout of the commit (its history and tags, no remote) mounted at
`/work`, `/tmp` on a disk of its own rather than in memory, no network (the
owner may allow public destinations per repository with `ci_network`), the
CPUs and memory of the runner's `[ci.executor]` settings, and its
`timeout_secs` capped by the runner's time limit (which is the limit of a
job that gives none). Without a runner, jobs end
`startup_failure` after two minutes; nothing ever runs on the host. Logs,
artifacts, retry and cancel, states, limits, and the trust model:
[`docs/ci.md`](docs/ci.md).

### This repository's pipeline

[`.branvil/ci.toml`](.branvil/ci.toml) runs the checks of "Development" as
six jobs: `fmt` (`cargo fmt --check`), `clippy` (`cargo clippy --all-targets
-- -D warnings`), `test` (`cargo test`), `web` (`npm ci`, then `npm run lint`,
`npm run typecheck`, and `npm run test` in `web`), `dev-script` (the Python
launcher tests), and `branvil-integration` (`scripts/guildbotics-smoke.sh`
with GuildBotics' `main`; see "Checking with GuildBotics"). Their image is
GuildBotics' development image
(`guildbotics/agent-environment:dev`, built by GuildBotics'
`scripts/build-agent-environment-image.sh`): the owner loads its `docker save`
archive under Settings → CI, and the pipeline names it by its tag. When the
image is rebuilt, loading the new archive is all it takes (GuildBotics' script
does so itself); the pipeline stays as it is. `clippy` and `test` fetch crates
from crates.io and `web` fetches packages from `registry.npmjs.org`, so the
repository's CI tab must allow its jobs the network. `clippy` and `test` cache
Cargo's registry and `target`, keyed by `Cargo.lock`; `web` caches npm's
downloads, keyed by `web/package-lock.json`.

## Upstream (GitHub)

Branvil is where agents try things — many pushes, review rounds, and CI runs,
locally and without cost. A repository can have an **upstream** on GitHub that
stays the place of record: its protected default branch, people's approvals
and merges, its CI (Windows included), releases, and the pull requests people
outside see. Branvil never pushes the default branch; it follows it.

1. Give every agent a GitHub account in Branvil, checked with GitHub: the
   account that publishes its pull requests. It is a GitHub App (its app ID,
   the ID of its installation on the account that owns the repositories, and
   its private key; it acts as `<app>[bot]`, and Branvil makes its short-lived
   tokens itself) or a token of a bot account. One of them also reads GitHub
   for Branvil, so your own token is not needed (set it only to publish pull
   requests you author yourself):

   ```sh
   branvil member account aiko --app-id 123456 --installation-id 7890123 --private-key-file aiko-app.private-key.pem
   KEN_GITHUB_TOKEN=... branvil member account ken --token-env KEN_GITHUB_TOKEN
   ```

   Settings → Members → a member → Accounts on code hosts does the same.

2. Point the repository at its GitHub repository. An empty repository is filled
   with GitHub's default branch; an existing one must be the same or behind:

   ```sh
   branvil repo create acme/demo
   branvil repo upstream acme/demo --repository acme/demo
   ```

   The repository settings show whose account reads it; `--reader <member>`
   (or the settings) chooses another.

3. Agents work in Branvil as before. On a pull request the owner finds
   **Publish to GitHub** where the merge button was: it shows exactly what will
   be posted (title, body, and `Closes #12` for linked tickets that name a
   GitHub issue), then pushes the branch and opens the pull request on GitHub
   as the author's account. GitHub's checks of the head appear among the pull
   request's checks (`github/<name>`, with their logs), later pushes and edits
   follow, and once a person merges on GitHub, Branvil's default branch follows
   and its pull request and tickets close.

   With **Land automatically** on, Branvil publishes a pull request by itself
   once it meets the conditions. Each time its landing gate passes, Branvil
   tells a webhook, with links to both pull requests, that it is ready to
   merge on GitHub once GitHub's checks of its head have succeeded. This
   includes a new gate for an already published head. When a check on GitHub
   fails, Branvil says so on its pull request instead. Under
   Settings → Notifications, the owner enters its URL (a Slack Incoming Webhook takes the
   message as it is), which is kept like a credential and never shown again.

Members read the GitHub repository's Dependabot alerts through Branvil
(`docs/api.md`, "Dependency alerts"), with the reader's account; Branvil
keeps none of them. For that the reader needs Dependabot alerts enabled for
the repository and, for a GitHub App, Repository permissions → **Dependabot
alerts: Read-only** with access to the repository (adding a permission to an
installed app takes effect once the account that installed it approves it);
for a token, the same permission (fine-grained) or the `security_events`
scope (classic).

Branvil polls GitHub with conditional requests (an unchanged answer is a free
`304`): every 60 s while a published pull request is open, every 15 min
otherwise, and at once after publishing or "Check now". GitHub registers the
checks of a pushed head a little later, so a head waits for more checks until
`checks_registration_secs` after the push (or the pull request's creation,
edit, or reopening there) and while a workflow run of it has not finished.
The registration wait defaults to 60 s. The poller also wakes at its deadline
to read GitHub, without waiting for the next regular poll; only a successful
read ends the registration wait.
The reading account needs, for a GitHub App, Repository permissions →
**Checks: Read-only** and **Actions: Read-only** (the workflow runs and the
jobs' logs); for a token, the same permissions (fine-grained) or the `repo`
scope (classic).
Settings in `branvil.toml`:

```toml
[upstream]
poll_active_secs = 60
poll_idle_secs = 900
checks_registration_secs = 60
```

Notes:

- Conversation, reviews, threads, tickets, and boards stay in Branvil; back
  them up with `branvil backup`.
- A ticket names the GitHub issue it stands for in its "GitHub issue" field
  (`upstream_issue` in the API); without it nothing about the ticket is written
  on GitHub.
- Branvil's references in the title and body (`#3`, `acme/demo#3`, `!3`, its
  URLs) mean something else on GitHub, so publishing rewrites them: to the
  GitHub issue or pull request they stand for, or to a code span that GitHub
  does not link. The preview shows the result; when it still needs a hand, the
  owner edits the title and body in the publish panel.
- Real GitHub was not used for this build; the tests use a stand-in that
  answers GitHub's REST API, with bare repositories as its Git side.
- Design: [`docs/architecture.md`](docs/architecture.md) ("Upstream"); API:
  [`docs/api.md`](docs/api.md) ("Upstream").

## Backups

```sh
branvil backup --to /srv/backups/branvil-2026-10-14     # on the Hub, service running
branvil restore --from /srv/backups/branvil-2026-10-14 --data-dir /srv/branvil-restored
branvil admin check --data-dir /srv/branvil-restored
```

A backup holds the database, every repository (as a Git bundle), CI logs and
artifacts, the configuration, and a manifest with checksums. It is taken while
the service runs, with ref changes paused for the moment the database is copied
and the repositories are bundled. Members' upstream tokens are not in it,
nor the owner's token: the restored service writes a new one at its first start
and starts with no GUI sessions.
See [`docs/backup-restore.md`](docs/backup-restore.md).

## Maintenance

With the service stopped:

```sh
branvil admin check --data-dir ./data     # redb structure + every index against the records
branvil admin reindex --data-dir ./data   # rebuild every index from the records
branvil admin upgrade --data-dir ./data   # after updating Branvil, when the service says so
```

A build whose stored format changed refuses the data directory of the version
before it until `branvil admin upgrade` upgrades it; take a backup first. An
upgraded database is then refused by `branvil serve` until
`branvil admin check` passes. See "Upgrading" in
[`docs/backup-restore.md`](docs/backup-restore.md).

Logging goes to stderr; `BRANVIL_LOG` takes a `tracing` filter
(default `branvil=info`).

## Development

To build and start the server with its browser GUI from this checkout:

```sh
./scripts/dev.sh
```

Requires Node.js 24 or later and npm, in addition to Rust and Git. The script
runs `npm ci` when `web/node_modules/.package-lock.json` is missing or older
than `web/package-lock.json`, builds the GUI and the debug binary,
initializes `<repo>/data` on the first run with owner name `owner`,
and opens the GUI after the server is ready. Later runs reuse the data and
`branvil.toml`. Ctrl-C stops the server; restart the script to pick up code
changes. It does not watch files.

Cargo builds everything again under new names when the build conditions
change, and leaves the old outputs in `target`. Before building, the script
runs `scripts/clean-stale-target.sh target`, which runs `cargo clean` when
`target` exists and its record of `rustc -vV` and the `[profile.*]` tables of
`Cargo.toml` (`target/.build-conditions`) is missing or differs from the
current ones. After building, even when the build fails, it runs
`scripts/clean-stale-target.sh --mark target` to record the current ones.
Neither creates `target`: Cargo marks only a directory it creates itself as a
cache to leave out of backups (`CACHEDIR.TAG`, and on macOS the Time Machine
exclusion). The same script can be run on any other target directory.

Use a different data directory and initial owner, or skip opening the browser:

```sh
./scripts/dev.sh --data-dir /path/to/dev-data --owner alice
./scripts/dev.sh --no-open
```

`BRANVIL_DATA_DIR` also selects the data directory; `--data-dir` takes precedence.
Relative data paths are resolved from the repository root. `--owner` applies
only when initializing a new database. An existing configuration keeps its
listen address and base URL (by default `http://127.0.0.1:7878`).

If the browser cannot open, the script prints a single-use GUI link valid for
60 seconds and keeps the server running. Both attempts use this data
directory's credentials and URL rather than `BRANVIL_TOKEN` or `BRANVIL_URL`
from the calling shell. `--no-open` skips both attempts.

Instead of starting a server, `scripts/dev.sh --install DIR` builds the GUI
and a release server with the GUI built into it, puts the server at
`DIR/branvil`, and exits (`scripts/serve.sh` and `scripts/update.sh` install
builds so). `DIR` must be named after the checkout's `HEAD` commit, and the
tracked files must have no uncommitted changes, so that a build named after a
commit is that commit. It assembles `DIR.new` and renames it to `DIR`, so
`DIR` exists only when it is complete.

To check the scripts without building or starting a real server (Python 3,
Node.js, Git, POSIX only):

```sh
python3 -m unittest discover -s tests -p test_dev_script.py -v
```

To run the Rust checks:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

To run the GUI checks (lint, types, and the Vitest tests):

```sh
cd web
npm ci
npm run lint
npm run typecheck
npm run test
```

The tests use at most four workers and write file and test timings to
`web/test-results/timing.json`; CI keeps this in the `web-tests` artifact
and streams Vitest output into the job log. Measurements and interpretation
are in
[`docs/performance.md`](docs/performance.md#web-tests).

The integration tests start real `branvil serve` processes on free loopback
ports and drive them with real Git clients isolated from the user's Git
configuration and credential helpers. They are one test crate,
[`tests/main.rs`](tests/main.rs), with a module per file:
`cargo test --test main tickets::` runs the tests of `tests/tickets.rs`.

### Running the service from this checkout

`scripts/serve.sh` keeps a Branvil built from this checkout running and updates
it after every merge to `main` whose required checks have passed, with nobody
watching.
Run it as a launchd user agent (instead of `scripts/dev.sh` in a terminal):

```sh
./scripts/serve.sh --plist > ~/Library/LaunchAgents/com.guildbotics.branvil.plist   # or: --data-dir PATH, --interval SECONDS
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.guildbotics.branvil.plist
```

The agent runs `/bin/bash scripts/serve.sh` with the data directory chosen as
`scripts/dev.sh` chooses it (it must be initialized already; `scripts/dev.sh`
does that on its first run) and with the `PATH` of the shell that printed it,
because launchd's own has no `cargo`, `npm`, or `node` (print and bootstrap it
again when that changes). launchd starts it at login and again when it exits.
Its output and that of what it runs, including the service's log, go to
`~/Library/Logs/branvil.log`; the service's standard output, with the
`branvil listening on …` line that says it is ready, goes to
`builds/serve.out` instead. A restart
(`launchctl kickstart -k gui/$(id -u)/com.guildbotics.branvil`) checks for an
update at once and applies changes to `scripts/serve.sh`, as an update does.
To switch from `scripts/dev.sh`, stop it (Ctrl-C) first: both cannot listen
on the same address.

Run `scripts/serve.sh` from a clone of its own that nobody develops in, whose
`origin` is the repository in this Branvil
(`<base URL>/git/<workspace>/<repository>.git`, which `scripts/update.sh`
fetches as the owner), and pass `--data-dir` when the service's data is not
that clone's `data`. Updates switch the clone to `main` and fast-forward it;
an uncommitted change to a tracked file, or a commit on its `main` that
`origin` lacks, stops them. A worktree of the repository you develop in does
not work: it cannot check out `main` while another worktree has it.

On its first start it installs the checkout's `HEAD` as a release build under
`builds/` (ignored by Git) and runs it only once that release build has
finished, which takes minutes; building the checkout, `npm run build`, or
`cargo clean` never change what runs. It starts the service again when it
exits, and at its start and every 5 minutes (`--interval`) it updates to
`origin/main` when that commit's run in this Branvil has finished and every
check the repository requires for a merge succeeded (a check that is not
required, such as `branvil-integration`, is waited for but decides nothing;
require every job that must keep a commit out of the service, here `fmt`,
`clippy`, `test`, `web`, and `dev-script`):
it backs the service up, builds the new commit and rehearses its upgrade on a
copy of the backup while the old build serves, and stops the service only to
switch builds, once its running CI jobs have finished. A commit whose required checks failed is checked again each round and can
update after a successful retry. An installation, rehearsal, or activation
failure waits for a person or a newer main commit.
Failures are written to the log, sent to the configured notification webhook
(also while the service is stopped), and shown once as a macOS notification
(`osascript`, which macOS lists as Script Editor under System Settings →
Notifications; allow it once, for instance after
`osascript -e 'display notification "test" with title "Branvil"'`).

How it works, how to read its state, and what to do when it needs a person
are in "Upgrading" in [`docs/backup-restore.md`](docs/backup-restore.md).

### GitHub Actions

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on pushes to `main`
and pull requests targeting `main`. It runs only what this repository's
pipeline cannot, on `macos-latest`, and only for the changes it concerns: its
`checks-macos` job runs `cargo clippy --all-targets -- -D warnings` (Rust
1.96.0), which compiles and lints the code built for macOS alone, when `*.rs`,
`Cargo.toml`, or `Cargo.lock` changes, and the Python development launcher
tests above under `/bin/bash` (the bash launchd runs `scripts/serve.sh` with,
and the oldest one the scripts meet) when `scripts/` or
`tests/test_dev_script.py` changes. A change to the workflow
runs both. The job reports for every change, with the steps it does not
concern skipped. Every other check runs in this repository's pipeline. The opt-in real microVM
executor tests still require the runtime and image described in
[`docs/ci.md`](docs/ci.md).

For a pull request published to GitHub by the owner, Branvil imports the job's
result as `github/checks-macos` (see "Upstream (GitHub)").

### Checking with GuildBotics

GuildBotics' `branvil` provider is this API's main client. Its port contracts
(`tests/guildbotics/integrations/contracts/` in GuildBotics) run against a
Python double of the API; `scripts/guildbotics-smoke.sh` checks that the
production provider and a real `branvil serve` still work together:

```sh
# a GuildBotics checkout, environment synced (uv sync --extra test --extra dev)
scripts/guildbotics-smoke.sh /path/to/GuildBotics
```

It prints the commits of both checkouts, builds the debug binary, and walks
three paths, each against its own service (temporary data directory, free
loopback port, members and credentials of its own) through the provider that
GuildBotics' factory makes:

- tickets: created, changed, and read back with their assignee and lane;
  a parent and a ticket it is blocked by read back from both sides and are
  taken back; a ready ticket blocked by an open one is no patrol candidate and
  cannot be moved in progress until its blocker is closed;
- Git, pull request, and merge: a push with the provider's clone URL and
  credential, a pull request that closes a ticket, and, after the owner's
  merge, the pull request merged and the ticket closed in the done lane;
- checks: a pull request is not ready while its head's job is pending, is
  ready once that commit's job succeeds, and is not ready again when its head
  moves on.

Only the owner's setup and merge and the runner's claim and finish call the
API directly; every result is read through the provider. It needs no microVM,
browser, or running service, and touches nothing of this machine's GuildBotics
or Branvil ([`integration/guildbotics/test_smoke.py`](integration/guildbotics/test_smoke.py)).

Run it when you change the HTTP API, and GuildBotics runs it when its provider
changes. Both repositories' pipelines run it as the job `branvil-integration`
on `main` and every pull request head, each with the other's `main` cloned
from GitHub (CI jobs reach only public networks); the job is not a required
check. When it fails, the log names both commits: a change on this side is
fixed in its own pull request, a `main` of the other that no longer works with
this one is fixed there.

## Documents

- [`docs/api.md`](docs/api.md): the HTTP API contract
- [`docs/port-mapping.md`](docs/port-mapping.md): GuildBotics port operations → API
- [`docs/storage-design.md`](docs/storage-design.md): tables, keys, indexes, transactions
- [`docs/architecture.md`](docs/architecture.md): modules, ref-change coordination, recovery, CI, upstream, backups
- [`docs/ci.md`](docs/ci.md): pipelines, trust model, states, limits, runner, executor and its prerequisites
- [`docs/performance.md`](docs/performance.md): measurements
- [`docs/security.md`](docs/security.md): threat model, authentication, Git, secrets, upstream tokens, what is not protected
- [`docs/backup-restore.md`](docs/backup-restore.md): backup contents, consistency, restore, credentials
