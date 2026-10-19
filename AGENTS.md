# AGENTS.md

A working guide for AI and automation agents in this repository. The structure
is described in [`docs/architecture.md`](docs/architecture.md), and the list of
documents is under "Documents" in [`README.md`](README.md). This file holds
only the rules that reading the code does not reveal and that hurt when broken.

## Most important rules

- The source code is the source of truth (README and `docs/*.md` are reference
  material).
- Do not read files or directories that `.gitignore` excludes, in particular
  `/data/` (the development data directory, which holds real tokens such as
  `secrets/owner.token`) and `target/`.
- When you change behavior, update the related documents as needed. The HTTP
  API's contract is [`docs/api.md`](docs/api.md), and GuildBotics' `branvil`
  provider relies on it.
- When changing code, give top priority not to the smallest diff but to the
  smallest amount of code after the change. Aim for the clean code the design
  calls for, not an ad hoc patch.
- Do not add code that keeps backward compatibility (migration flags, fallback
  reads, parallel support for old formats, old and new paths side by side
  during a deprecation period). Keeping the code as the design calls for comes
  before keeping redundant code for compatibility. "Important Notice
  (Disclaimer)" in the README declares that incompatible changes happen, and
  in practice the maintainer is the only user.
  - When the stored format changes, raise `SCHEMA_VERSION` in `src/store.rs`
    and replace `src/upgrade.rs` with the upgrade from the previous version
    (`branvil admin upgrade` and `branvil restore` run it). Write no
    conversion from any older version, and read old shapes nowhere else: the
    data directory holds the owner's real data, so a change of the stored
    format must never leave it unreadable.
  - When the HTTP API changes, build no compatibility layer; change GuildBotics'
    `guildbotics/integrations/branvil/` at the same time.
- Judging the meaning of natural language (classifying, deciding whether to
  accept, …) is not Branvil's job. When it is needed, it goes to GuildBotics;
  Branvil does not substitute keyword lists or string matching for it.
- Do not implement across responsibility boundaries (see "Boundaries" below).

## Boundaries

The responsibilities of each module are defined by "Modules" in
[`docs/architecture.md`](docs/architecture.md). Beyond those, these boundaries
hold across modules:

- **Branvil knows nothing of GuildBotics.** The dependency goes one way,
  GuildBotics → Branvil. The only behavior of Branvil that assumes GuildBotics
  is where `branvil runner` looks for msb by default (`~/.guildbotics/data/msb`
  in `src/runner/executor.rs`). GuildBotics is named only in documents and in
  the harness that runs GuildBotics' contract tests against a real service
  (`integration/guildbotics/`, `scripts/guildbotics-contracts.sh`,
  `scripts/e2e-flow.sh`).
- **The upstream sits behind a provider-neutral boundary.** `src/upstream/`
  knows no code host; everything host-specific goes in the provider's
  implementation of `Host` (`src/upstream/host.rs`), today only
  `src/upstream/github.rs`.
- **Branvil listens on loopback only.** Other machines reach it through the SSH
  port forward of `branvil tunnel` (`src/config.rs` refuses any other listen
  address).
- **The HTTP API is the only way into the data.** The GUI, GuildBotics'
  provider, and the `branvil` CLI's client commands all use it. Only
  `branvil init`, `branvil admin check|reindex|upgrade`, and `branvil restore`, run
  with the service stopped, work on the data directory directly.
- **The GUI looks like GuildBotics Desktop.** Keep the Mantine version, the
  theme (`web/src/theme.ts`), and the design tokens and component overrides in
  `web/src/styles.css` the same as GuildBotics Desktop's ("GUI files" in
  `docs/architecture.md`).

## Git and credentials

- Git run by the service goes through `src/git.rs` (it inherits none of the
  service's `GIT_*` and reads no system or global configuration).
- Client Git in tests and scripts must not touch the user's Git configuration
  or credential helpers. Use `git` / `git_as` in `tests/common/mod.rs`; when
  you write the same isolation yourself, set `GIT_CONFIG_NOSYSTEM=1`, an empty
  `GIT_CONFIG_GLOBAL`, `GIT_TERMINAL_PROMPT=0`, and `-c credential.helper=`,
  and pass tokens through `GIT_ASKPASS` and environment variables (never in a
  URL or argv). On macOS the system configuration's
  `credential.helper=osxkeychain` tries to store the token and puts a Keychain
  dialog on the user's screen.
- Do not use real people's names for test members.

## Checks (as in CI)

The same checks as CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)):

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
python3 -m unittest discover -s tests -p test_dev_script.py -v
```

- The integration tests are one test crate (`tests/main.rs`). To run one file:
  `cargo test --test main <file>::` (for example
  `cargo test --test main tickets::`).
- The tests that use real microVMs (`tests/real_executor.rs`) run only with
  `BRANVIL_REAL_EXECUTOR=1`. They are not part of the usual checks.
