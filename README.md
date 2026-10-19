# Branvil

Branvil is a lightweight local code hosting service for GuildBotics: Git
repositories over Smart HTTP in workspaces, each workspace with one kanban
board for the tickets of its repositories, pull requests with reviews and a
fast-forward merge, and a CI that runs each job in a microVM. It is one Rust
process that keeps metadata in redb and Git data in bare repositories, and
listens on loopback only; CI jobs run in a separate `branvil runner` process.

Status: the service core (Git hosting, workspaces, tickets and boards, pull requests,
reviews, merge, recovery), CI (pipelines, queue, runner, microVM executor),
following an upstream on GitHub and publishing pull requests to it,
`branvil tunnel`, and backup/restore are implemented and tested. The GuildBotics `branvil` provider lives in GuildBotics
(`guildbotics/integrations/branvil/`); its port contracts also run against a
real service from this repository (see "Development"). See
[`docs/architecture.md`](docs/architecture.md) for what exists.

## Important Notice (Disclaimer)

- This software is in alpha. Its data format, HTTP API, configuration, and
  command line will change incompatibly without notice.
- The author and distributor assume no responsibility for malfunctions of this
  software or damages caused thereby. Use it at your own risk.

## Requirements

- Rust 1.90 or later (developed with 1.96, edition 2024)
- Git 2.28 or later on `PATH` (the service runs `git http-backend`,
  `update-ref`, `diff`, …; developed with 2.54)

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

# Start the service. Ctrl-C or SIGTERM stops it: open GUI event streams end at
# once, and other requests get 5 seconds to finish.
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
repository defines. A ticket is on its workspace's board, in a lane, when it is
created there or put there. A merge closes only tickets of the pull request's
own repository.

What a member may do follows from its kind. The owner may do everything. An
agent reads everything and works in every repository: tickets, comments,
reviews, pull requests, pushes to unprotected branches. Creating workspaces
and repositories, settings and upstreams, members and their accounts, backups,
merging, publishing, and marking a draft pull request ready are the owner's; a merge needs no approval (the owner
who merges is the only reviewer whose verdict could count), only the
repository's required checks on the head. A runner uses only the runner API.

A ticket has at most one assignee: the agent that works on it. GuildBotics'
provider takes a ticket as an agent's work when that agent is its assignee, so
assigning another agent replaces the assignee instead of adding a second one.

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
image = "docker.io/library/alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8"
commands = ["./scripts/test.sh"]
timeout_secs = 600
artifacts = [{ name = "report", paths = ["out/"] }]
```

Then require the job for merging (`PATCH /api/v1/repos/{repo}`
`{"required_checks": ["test"]}`) and start a runner. The runner is a separate
process; it needs a runner member's token and the images in its cache (jobs
never pull):

```sh
branvil runner register runner1                                # on the Hub: prints runner1's token once
branvil runner pull docker.io/library/alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8
branvil runner doctor                                          # msb found? executor ready?
BRANVIL_TOKEN=<runner token> branvil runner                    # runs until stopped
```

Or let the service start and supervise one: under Settings → CI in the GUI
the owner switches the runner on, sets the job limits, and pulls an image or
loads one from a `docker save` archive on the Branvil machine (the page shows
the reference to write as `image`); a repository's CI tab allows its jobs the
network. The same settings are `runner = "auto"` and `[ci.executor]` in
`branvil.toml`.

Each job runs in its own microVM with only the commit's tree mounted at
`/work`, no network (the owner may allow public destinations per repository
with `ci_network`), and the job's CPU, memory, and time limits capped by the
runner's `[ci.executor]` settings. Without a runner, jobs end
`startup_failure` after two minutes; nothing ever runs on the host. Logs,
artifacts, retry and cancel, states, limits, and the trust model:
[`docs/ci.md`](docs/ci.md).

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

Branvil polls GitHub with conditional requests (an unchanged answer is a free
`304`): every 60 s while a published pull request is open, every 15 min
otherwise, and at once after publishing or "Check now". Settings in
`branvil.toml`:

```toml
[upstream]
poll_active_secs = 60
poll_idle_secs = 900
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
before it until `branvil admin upgrade` upgrades it; take a backup first. See
"Upgrading" in [`docs/backup-restore.md`](docs/backup-restore.md).

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

To check the launcher without building or starting a real server (Python 3,
POSIX only):

```sh
python3 -m unittest discover -s tests -p test_dev_script.py -v
```

To run the Rust checks:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

The integration tests start real `branvil serve` processes on free loopback
ports and drive them with real Git clients isolated from the user's Git
configuration and credential helpers. They are one test crate,
[`tests/main.rs`](tests/main.rs), with a module per file:
`cargo test --test main tickets::` runs the tests of `tests/tickets.rs`.

### GitHub Actions

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on pushes to `main`
and pull requests targeting `main`. Its `checks-macos` job uses `macos-latest`
and Rust 1.96.0 with rustfmt and clippy to run `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings`, `cargo test`, and the Python
development launcher tests above. The opt-in real microVM executor tests
still require the runtime and image described in [`docs/ci.md`](docs/ci.md).

For a pull request published to GitHub by the owner, Branvil imports the job's
result as `github/checks-macos` (see "Upstream (GitHub)").

### GuildBotics port contracts against a live service

GuildBotics' port contracts (`tests/guildbotics/integrations/contracts/` in
GuildBotics) run their `branvil` cases against a Python double of this API.
`scripts/guildbotics-contracts.sh` runs the same, unchanged contracts against a
real `branvil serve` instead:

```sh
# a GuildBotics checkout with the branvil provider, environment synced
scripts/guildbotics-contracts.sh /path/to/GuildBotics
```

It builds the debug binary and runs GuildBotics' pytest with the plugin
[`integration/guildbotics/branvil_live.py`](integration/guildbotics/branvil_live.py),
which replaces the double's harness: every test gets its own service (temp data
directory, free port), members, workspaces, and repositories, real Git
pushes for branches, and a fake CI runner that concludes queued attempts
`success` (and uploads artifacts) through the runner API before the provider
reads checks. A skipped contract fails the run. The module docstring lists
exactly what the harness sets up and how it counts writes.

### End-to-end flow

```sh
scripts/e2e-flow.sh /path/to/GuildBotics
```

Runs the whole path once on a fresh service with the real runner
(`[ci] runner = "auto"`, microVM executor): the owner files a ticket and
assigns the agent `aiko`; GuildBotics' provider takes it (`candidates()`,
`move()`); `aiko` pushes `ticket/1` with Git and the provider opens a pull
request that closes the ticket; the required check runs in a microVM and the
provider's readiness goes from pending to ready; the owner enters the GUI with
`branvil open --print`, looks at the check, its log, and the diff, and merges ([`web/e2e/flow.spec.ts`](web/e2e/flow.spec.ts), Playwright
with the installed Google Chrome; `BRANVIL_E2E_CHANNEL=` for Playwright's
Chromium); the ticket ends closed in the done lane, `closed_since()` lists the
ticket and the pull request, and `main` is the merged head (the repository has
no upstream; publishing is covered by `tests/upstream.rs`). It needs msb 0.6.17 with the pinned alpine image in
Branvil's image cache (see "CI") and `web/node_modules`. It prints the time of
every step ([`docs/performance.md`](docs/performance.md) has measured runs).

## Documents

- [`docs/api.md`](docs/api.md): the HTTP API contract
- [`docs/port-mapping.md`](docs/port-mapping.md): GuildBotics port operations → API
- [`docs/storage-design.md`](docs/storage-design.md): tables, keys, indexes, transactions
- [`docs/architecture.md`](docs/architecture.md): modules, ref-change coordination, recovery, CI, upstream, backups
- [`docs/ci.md`](docs/ci.md): pipelines, trust model, states, limits, runner, executor and its prerequisites
- [`docs/performance.md`](docs/performance.md): measurements
- [`docs/security.md`](docs/security.md): threat model, authentication, Git, secrets, upstream tokens, what is not protected
- [`docs/backup-restore.md`](docs/backup-restore.md): backup contents, consistency, restore, credentials
