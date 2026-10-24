---
name: resolve-dependency-alerts
description: Resolve Branvil's open dependency alerts unattended by raising the vulnerable packages in `Cargo.lock` and `web/package-lock.json` to their patched versions, and create a PR.
---

# Resolving the dependency alerts

Invoke with `$resolve-dependency-alerts` or "依存アラートを解消して". Read the
open dependency alerts, raise the packages, run the checks of
[`AGENTS.md`](../../../AGENTS.md) ("Checks (as in CI)"), and create a PR
without asking for confirmation. Invoking this workflow includes committing and
pushing its branch and creating or updating its PR. Do not merge it.

Branvil's libraries are locked in `Cargo.lock` and `web/package-lock.json`.
The code host reports the vulnerable ones as dependency alerts. This
repository follows an upstream host (`docs/architecture.md`, "Upstream"), so
its alerts are the upstream's: an alert closes only once the fix is on the
upstream's default branch and the upstream has re-evaluated it.

## Scope

- Fetch the alerts on every run. Decide what is needed against the remote
  default branch, which is what ships: a fix that only the update branch
  has still needs this run's PR. Finish without file changes or a PR only
  when no alert is open, or the default branch already has every fix that can
  be made.
- One PR for the whole run, separate from any other change. Use the fixed head
  branch `codex/resolve-dependency-alerts` and the remote default branch as
  base. Reuse an open PR with that head and base, preserving others' changes.
  Do not infer ownership from titles or dates, or create a duplicate PR.
- An alert that cannot be resolved stays open with its reason; resolve the
  others.
- Only the manifests (`Cargo.toml`, `web/package.json`) and the locks change.
  Not touched: the sources (`src/`, `web/src`, `tests/`), and `@mantine/core`
  and `@mantine/hooks`, whose version `AGENTS.md` keeps equal to GuildBotics
  Desktop's ("The GUI looks like GuildBotics Desktop"); an alert of one of
  those stays open for a human.

## 1. Isolate the run

Work in a dedicated worktree or checkout on `codex/resolve-dependency-alerts`.
For an open PR, continue its branch, first merging the current remote default
branch into it, so that what it compares and changes starts from today's
default branch (a fix merged there since makes an alert nothing to do instead
of a conflict); if no open PR exists, base the run on the current remote
default branch. A branch left by a PR closed without a merge is continued the
same way when it holds only earlier runs of this workflow; a new PR then
carries its fixes (section 5). When it holds anything else, stop and report it
(the member cannot force-push to replace it). Keep the checkout the
running service updates itself from unchanged.

## 2. Read the alerts

Read every open alert, following `continuation` with the same parameters
until it is `null`. An unattended run is a GuildBotics member, which reads
them with its member CLI:

```bash
guildbotics member repo-info read --person <person> --resource dependency_alerts \
  --subject GuildBotics/Branvil --params '{"state":"open","page_size":100}' \
  [--continuation <value>]
```

A failed read is a failure, never zero alerts: report it and change nothing.
Dismissed alerts are a human decision and are not touched.

## 3. Raise the packages

Each alert names the lock in `manifest_path`, the `package`, the
`affected_versions` range, and the `patched_version`. A lock can hold several
versions of one package (`getrandom` 0.2 and 0.4 side by side), so work per
locked version: match each alert to the versions of its package the lock holds
within its `affected_versions`, and give each such locked version the highest
`patched_version` among the alerts that match it as its target. A locked
version that no alert matches is left alone.

First look at the remote default branch's lock: an alert none of whose
versions there is within its `affected_versions` is fixed where it ships; the
upstream has not re-evaluated it yet. Otherwise look at the update branch: when
it already holds every target (an earlier run), this run's PR carries the
fix, and the report says it awaits that PR's merge and its publishing to the
upstream. Otherwise raise each locked version to its target:

| `manifest_path` | Raise it with |
| --- | --- |
| `Cargo.lock` | `cargo update -p <package>@<locked_version> --precise <target>` (the locked version tells two versions of the package apart). When cargo cannot write its registry, point `CARGO_HOME` at a writable directory |
| `web/package-lock.json` | In `web`: a transitive dependency with `npm update <package>` (`npm install <package>` would add it to `package.json` as a direct dependency); a direct one with `npm install <package>@<target> --save-exact`, which keeps `package.json`'s exact pins (every dependency there is pinned exactly; without `--save-exact` npm writes a `^` range). npm cannot pick the target of a transitive dependency: `npm update` takes the newest version the requirers' ranges admit, which can be a later major (a `*` or `>=1 <3` range), so the check below decides |

Then read every version of the package the lock now holds (`cargo tree -i
<package>@<version>` for each version the lock lists, as a bare name is
ambiguous when there are several; `npm ls <package> --all`; or the lock
entries) and compare each with the one it replaced. An alert is resolved only
when each locked version it matched is now at or above that version's target
and in the same major as before (for a `0.x` version, the same minor): moving
to another major is a human decision, whichever version the tool picked. A command that ran is not a fix: npm succeeds without
changing anything when what requires the package does not admit the patched
version, and `npm update` succeeds when it moves past the major. Decide on the
final lock, after every alert of that lock has been raised.

Leave an alert open and report why when:

- it has no `patched_version`;
- its final locked version is not resolved as above: the target is in
  another major, the packages that require it do not admit it, a direct
  dependency's range in `Cargo.toml` or `web/package.json` has an upper bound
  that excludes it, or the tool picked a version in another major;
- it is a package whose version stays equal to GuildBotics Desktop's (Scope);
- the target needs a newer Rust than `rust-version` in `Cargo.toml`
  (cargo says so when it resolves the version): raising `rust-version` is a
  human decision;
- reaching it needs changes beyond the manifests and locks, or the checks fail
  with it;
- its `manifest_path` is not one of the above.

To leave a raised package open again, restore the lock and its manifest from
the branch, then raise the other alerts of that lock again.

After the npm changes, read `npm audit` in `web` to confirm that the raised
packages are no longer reported. It exits non-zero while any finding remains,
including the alerts left open; its exit status is not a check result.

## 4. Checks

Run the checks of `AGENTS.md` ("Checks (as in CI)") that the change concerns:
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test` when `Cargo.lock` changed; `npm ci`, `npm run lint`,
`npm run typecheck`, `npm run test`, and `npm run build` in `web` when
`web/package-lock.json` changed (the build is what the service's self-update
runs; CI does not). Fix nothing in the sources to make a check pass: leave the
alert open instead (section 3), then rerun the affected checks on the final
locks; the PR reports the checks of what it pushes. A Rust build that cannot
finish in the environment's resources is recorded as an unperformed check
(below), not as a failure of the package.

Follow `AGENTS.md` when diagnosing a failure that the change did not cause:
put its complete Issue draft in the PR or final report, without publishing an
Issue or waiting for a reply. If checks cannot run in the available
environment, preserve the patch and record the exact unperformed checks and
reason in the PR.

## 5. Create the PR and report

Commit and push only this run's changes and create or update its PR. Do not
stop at a local diff or ask permission to push or create the PR. An update
branch that holds fixes the default branch lacks always ends with an open PR,
also when this run committed nothing: `member pr create` returns the open PR
of that head and base, or opens one (after a PR closed without a merge, say so
in the body). If the default branch already has every fix that can be made,
finish with the reasons and no PR. If the code host is unavailable, keep the
local changes and report the specific blocker.

The body lists each alert resolved (number, package, old and new version,
lock), awaiting this PR's merge or the upstream's re-evaluation, or left open
with its reason, and the checks
and their results. After the merge, the alerts close once the change is
published to the upstream, merged there, and re-evaluated; the next run reads
what is still open.

Report the alerts resolved, awaiting, and left open, the check results, and the PR link.
When nothing needs changing or attention, do not send a notification if the
runner supports silent completion.
