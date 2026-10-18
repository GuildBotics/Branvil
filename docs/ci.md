# CI

Branvil runs a small CI of its own: a pipeline file in the repository, a
persistent queue in the service, and a runner process that executes each job
in a throwaway microVM. Its results are the checks of `GET
/api/v1/repos/{repo}/pulls/{n}/checks` and the merge gate (every required check
must have succeeded on the exact head).

```
git push ─▶ branvil serve ─ register run (pipeline from the commit) ─────────▶ redb queue
                 ▲                                                               │ claim (lease)
                 │  heartbeat · log · artifacts · finish                         ▼
                 └──────────────────────────────────────────────── branvil runner ─ msb run ─▶ microVM
```

## The pipeline: `.branvil/ci.toml`

```toml
version = 1                       # optional; only 1 exists

[[jobs]]
name = "test"                     # [A-Za-z0-9_.-]{1,64}, not "pipeline", unique
image = "docker.io/library/alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8"
commands = ["./scripts/test.sh", "make lint"]   # 1-64 commands, each run by /bin/sh -c
timeout_secs = 600                # 1-86400, default 600
cpus = 1                          # 1-64, default 1
memory_mib = 1024                 # 128-65536, default 1024
artifacts = [{ name = "report", paths = ["out/report/", "coverage.xml"] }]   # up to 16
cache = { paths = ["target"], key_files = ["Cargo.lock"] }                    # optional
```

- Commands run in order in `/work` (the commit's tree); the first one that
  fails ends the job with its exit code. The job's environment has only CI
  metadata: `CI=true`, `BRANVIL_CI=true`, `BRANVIL_REPOSITORY`, `BRANVIL_SHA`,
  `BRANVIL_JOB`, `BRANVIL_ATTEMPT`, `BRANVIL_ATTEMPT_ID`. No tokens, no host
  variables.
- Jobs of one run are queued together and run one after another on a
  runner (there are no dependencies between jobs).
- Validation is strict: an unknown key, a value out of range, a path that is
  absolute or contains `..`, an image reference that is not
  `name[:tag][@sha256:<64 hex>]` makes the whole pipeline invalid. An invalid
  pipeline is shown as a failing check named `pipeline` whose details give the
  reason; it never yields success.
- Pin images by digest. The runner never pulls during a job (pull policy
  `never`): images enter the runner's cache only through `branvil runner pull`
  or Settings → CI (pull from a registry, or load a `docker save` / OCI
  archive from a file on the Branvil machine).
- An image pulled by digest is pinned as it was pulled (`name@sha256:<digest>`).
  One loaded from an archive (or pulled by tag) is known to the cache by its
  tag only, so it is pinned as `name:tag@sha256:<configuration digest>`: the
  configuration digest survives `docker save` and loading, the manifest digest
  does not. The runner runs it by the tag once the image the cache holds under
  that tag has that digest, and concludes `startup_failure` when it holds
  another one. Pulls and loads (Settings → CI, `branvil runner pull`) hold a
  lock of the image cache (a file of `MSB_HOME`) exclusively, and a job holds
  it shared from checking its image until its VM has started, so a load that
  re-points the tag waits for the jobs that are starting and never changes
  the image a checked job runs; a job that waits for a pull or a load says
  so in its log and can be cancelled meanwhile (`msb` used by hand does not
  take the lock).
  Settings → CI shows the reference to write for every cached
  image. (A runner started by hand on another machine loads such an archive
  with `msb load` under its own `MSB_HOME`.)

## Trust: where the pipeline comes from

Every commit runs its own pipeline: a tip of the default branch and the head
of an open pull request alike read `.branvil/ci.toml` from that commit. A pull
request that changes the pipeline runs the changed pipeline as its checks, so
a mistake in it shows before the merge. A merge is a fast-forward, so the
default branch's new tip keeps the run the head already has.

A pull request can therefore change its own jobs, but not the repository's
settings, which only the owner changes (`PATCH /api/v1/repos/{repo}`):

- **Required checks.** Each required check needs a successful job of that
  name in the head's own run. A head whose pipeline drops or breaks one cannot
  be merged or published; a head that weakens what one does is caught by
  reviewing its diff (below).
- **Network access** (`ci_network`). Whatever the pipeline says, a job has no
  network unless the owner allows it for the repository.
- **What a job runs on.** A job runs in a microVM with no secrets, only images
  already in the runner's cache, and the runner's limits; a pull request's run
  never writes the protected cache ("Dependency cache"). Settings → CI lists
  the images of the default branches' pipelines; a pull request that names
  another image concludes `startup_failure` with the reason until the owner
  pulls or loads it.

So a change to `.branvil/ci.toml` in a pull request changes that pull request's
own checks, and is reviewed as part of its diff: the owner reads it before
merging or publishing, as with any other change.

## Runs, attempts, and states

A **run** is the CI of one commit of one repository; an **attempt** is one try
of one job (`att_<ULID>`, numbered 1, 2, … per job). Checks show the latest
attempt of each job.

| Attempt status | Conclusion (when completed) |
| --- | --- |
| `queued` → `in_progress` → `completed` | `success`, `failure` (a command exited non-zero), `timed_out`, `cancelled`, `startup_failure` (the commands never started), `skipped` (accepted from a runner, not produced by this one) |

What registers a run:

- a push of the default branch or of the head of an open pull request (after
  the push is recorded, `Ci::on_ref_changes`), opening or reopening a pull
  request, and a merge;
- the reconciler (at start, before the service listens, and every minute),
  which registers every default-branch tip and open pull request head again:
  a crash between recording a push and registering its run, or a Git failure
  while reading its pipeline, is repaired, and until then the checks say "No
  CI run is registered for this commit yet" (`queued`, never `no checks`).

A registration always reads the commit's own pipeline. When a run was recorded
with another one (a run of schema version 7 read its pull request's base),
the run takes it: jobs whose definition changed, or that are new, get a new
attempt. Checks show only attempts of the run's current job definitions; an
attempt of a job the pipeline dropped or defined differently stays on its job
page as history and never counts as a check.

Rapid pushes: when a branch moves from commit A to B and no other branch still
wants A, A is **superseded**: its queued attempts are concluded `cancelled`
("Superseded: …"), its running attempts are asked to stop (the runner sees
`cancel: true` on its next heartbeat), and any result a runner still reports
for A is recorded as `cancelled`. Results only ever describe their own commit,
so nothing of A can count for B. A commit that becomes a tip again gets fresh
attempts.

Leases and interruptions:

- Claiming an attempt gives the runner a lease (`ci.lease_secs`, default 60 s),
  renewed by heartbeats every third of its length. Every runner call carries
  the lease; one that is not the attempt's current, unexpired lease (another
  runner's, an expired one, one of a cancelled or re-queued attempt) is
  `409 conflict` and changes nothing.
- An expired lease (the runner died, lost the service, or the service was
  down) **interrupts** the attempt: it is concluded `cancelled` with the
  details "Interrupted: …", and, the first time for that job and only when it
  was neither cancelled nor superseded, a new attempt is queued. A second
  interruption is final ("Interrupted twice …; retry the job").
- A restart of the service is handled the same way: leases are stored, so a
  runner that keeps working through a short restart keeps its attempt, and
  attempts whose lease ran out while the service was down are interrupted by
  the first sweep after start.
- A runner that cannot renew its lease before it expires stops the job and
  reports nothing.

No runner, no executor:

- When no runner has contacted the service for `ci.runner_absent_secs`
  (default 120 s), queued attempts older than that are concluded
  `startup_failure` ("No CI runner has contacted Branvil …").
- A runner whose executor is unusable still claims attempts and concludes each
  `startup_failure` with the reason (for example "No usable msb 0.6.17
  found …"), so the problem is visible on the checks. Nothing ever runs on the
  host instead.

Cancel and retry: `POST …/jobs/{id}/cancel` concludes a queued attempt
`cancelled` at once and asks a running one to stop; `POST …/jobs/{id}/retry`
queues attempt n+1 of the latest completed attempt, with the same job
definition (the pipeline is not read again).

## Limits

| Limit | Default | Where |
| --- | --- | --- |
| queued attempts | 256 (`ci.queue_max`) | a run whose jobs do not fit **waits**, visibly ("Waiting for room in the CI queue"), and is admitted in order as room frees up; a retry into a full queue is refused with `503 unavailable` |
| log per attempt | 16 MiB (`ci.log_max_mib`) | then one truncation notice and nothing more; one log request is at most 1 MiB |
| one artifact | 64 MiB (`ci.artifact_max_mib`) | `413 too_large`; at most 16 artifacts per attempt, zip only, names `[A-Za-z0-9_.-]` declared by the job |
| artifacts per attempt (before zipping) | 64 MiB (`ci.executor.artifacts_max_mib`) | runner side |
| job cpus / memory / timeout | 4 / 4096 MiB / 3600 s (`ci.executor.max_*`) | the runner lowers a job's request to these and says so in the log |
| dependency cache | 2048 MiB (`ci.executor.cache_max_mib`) | least recently used entries are removed |
| retention | 30 days (`ci.retention_days`) | logs and artifacts of attempts finished earlier are deleted; the attempt records stay |
| runners per work directory | 1 | a lock file refuses a second runner |

## Logs and artifacts

Logs are files `<data>/ci/logs/<attempt id>.log`, artifacts
`<data>/ci/artifacts/<attempt id>/<name>.zip`; redb records only which
artifacts an attempt has (written after the file is in place). Files that
belong to no attempt are removed at start (orphan collection). An artifact of
a commit is downloaded with `GET …/commits/{sha}/artifacts/{name}` (the newest
attempt of the commit that has it).

The runner collects artifact paths from the work directory without following
symbolic links: a link is skipped and noted in the log, a path through a link
is refused, and nothing outside the work directory is read.

## Dependency cache

`cache.paths` are restored into the work directory before the job and saved
after a successful job. The key is the SHA-256 of the repository, the job
name, the image, the cached paths, the content of `cache.key_files`, and the
**scope**: `protected` for runs a protected branch asked for, `branch:<name>`
otherwise. A run reads its own scope and, when it is not protected, the
protected scope too, but writes only its own: a pull request cannot plant a
cache entry that the default branch would use. Copies never write through a
symbolic link at the destination. The cache lives in
`<runner work dir>/cache/`.

Restoring and saving both explicitly record the last-use time. Eviction removes
the oldest timestamps first. For equal timestamps, the entry just saved is
removed last; other entries are removed in cache-key order, regardless of size.
The byte limit still applies to the saved entry: an entry larger than the limit
is removed too.

## The runner

`branvil runner` is a separate process (the same binary). It talks to the
service only over the runner API with the token of a member of kind `runner`;
it never opens the database or a repository (the source arrives as a tar made
by the service with `git archive`).

```sh
# once, on the Hub machine (the owner's token from the data directory): create the runner member, print its token
branvil runner register runner1 --url http://127.0.0.1:7878

# once per image the pipelines use
branvil runner pull docker.io/library/alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8

branvil runner doctor                                          # which msb, executor ready?
BRANVIL_TOKEN=<runner token> branvil runner --url http://127.0.0.1:7878
```

Or let the service supervise one: `[ci] runner = "auto"` in `branvil.toml`
makes `branvil serve` start `branvil runner` as a child (member
`branvil-runner`, a token minted at every start and passed through the
environment; the previous automatic token is revoked) and restart it when it
exits. The child reads the service's configuration file for its
`[ci.executor]` and `[ci.microsandbox]` settings.

The owner changes the mode and the job limits under Settings → CI
(`docs/api.md`, "CI settings") without restarting the service: they are
written to `branvil.toml` (the rest of the file is kept as it is), switching
the mode off stops the child, and new limits start it again; the settings
already in effect change nothing. Stopping or restarting the child interrupts
the attempt it is running, which is queued once more (see "Leases and
interruptions"); the service then removes the microVM the child left. The
child refuses to start when it cannot read the configuration file. The image
cache Settings → CI
manages is the supervised runner's (`[ci.microsandbox] home`); a runner
started by hand on another machine keeps its own, filled with
`branvil runner pull`.

Per job the runner:

1. checks the executor (`msb doctor`, cached) and that the image is cached;
2. creates `<work dir>/jobs/<attempt id>/work`, unpacks the source there,
   creates the empty `<work dir>/jobs/<attempt id>/log` and its log file
   `out` (opened once, before the VM exists), and checks that no component
   below the runner's directory is a symbolic link;
3. restores the cache, then runs
   `msb run --name bv-job-<attempt> --label branvil.runner=<runner> --label branvil.job=<attempt> --pull never --cpus N --memory NM --mount-dir <work>:/work --mount-dir <log>:/.branvil-log --workdir /work --max-duration <timeout+60>s --no-net <image> -- /bin/sh -c <script>`;
   the script first sends its stdout and stderr to `/.branvil-log/out`;
4. streams that file to the log (read every 50 ms through the handle opened
   in step 2, so a link the job puts there is never followed), stops the VM
   when the timeout passes or the service asks to cancel (killing `msb run`
   ends the VM through its parent watchdog, so every process in it stops),
   keeps what the job wrote up to then, and removes the sandbox;
5. uploads artifacts, saves the cache after a success, reports the
   conclusion, and deletes the work directory.

At start it removes every sandbox labelled with its own name (leftovers of a
runner that was killed) and the old job directories.

## Executor: microsandbox

Why: a microVM per job is a hardware-virtualized boundary (a guest kernel),
stronger than a container sharing the host kernel, and it starts fast (about
0.25 s measured, `docs/performance.md`). GuildBotics already deploys the same
runtime, so no second VM stack is installed.

How it is driven: through the `msb` command line, version **0.6.17 exactly**
(the runner checks `msb --version`). The Rust SDK crate was the first choice,
but its 0.6.17 release cannot be built without downloading a guest-agent
binary at build time, which Branvil's build does not do; the CLI needs no
build-time download and no SDK dependency.

`msb` is looked up in this order, each accepted only with the pinned version:
`ci.microsandbox.msb_path`, GuildBotics' copy
`~/.guildbotics/data/msb/bin/msb`, Branvil's copy `~/.branvil/msb/bin/msb`
(created only by `branvil runner install-msb --from <msb>`). Nothing is
downloaded. Branvil's microsandbox state (`MSB_HOME`: image cache, sandbox
records) is `~/.branvil/msb-home` (`ci.microsandbox.home`), separate from
GuildBotics'. `msb` runs with only `PATH`, `HOME`, `USER`, `LOGNAME`, `TMPDIR`,
`LANG` (and the Windows equivalents) from the host, plus `MSB_HOME` and an
empty `DOCKER_CONFIG` (host registry credentials are never read).

Why the log goes through a file: `msb run` 0.6.17 hands a command's stdout
and stderr over only when the command exits (measured: lines written at 0 s
and at 2 s arrive together when the command ends; `--tty` and `--timeout` do
not change it, and a killed `msb run` delivers nothing). Read from `msb run`,
a running job's log would stay empty, and a job stopped by its timeout or a
cancellation would lose all its output.

What a job gets: the work directory at `/work` and its own empty log
directory at `/.branvil-log` as its only host mounts; no host
home, no Docker socket, no Branvil data, no secrets. Network: denied
(`--no-net`: the guest has an interface, but the host-side network stack
refuses every connection and DNS answers nothing; verified to a public host
name, a public IP, and the host gateway). With `ci_network = true` (the owner
only) a job gets `--net public`: public destinations only, never the host or
private networks; microsandbox's hostname filtering is best effort and not
used as a guarantee.

### Host prerequisites

| OS | Requirement | Status |
| --- | --- | --- |
| macOS, Apple Silicon | Hypervisor.framework (built in) | **verified** on macOS 26.6 (M1 Pro) with msb 0.6.17 |
| Linux | KVM (`/dev/kvm` readable and writable by the runner's user) | unverified |
| Windows | Windows Hypervisor Platform (microsandbox's Windows support is a preview); the work directory must not be under `%LOCALAPPDATA%\Temp`, and a path containing `:` cannot be mounted by `msb run` today, so jobs conclude `startup_failure` there | unverified |

On every OS: `msb doctor` (run by `branvil runner doctor`) must pass.

## Testing

- `src/ci/queue.rs`, `src/ci/mod.rs`: the state machine and the checks, on a
  real redb file.
- `tests/ci.rs`: a real service and real Git, with the test acting as the
  runner: a head runs its own pipeline and cannot drop a required check, rapid
  pushes supersede, late and forged results are refused, required checks are
  never success or "no checks" when unregistered, pending, failed, cancelled,
  invalid, or without a runner; leases expire and re-queue once; a restart
  interrupts orphaned attempts; a merge needs success on the exact head; a run
  of schema version 7 that holds its base's pipeline takes the head's own
  after `branvil admin upgrade`.
- `src/runner/mod.rs`: the runner loop against an in-process service with a
  fake executor that starts no process (results, cancellation, an unusable
  executor).
- `tests/real_executor.rs` (opt-in, `BRANVIL_REAL_EXECUTOR=1`): real microVMs.
