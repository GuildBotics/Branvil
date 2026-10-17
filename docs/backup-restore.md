# Backup and restore

```sh
# On the Hub machine, while the service runs (the owner's token from the data directory):
branvil backup --to /srv/backups/branvil-2026-10-14

# Anywhere, the service need not run; the target must be a new directory:
branvil restore --from /srv/backups/branvil-2026-10-14 --data-dir /srv/branvil-restored
branvil admin check --data-dir /srv/branvil-restored
branvil serve --data-dir /srv/branvil-restored
```

`branvil backup` asks the running service (`POST /api/v1/admin/backup`) to
write the backup. The directory is on the machine the service runs on; a
relative `--to` is made absolute against the current directory of the
`branvil backup` command, so run it on the Hub machine. The service keeps
serving requests while the backup runs.

## What a backup contains

```
branvil-2026-10-14/          (0700)
├── manifest.json            versions, tables, repositories, checksums, exclusions
├── branvil.redb             the database as of one read transaction
├── branvil.toml             the configuration file
├── repos/<repo id>.bundle   `git bundle --all` of each repository with refs
└── ci/logs/, ci/artifacts/  CI logs and artifacts
```

`manifest.json` (abridged):

```json
{"format": 1, "service": "branvil", "branvil_version": "0.1.0", "schema_version": 7,
 "git_version": "git version 2.54.0", "created_at": "2026-10-14T10:41:20.123Z",
 "base_url": "http://127.0.0.1:7878",
 "tables": {"tickets": 12, "pulls": 3, "reviews": 4, "...": 0},
 "repositories": [{"id": "repo_01K…", "path": "acme/demo", "default_branch": "main",
                   "bundle": "repos/repo_01K….bundle",
                   "refs": {"refs/heads/main": "<sha>", "refs/branvil/upstream/main": "<sha>"}}],
 "files": [{"path": "branvil.redb", "bytes": 565248, "sha256": "…"}, "…"],
 "excluded": ["secrets/ (…)", "secrets/accounts/ (…)", "hooks/ and git/ (…)", "Git reflogs (…)"],
 "account_tokens_excluded": [{"member": "aiko", "provider": "github"}]}
```

## How the backup stays consistent

Git refs and redb are separate stores, and a backup must not catch one
operation half in each. While the database is copied and the repositories are
bundled, the service holds the lock that serializes repository creation and
every repository's ref lock (`App::repo_lock`, taken in id order). Every ref
change Branvil makes itself, and every recording of a push, runs under these
locks, so during the backup:

- no merge runs (a merge is journal entry → `git update-ref` → database update
  under the lock), so the database copy and the bundles never disagree about a
  merge; a journal entry caught in the copy is finished or dropped by the
  restored service exactly as after a crash;
- no push is recorded, the default branch does not follow its upstream, no
  repository is created.

The database copy is one redb read transaction written into a new file
(`Store::export_to`), so it is one consistent state even though API writes
(tickets, comments, reviews) continue; anything written after that moment is
not in the backup.

Git itself can still accept a push while the locks are held (its
`post-receive` then waits for the lock). Such a push may be in a bundle but not
in the database: the same state as a push the service had not recorded when it
stopped. The restored service records it at startup (the startup rescan of
`docs/architecture.md`). Each bundle is self-consistent (`git bundle` resolves
the refs and packs exactly their objects), and the manifest lists the refs as
the bundle has them.

CI logs and artifacts are copied after the locks are released and after the
database copy. A file the copied database refers to is therefore in the backup
as long as CI writes a file before it records it, which it does (an artifact
is written, synced, renamed, and only then recorded; a log is a file only); a
file written later is an orphan the restored service removes at start. A log
copied while a job runs may hold a few more lines than the database snapshot
knows of. An attempt that was in progress at backup time is interrupted by
the restored service when its lease runs out (`docs/ci.md`).

## Restore

`branvil restore --from <backup> --data-dir <new dir>`:

1. reads `manifest.json` and refuses another format or a schema version other
   than this build's (there is no conversion);
2. checks every file against its size and SHA-256 (a damaged backup is refused
   before anything is written) and refuses manifest paths that leave the
   backup directory;
3. refuses a data directory that exists and is not empty;
4. copies the database and the configuration, creates each repository as a
   managed bare repository and fetches its bundle (`+refs/*:refs/*`), and
   checks that the refs equal the manifest's;
5. copies CI logs and artifacts;
6. runs the checks of `branvil admin check` (redb structure, every index
   against the records) plus a Git directory for every repository record, and
   exits `1` when one fails.

The restored directory starts like any other with `branvil serve`; at startup
it re-applies Branvil's Git configuration and hooks, finishes or drops pending
journal entries, and records refs the database had not seen. Because a backup
has no `secrets/owner.token`, the first start also writes a new owner token
into the restored data directory, revokes the backed-up one, and starts with no
GUI sessions; enter the GUI again with `branvil open`.

Restoring over the original: stop the original service, restore into a new
directory, and start the restored one with the same `listen` and `base_url`
(they come from the restored `branvil.toml`), so every URL recorded in
GuildBotics stays valid. Two services on the same base URL cannot both listen.

## Credentials

| What | In the backup | Why |
| --- | --- | --- |
| Member tokens | Yes, as SHA-256 hashes in the database | The same tokens keep working after a restore, except the owner's token file's. A hash does not give the token, but protect the backup like the data directory (it is created `0700`). |
| The owner's token (`secrets/owner.token`) | No | The first start of the restored service writes a new one and revokes the old. |
| GUI sessions | Their records, but they do not survive | The new owner token's start deletes every session. |
| Members' upstream credentials (`secrets/accounts/`: tokens, GitHub App keys) | **No** | A credential for GitHub is a secret of another system; a backup copied to another disk or machine should not carry it. After a restore the accounts (provider, kind, and login) are there, but polling reports that the credential is not on this machine and nothing is read or sent until it is set again (`PUT …/members/{name}/accounts/{provider}`). `branvil backup` and `branvil restore` list these accounts. |
| `secrets/cursor.key` | No | A new key is made at the first start; list cursors handed out before become `invalid_cursor`, which clients recover from by starting the list again. |
| The hook secret | No | It is made per process. |

## Not in a backup

- Git reflogs and repository-local Git configuration (Branvil re-applies its
  own configuration at every start).
- `hooks/` and `git/` (written at every start).
- Anything other than `ci/logs` and `ci/artifacts` under `ci/` (caches and job
  work areas are disposable).
- Workspaces, tickets, pull requests, reviews, and boards are in the database, so they are
  in a Branvil backup; the upstream host has only what was published there
  (`docs/security.md`).

## Tested

`tests/backup.rs` creates a workspace and a repository with a push, a ticket on the workspace's board, a pull
request that closes it, a review, a conversation comment, an inline thread with
a reply, and an upstream (a GitHub stand-in) read with a member's account and
its token; it backs the service up while it runs, checks that the token is in
no file of the backup, that a damaged copy is refused, restores into another
directory, runs `branvil admin check`, starts the restored service on the
original address, and checks the refs (`git ls-remote`), the ticket and its
lane, the pull request and its ticket link, the review, the comments and
thread, the ticket's linked pull requests, the board card, the member's
account (its login, while polling reports the credential missing), and that a new push is
recorded. `tests/ci.rs`
(`ci_records_logs_and_artifacts_survive_a_backup_and_restore`) backs up a
service with a finished CI attempt, restores it, and checks the checks, the
job log, and the artifact download.
