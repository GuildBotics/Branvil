# Storage design

Branvil keeps metadata in one redb file (`<data>/branvil.redb`), Git objects in
bare repositories (`<data>/repos/<path>.git`), and CI logs and artifacts in
files under `<data>/ci/` (never in redb). Only the service process opens the database; the
GUI, the GuildBotics provider, and the CLI go through the HTTP API (the offline
`branvil init` / `branvil admin` commands open it only while the service is
stopped, and redb's file lock refuses a second opener).

This document derives the tables from [`port-mapping.md`](port-mapping.md):
for each record it says which operations read and write it, which index serves
which query, and what one write transaction covers. The stored records are not
the API shapes: views are assembled in `src/domain/` from records plus live Git
state (branch tips, diffs), and anything that depends on the caller
(`assignee=me`, `involves=me`) is computed at request time.

## redb settings

| Setting | Value | Why |
| --- | --- | --- |
| redb | 4.4.0 (stable API only; `experimental-api-5` is not enabled) | `Cargo.lock` |
| Durability | redb default (`Immediate`: a commit is on disk when `commit()` returns) | reviews, merges, and ref journal entries must survive a crash |
| Cache | `storage.cache_size_mib`, default 32 MiB | evaluation starting point; it is redb's page cache, not a process limit |
| Writers | one async semaphore permit in front of `begin_write` | at most one blocking thread waits on redb's writer lock |
| Readers | `storage.read_concurrency` (default 8) permits | bounded `spawn_blocking`; the async runtime never blocks on storage |

No transaction is held across Git, network, or user input: a request reads Git
first, then runs one short closure inside `Store::write`, which commits when the
closure returns `Ok` and aborts otherwise.

## Encoding

- Values are JSON (`serde_json`) of the structs in `src/model.rs`. Readability
  over compactness for now.
- Keys are order-preserving tuples (`store::Key`): a string element is its
  bytes with `00` escaped as `00 FF` and a `00 01` terminator; an integer is 8
  bytes big-endian. Element-wise order equals byte order, a prefix of elements
  is a byte prefix (prefix scans), and `("a","b|c")` cannot collide with
  `("a|b","c")`. Numbers never compare as strings (`10` sorts after `9`).
- Paths and member names are case-preserving in records and case-folded
  (`to_lowercase`) in keys, so `Acme/Demo` and `acme/demo` cannot both exist.
- Workspaces and repositories have internal ids (`ws_…`, `repo_…`,
  ULID-based). Keys use ids, never paths; URLs are built from paths and
  the configured `base_url` at response time, so changing `base_url` changes
  every URL Branvil returns (and breaks URLs recorded elsewhere: keep it fixed).
- Entry ids (`cmt_…`, `rev_…`, `ilc_…`, `rct_…`) are opaque; ordering inside an
  item comes from a service-wide sequence counter (`counters["entry"]`), not from
  the id.
- Timestamps are RFC 3339 UTC strings with milliseconds, so string order is
  time order (used in the closed-at and patrol orderings).

## Schema version

`meta["schema_version"]` is written on creation (currently `7`: members are
the owner, agents, and runners, whose kind alone decides what they may do, and
have accounts on upstream hosts, each a token or an app; a ticket has at most one assignee and may name
an upstream issue; tickets belong to repositories, which hold their labels and
may have an upstream; repositories belong to workspaces, each with one board;
a pull request may be published upstream). A
database of any other version is refused at open; there is no migration, so a
data directory of another version is not readable by this build.

## Primary tables

| Table | Key | Value | Written by | Read by |
| --- | --- | --- | --- | --- |
| `meta` | `(name)` | schema version | open | open |
| `counters` | `(name)` | `u64` | ticket numbers `ticket:<repo>`, PR numbers `pull:<repo>`, `entry` sequence | allocation in the same txn as the insert |
| `members` | `(fold(name))` | member, kind (`owner`, `agent`, `runner`), accounts on upstream hosts (provider, login; the token is a file) | owner API, `init`, automatic runner | auth, kind checks, upstream |
| `tokens` | `(token id)` | member, label, SHA-256 of the secret, last use | token API | auth (by id, constant-time hash compare) |
| `sessions` | `(sha256(session id))` | member, CSRF secret, expiry | entry-code exchange (expired rows pruned there); all deleted when the owner's token file is replaced | cookie auth |
| `workspaces` | `(workspace id)` | name | workspace API | repository creation, cards, closed |
| `repos` | `(repo id)` | path, workspace id, default branch, protected branches, required checks, `ci_network`, labels, upstream (provider, repository), revision | repo API | everything under a repository |
| `tickets` | `(repo id, n)` | ticket incl. its workspace id, its one assignee with `assigned_at` (or none), labels, upstream issue (or none), lane/rank on the workspace's board (or none), state, revision | ticket API, merge | `read(tickets)`, candidates |
| `comments` | `("ticket", repo id, n, seq)` / `("pull", repo id, n, seq)` | comment with its reactions | comment and reaction API | comments, cards, activity |
| `pulls` | `(repo id, n)` | pull request record (links, reviewers, reviewed_by, draft, state, merge data, and once published: the host's pull request, what was last sent to it, the host's checks of the pushed head) | PR API, merge, publish, the upstream poller, push recording (`updated_at`) | PR reads, patrol, the upstream poller |
| `reviews` | `(repo id, n, seq)` | review with reactions | review API | reviews, activity |
| `threads` | `(repo id, n, root seq)` | path, line, side, commit, resolved | inline-comment API | threads |
| `inline_comments` | `(repo id, n, root seq, seq)` | thread comment with reactions | inline/reply API | thread comments (contiguous per thread) |
| `ref_snapshots` | `(repo id, refname)` | last recorded object | `refs::record_refs` | push recording, startup rescan |
| `ref_journal` | `(journal id)` | repo, ref, expected old/new, operation | merge (before `update-ref`) | completion, startup recovery |
| `idempotency` | `(fold(member), key)` | request hash, status, response body, time | API middleware | API middleware |
| `ci_runs` | `(repo id, sha)` | branches that want CI of the commit, pipeline source (branch, tip), pipeline (absent / invalid + message / job definitions), cache scope, waiting, superseded by | ref recording (`Ci::on_ref_changes`), PR open/reopen, reconciler, queue admission | checks, claims, retries |
| `ci_attempts` | `(attempt id)` | job definition (snapshot), number, queue sequence, status, conclusion, details, SHA-256 of the lease, lease expiry, cancel requested, runner, cache scope, network, times, interrupted/superseded flags, artifact names and sizes, files pruned | registration, claim, heartbeat, finish, cancel, retry, sweeps | checks, job API, runner API, artifacts |

Each comment, review, and inline comment is its own record, so adding one never
rewrites a history; a reaction rewrites only its target entry.

## Derived indexes

Every index entry of a record comes from one function in `src/model.rs`
(`ticket_index`, `pull_index`, …), used both when the record is written
(`put_indexed` diffs the old and new entries in the same transaction) and by
`branvil admin check` / `branvil admin reindex`. The two cannot drift apart.

| Index | Key → value | Serves (port-mapping) |
| --- | --- | --- |
| `ix_paths` | `("workspace"|"repo", fold(name or path))` → id | name and path lookup; unique names and paths |
| `ix_member_tokens` | `(member, token id)` | token list/revoke |
| `ix_cards` | `(workspace id, lane index, rank, repo id, n)` | `candidates` (`GET workspaces/{w}/board/cards?lane=…`), card order, `place` neighbours |
| `ix_ticket_labels` | `(repo id, label, n)` | `?label=` lists; consistency check for concurrent label changes |
| `ix_closed_tickets` | `(workspace id, closed_at, repo id, n)` | `closed_since` for tickets |
| `ix_entries` | `(entry id)` → `{kind, owner, record key}` | reactions, replies: an id is used only with its owning item (`owner` must match) |
| `ix_open_pulls` | `(repo id, head, base)` → number | unique open (head, base) on create and reopen; `open_pull_requests`; pushes → PR activity; the owner's `GET /pulls?involves=me` |
| `ix_involved` | `(member, repo id, n)` for open PRs (author, reviewers, reviewed_by) | `pull_request_candidates` (`GET /pulls?involves=me`) |
| `ix_ticket_pulls` | `(repo id, n, repo id, pr n)` | `ticket_pull_requests`, card `pull_requests` |
| `ix_closed_pulls` | `(repo id, closed_at, n)` for closed PRs | `closed_since` for pull requests |
| `ix_idempotency_time` | `(created_at, member, key)` | expiring idempotency keys after 24 hours |
| `ix_ci_sha` | `(repo id, sha, job, attempt number)` → attempt id | checks (latest attempt per job), next attempt number, artifacts of a commit |
| `ix_ci_pending` | `(queue sequence)` → attempt id, queued attempts only | claim (oldest first), queue length and bound, no-runner sweep |
| `ix_ci_active` | `(attempt id)`, in-progress attempts only | lease-expiry sweep (also the orphans found after a restart) |
| `ix_ci_waiting` | `(run sequence)` → run key, runs that did not fit the queue | admission in order when room frees up |

Cards read only the requested lanes' ranges of one workspace's `ix_cards` (the
`done` lane keeps every finished ticket, and candidates ask for `ready` and
`in_progress`); the assignee filter is applied to the tickets of those lanes.
Placing a card (create on a board, move, merge closing a ticket) reads only
the two index entries next to the new position, never the whole lane. Ticket
lists scan one repository's range (or one label's index range). `closed_since`
reads the repository records to find the workspace's repositories, whose pull
requests it lists (GuildBotics calls it to import the board's finished work as
activity, not on its patrol path). No hot path scans a whole table. Lists follow signed cursors: the cursor is the last position
(record key, or sort key for lists sorted in memory) plus an HMAC-SHA256 tag
over the collection, the caller, and every query condition, keyed by
`<data>/secrets/cursor.key`.

## Transaction boundaries (atomicity)

| Operation | One write transaction covers |
| --- | --- |
| create ticket | repository re-read, label check, number allocation, insert, card placement, indexes |
| update ticket | conditional revision check, label add/remove against the stored set, state change, indexes (concurrent label changes are serialized by redb's single writer, so none is lost) |
| create PR | open (head, base) lookup, link validation, number allocation, insert, indexes (concurrent creates make one PR) |
| review / inline comment | insert + PR activity + involvement; the head was read from Git just before, and the write is linearized at that read |
| reaction | read-modify-write of the target entry, after checking the entry belongs to the item |
| merge (DB side) | PR merged/closed, closed-at index, `closes` tickets closed and moved to `done`, journal entry removed |
| push recording | ref snapshot diff, `updated_at` of open PRs whose head moved |
| upstream poll of a pull request | what was sent and read, and a close or reopen the host made; a merge on the host is applied with the merge's own effects (above) in the same transaction |
| CI registration (after push recording, same repository lock; the pipeline was read from Git before) | run created or the branch added to it, queue bound check, every job's attempt queued (or the run marked waiting) |
| CI supersession | the branch removed from the old commit's run; when none is left, its queued attempts cancelled and running ones marked cancel-requested |
| claim | oldest pending attempt → in progress, new lease hash and expiry, pending entry removed, active entry added |
| heartbeat / finish / artifact record | lease checked (current and unexpired) and the attempt updated in the same transaction; `409` writes nothing |
| lease sweep | each expired attempt concluded as interrupted and, the first time, its re-queued successor inserted |

## Files, retention, backups

- Git objects live only in Git. Repositories are never deleted by the API
  (there is no delete endpoint yet).
- The data directory (0700), `ci/logs`, `ci/artifacts`, and `secrets/` (0700;
  `cursor.key` 0600) are created at start.
- CI: `ci/logs/<attempt id>.log` (appended while the job runs, at most
  `ci.log_max_mib`) and `ci/artifacts/<attempt id>/<name>.zip` (written to a
  temporary name, synced, renamed, and only then recorded in `ci_attempts`).
  Logs and artifacts of attempts finished more than `ci.retention_days` ago are
  deleted hourly and the attempt is marked `files_pruned` (its record and
  conclusion stay). At start, files that belong to no attempt (a crash between
  writing and recording, a restored backup taken mid-job) are deleted. Run and
  attempt records are kept; they are small (one per commit and job try).
- Sessions expire (`limits.session_hours`, default 720) and are pruned at
  the next entry-code exchange; idempotency keys after 24 hours; finished journal entries are deleted
  when they complete.
- Members' upstream credentials are `0600` files in `secrets/accounts/`,
  deleted with their account.
- Backups and restores: `docs/backup-restore.md`. The token hashes in the
  database are not secrets by themselves; `secrets/` is not backed up.
