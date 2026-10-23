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
image = "docker.io/library/alpine:3.22"   # [registry/]name[:tag], no digest
commands = ["./scripts/test.sh", "make lint"]   # 1-64 commands, each run by /bin/sh -c
timeout_secs = 600                # optional, 1-86400; default the runner's time limit
artifacts = [{ name = "report", paths = ["out/report/", "coverage.xml"] }]   # up to 16
cache = { paths = ["target"], key_files = ["Cargo.lock"] }                    # optional
```

- Commands run in order in `/work`; the first one that fails ends the job
  with its exit code. The job's environment has only CI metadata (besides
  the image's own variables and `HOME`): `CI=true`, `BRANVIL_CI=true`,
  `BRANVIL_REPOSITORY`, `BRANVIL_SHA`, `BRANVIL_JOB`, `BRANVIL_ATTEMPT`,
  `BRANVIL_ATTEMPT_ID`. No tokens, no host variables. See "The job's
  environment" for `/work`, the user, and `/tmp`.
- Jobs of one run are queued together; a runner runs queued jobs, of any
  run, up to `ci.executor.max_jobs` at once, each in a microVM of its own
  (there are no dependencies between jobs).
- Validation is strict: an unknown key (`cpus` and `memory_mib` included,
  with a reason of their own: they are the runner's), a value out of range, a
  path that is absolute or contains `..`, an image reference that is not
  `[registry/]name[:tag]` (one with a digest, `@sha256:…`, included) makes the
  whole pipeline invalid. An invalid pipeline is shown as a failing check
  named `pipeline` whose details give the reason; it never yields success.
- What a job does (commands, artifacts, cache, how long it may take) is the
  repository's; what it runs on (the image's version, the CPUs and memory) is
  the owner's. A job runs with the runner's CPUs and memory
  (`ci.executor.max_cpus`, `max_memory_mib`), and with its `timeout_secs`
  lowered to the runner's time limit (`max_timeout_secs`, noted in the log),
  or with that limit when it gives none.
- A pipeline names an image by its tag; which version runs is the image the
  runner's cache holds under that tag. The runner never pulls during a job
  (pull policy `never`): images enter the runner's cache only through
  `branvil runner pull` or Settings → CI (pull a tag from a registry, or load
  a `docker save` / OCI archive from a file on the Branvil machine). To move
  the jobs to a new version, pull or load the same tag again; nothing in the
  pipelines changes. A job whose tag the cache does not hold concludes
  `startup_failure` with the reason.
- A job's log starts with the attempt: its job, commit, executor, image tag,
  CPUs, memory, and time limit (and a note when its `timeout_secs` was
  lowered). Once the runner has checked the image, it writes the
  configuration digest of the image the job runs on in a line of its own
  (`image docker.io/library/alpine:3.22 is sha256:…`) and records it on the
  attempt: the configuration digest survives `docker save` and loading, the
  manifest digest does not. An image the cache does not hold, or whose
  digest `msb` does not report as `sha256:<64 hex>`, ends the job
  `startup_failure` with the reason. Pulls and
  loads (Settings → CI, `branvil runner pull`) hold a lock of the image cache
  (a file of `MSB_HOME`) exclusively, and a job holds it shared from checking
  its image until its VM has started, so a load that re-points the tag waits
  for the jobs that are starting and never changes the image a checked job
  runs (with several jobs at once, a pull or a load waits for a moment when
  no job is starting); a job that waits for a pull or a load says so in its
  log and can be cancelled meanwhile (`msb` used by hand does not take the
  lock). Settings → CI shows the tag to write and the digest of
  every cached image. (A runner started by hand on another machine loads such
  an archive with `msb load` under its own `MSB_HOME`.)

## The job's environment

What a job's commands find, as on common CI services:

- **`/work` is a Git checkout of the commit.** It has `.git` and an index,
  `HEAD` is the commit (detached), and it holds the commit's history and the
  tags whose commits it reaches, so `git ls-files`, `git describe --tags`,
  and version tools that read Git (`hatch-vcs`, `setuptools-scm`) work. It
  has no remote, no credentials, and no Git configuration besides the
  repository's own (no reflog of the checkout either). The files have the
  time of the checkout, not the commit's, and the index compares only times,
  sizes, and modes (`core.checkStat=minimal`): the VM sees other owners and
  inodes than the runner's machine wrote, and `git diff-index --quiet HEAD`
  must not report the checkout modified. Git attributes and ignore rules of
  the runner's machine do not apply. Being a checkout and not an archive,
  `export-ignore` and `export-subst` attributes do not apply either.
- **The commands run as a fixed user that is not root**, uid 1000 and gid
  1000 (`msb run --user 1000:1000`), without capabilities: a file of mode 0
  cannot be read. `/work`, the cache paths restored into it, and the log
  file are presented to the VM as that user's. `HOME` is the home the
  image's passwd gives uid 1000 when that user can write it, and `/tmp/home`
  (created) otherwise; the user may not exist in the image's passwd.
- **`/tmp` is on a disk, not in the VM's memory**: an ext4 disk of 4 GiB of
  the job's own (mode 1777), a named volume `bv-tmp-<runner>-<attempt id>`
  the runner removes after the job. (Without it, microsandbox puts `/tmp` in
  a tmpfs of a quarter of the VM's memory, at most 512 MiB.) The job's disk
  sizes are microsandbox's 4 GiB default, decided together: the VM's root
  disk (Branvil passes no `--root-disk`), `/tmp`, and what the job may add to
  `/work` beyond the checkout and the restored cache (the default quota of a
  mounted directory). They are the runner's, not the pipeline's.

## Trust: where the pipeline comes from

Every commit runs its own pipeline: a tip of the default branch and the head
of an open pull request alike read `.branvil/ci.toml` from that commit. A pull
request that changes the pipeline runs the changed pipeline as its checks, so
a mistake in it shows before the merge. A merge is a fast-forward, so the
default branch's new tip keeps the run the head already has; only the jobs
with a dependency cache run again, as the default branch's, since only a
protected branch's run saves the cache ("Dependency cache"). Their checks show
the new attempt.

A pull request can therefore change its own jobs, but not the repository's
settings, which only the owner changes (`PATCH /api/v1/repos/{repo}`):

- **Required checks.** Each required check needs a successful job of that
  name in the head's own run. A head whose pipeline drops or breaks one cannot
  be merged or published; a head that weakens what one does is caught by
  reviewing its diff (below).
- **Network access** (`ci_network`). Whatever the pipeline says, a job has no
  network unless the owner allows it for the repository.
- **What a job runs on.** A job runs in a microVM with no secrets, the
  runner's CPUs and memory, and its time limit capped by the runner's; its
  image is the version the owner put in the runner's cache under the tag the
  pipeline names (a pipeline cannot name a digest), and a pull request's run
  never writes the cache ("Dependency cache"). Settings → CI lists
  the images of the default branches' pipelines; a pull request that names
  another image concludes `startup_failure` with the reason until the owner
  pulls or loads it.

So a change to `.branvil/ci.toml` in a pull request changes that pull request's
own checks, and is reviewed as part of its diff: the owner reads it before
merging or publishing, as with any other change.

## Runs, attempts, and states

A **run** is the CI of one commit of one repository; an **attempt** is one try
of one job (`att_<ULID>`, numbered 1, 2, … per job). Checks show the latest
attempt of each job: a pull request's page shows its head's, a commit's page
that commit's, and a repository's page its default branch's tip
(`GET …/pulls/{number}/checks`, `GET …/commits/{sha}/checks`).

| Attempt status | Conclusion (when completed) |
| --- | --- |
| `queued` → `in_progress` → `completed` | `success`, `failure` (a command exited non-zero), `timed_out`, `cancelled`, `startup_failure` (the commands never started), `skipped` (accepted from a runner, not produced by this one) |

What registers a run:

- a push of the default branch or of the head of an open pull request (after
  the push is recorded, `Ci::on_ref_changes`), opening or reopening a pull
  request, and a merge;
- the reconciler (at start and every minute), which registers every
  default-branch tip and open pull request head again:
  a crash between recording a push and registering its run, or a Git failure
  while reading its pipeline, is repaired, and until then the checks say "No
  CI run is registered for this commit yet" (`queued`, never `no checks`).

A registration always reads the commit's own pipeline, and queues the jobs
that have no current attempt: the latest attempt of that very job
definition. When the run holds another pipeline (one an upgrade could not
verify, or one an older parser read from the same file), it takes the
commit's, and its unfinished attempts of another definition are
stopped. Checks and retries count only
current attempts; an attempt of a job the pipeline dropped or defined
differently stays on its job page as history, never counts as a check, and
cannot be retried.

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
| job cpus / memory | 4 / 4096 MiB (`ci.executor.max_cpus`, `max_memory_mib`) | every job runs with these |
| jobs at once | 1 (`ci.executor.max_jobs`) | per runner; the runner claims a job only when it can start it, so the others wait in the service's queue. Jobs at once use that many times the CPUs and memory, and the disk of that many work directories (each with its restored cache, and a copy of it in the cache while it is saved) |
| job time limit | 3600 s (`ci.executor.max_timeout_secs`) | the limit of a job that gives no `timeout_secs`; a larger `timeout_secs` is lowered to it, and the log says so |
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

`cache.paths` are restored into the work directory before the job, and saved
after a successful job of a run that a protected branch asked for (its cache
scope is `protected`). When a protected branch comes to want a commit whose
jobs ran for a pull request (a fast-forward merge), the jobs with a cache run
again as its own (one still running for the pull request is stopped), and an
attempt still queued takes its run's scope when a runner claims it. So a merge
costs one more run of every job with a cache (four of the six of Branvil's
own pipeline), and when the branch moves on before they end, they are
superseded and the next commit's run saves the cache.
A pull request's run (`branch:<name>`) restores the
same entries and saves nothing (its log says `cache not saved`): it cannot
plant a cache entry that the default branch would use, and its runs never
push out of the cache the entries every run restores. The key is the SHA-256
of the repository, the job name, the configuration digest of the image the
job runs on (a new version of the tag does not reuse what the previous one
built), the user the job runs as (microsandbox records with each file the job
writes the owner the VM saw, and the copies keep it), the cached paths, and
the content of `cache.key_files`. Copies never write through a symbolic link
at the destination. The cache lives in `<runner work dir>/cache/`.

A job finds no entry (`no cache entry yet`) when no protected branch's run
has saved one for its key yet: its `cache.key_files` differ from those a
protected branch last ran with (a pull request that changes a lock file
builds its dependencies again on every push until a protected branch has run
with that file), its image tag has a new version, or the entry was evicted.
The cache holds one entry per job with a cache of every repository the runner
serves. While a protected branch's run saves the entries of a new lock file,
pull requests still restore the previous ones, so a `cache_max_mib` of twice
their total keeps both until the pull requests catch up.

Restoring and saving both explicitly record the last-use time. Eviction removes
the oldest timestamps first. For equal timestamps, the entry just saved is
removed last; other entries are removed in cache-key order, regardless of size.
The byte limit still applies to the saved entry: an entry larger than the limit
is removed too.

## The runner

`branvil runner` is a separate process (the same binary). It talks to the
service only over the runner API with the token of a member of kind `runner`;
it never opens the database or the service's repositories (the source arrives
as a Git bundle made by the service, which the runner checks out with the
Git of its machine).

```sh
# once, on the Hub machine (the owner's token from the data directory): create the runner member, print its token
branvil runner register runner1 --url http://127.0.0.1:7878

# once per image the pipelines use, and again to move them to a new version of the tag
branvil runner pull docker.io/library/alpine:3.22

branvil runner doctor                                          # which msb, executor ready?
BRANVIL_TOKEN=<runner token> branvil runner --url http://127.0.0.1:7878
```

Or let the service supervise one: `[ci] runner = "auto"` in `branvil.toml`
makes `branvil serve` start `branvil runner` as a child (member
`branvil-runner`, a token minted at every start and passed through the
environment; the previous automatic token is revoked) and restart it when it
exits. The child reads the service's configuration file for its
`[ci.executor]` and `[ci.microsandbox]` settings.

The owner changes the mode and the job resources under Settings → CI
(`docs/api.md`, "CI settings") without restarting the service: they are
written to `branvil.toml` (the rest of the file is kept as it is), switching
the mode off stops the child, and the runner reads the limits from the file
again before every claim, so new limits apply from the next job it starts and
the running jobs keep theirs (a runner started by hand reads its own
configuration file the same way). Stopping or restarting the child interrupts
the attempts it is running, which are queued once more (see "Leases and
interruptions"); the service then removes the microVMs the child left. The
child refuses to start when it cannot read the configuration file. The image
cache Settings → CI
manages is the supervised runner's (`[ci.microsandbox] home`); a runner
started by hand on another machine keeps its own, filled with
`branvil runner pull`. A runner started by hand is upgraded together with
the service: the runner API carries the job definition and the image digest
in this version's shapes.

Per job the runner:

1. checks the executor (an accepted `msb`, `git` on `PATH`, `msb doctor`;
   cached), logs the attempt with its
   image tag, CPUs, memory, and time limit, then checks the image the cache
   holds under the tag and logs its configuration digest;
2. creates `<work dir>/jobs/<attempt id>/work`, writes the source's bundle
   next to it and clones the bundle into it (Git with an empty global
   configuration and none of the machine's attributes, no reflog,
   `core.checkStat=minimal`), then removes the clone's remote and the
   bundle; creates the
   empty `<work dir>/jobs/<attempt id>/log` and its log file `out` (opened
   once, before the VM exists), and checks that no component below the
   runner's directory is a symbolic link. Git never runs in the work
   directory once the VM has had it;
3. restores the cache, then runs
   `msb run --name bv-job-<attempt> --label branvil.runner=<runner> --label branvil.job=<attempt> --pull never --cpus N --memory NM --user 1000:1000 --mount-dir <work>:/work:uid=1000,gid=1000 --mount-dir <log>:/.branvil-log:uid=1000,gid=1000 --mount-named bv-tmp-<runner>-<attempt>:/tmp:kind=disk,size=4G --workdir /work --max-duration <timeout+60>s --no-net <image> -- /bin/sh -c <script>`
   (`N` and `M` the runner's CPUs and memory);
   the script first sends its stdout and stderr to `/.branvil-log/out` and
   sets `HOME` ("The job's environment");
4. streams that file to the log (read every 50 ms through the handle opened
   in step 2, so a link the job puts there is never followed), stops the VM
   when the timeout passes or the service asks to cancel (killing `msb run`
   ends the VM through its parent watchdog, so every process in it stops),
   keeps what the job wrote up to then, and removes the sandbox (by its
   `branvil.job` label) and then its `/tmp` volume (which outlives the
   sandbox), warning only about one that exists and could not be removed;
5. uploads artifacts, saves the cache after a success of a protected
   branch's run, reports the
   conclusion with the image's configuration digest, and deletes the work
   directory.

The log says how long each step took, so the time of a job can be read off
it: how long the attempt waited in the queue (`queued for … s`, from its
registration to the claim), then `source fetched in … ms` (with the bundle's
size), `checked out in … ms`, `cache restored in … ms` (with the bytes
copied), `sandbox started in … ms` (the spawn of `msb run` → the script's
first line), `commands ran for … ms`, `sandbox removed in … ms`,
`artifacts handled in … ms` (a job that declares artifacts), and `cache saved
in … ms` (with the bytes saved). `peak memory of msb run and its VM: … MiB` is
the largest resident memory of `msb run` and every process under it (the VM
among them), read with `ps` every second while the job runs; the VM's grows
with the guest memory the job touches and does not shrink, so the last samples
hold the peak. A host without `ps` gives no such line.

At start it removes every sandbox labelled with its own name and every volume
named `bv-tmp-<its name>-<attempt id>` (leftovers of a runner that was killed),
and the old job directories.

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
directory at `/.branvil-log` as its only host mounts, and a disk of its own at
`/tmp`; no host home, no Docker socket, no Branvil data, no secrets. Network: denied
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

On every OS: `msb doctor` (run by `branvil runner doctor`) must pass, and
`git` must be on the runner's `PATH` (it checks the source out; the executor
reports itself unavailable without it, as without `msb`).

## Testing

- `src/ci/queue.rs`, `src/ci/mod.rs`: the state machine and the checks, on a
  real redb file.
- `tests/ci.rs`: a real service and real Git, with the test acting as the
  runner: a head runs its own pipeline and cannot drop a required check, rapid
  pushes supersede, late and forged results are refused, required checks are
  never success or "no checks" when unregistered, pending, failed, cancelled,
  invalid, or without a runner; leases expire and re-queue once; a restart
  interrupts orphaned attempts; a merge needs success on the exact head.
- `src/runner/mod.rs`: the runner loop against an in-process service with a
  fake executor that starts no VM (results, cancellation, an unusable
  executor, the runner's CPUs, memory, and time limit, the image's digest on
  the attempt and in the log, a work directory that is a checkout of the
  commit with the tags it reaches and no remote, up to `max_jobs` jobs at
  once with the limit read before every claim, a refused token that
  lets the running jobs end before the runner stops, and a cache that only
  a protected branch's run saves).
- `src/runner/workspace.rs`: the dependency cache (keys, eviction, and
  restores and saves of one entry at once copying whole entries).
- `src/runner/executor.rs`: `msb` run as a fake script (the user, the mounts
  and `/tmp` volume a job gets, their removal, and the sweep of this runner's
  leftovers only).
- `tests/real_executor.rs` (opt-in, `BRANVIL_REAL_EXECUTOR=1`): real microVMs
  (no network or host files; a checkout of the commit at `/work`, a non-root
  user that cannot read a file of mode 0, `/tmp` on a disk, the cache
  restored in the next attempt, nothing left behind).
