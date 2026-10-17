//! Git child processes. Every command is an argv array, runs with the
//! service's `GIT_*` variables removed and with an empty global configuration,
//! and is bounded in time, output, and concurrency.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;

use crate::config::Layout;
use crate::error::{AppError, AppResult};

pub const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// Largest stdout a service-side command may produce (diffs are the big ones).
const MAX_OUTPUT: usize = 64 << 20;

#[derive(Clone)]
pub struct Git {
    layout: Layout,
    permits: Arc<Semaphore>,
    timeout: Duration,
}

pub struct Output {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
    /// stdout was longer than the cap and was cut.
    pub truncated: bool,
}

impl Output {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Builds a Git command with a sanitized environment. Shared with Smart HTTP.
pub fn base_command(layout: &Layout) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&key);
        }
    }
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", layout.git_config())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    cmd
}

impl Git {
    pub fn new(layout: Layout, concurrency: usize, timeout: Duration) -> Self {
        Self {
            layout,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            timeout,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn repo_dir(&self, path: &str) -> PathBuf {
        self.layout.repo_dir(path)
    }

    pub async fn run(&self, dir: Option<&Path>, args: &[&str]) -> AppResult<Output> {
        self.run_env(dir, args, &[]).await
    }

    /// `run` with extra environment variables for the child only (external
    /// sync passes its askpass settings this way, never through argv).
    pub async fn run_env(
        &self,
        dir: Option<&Path>,
        args: &[&str],
        envs: &[(&str, &str)],
    ) -> AppResult<Output> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| AppError::unavailable("Git is shutting down."))?;
        let mut cmd = base_command(&self.layout);
        if let Some(dir) = dir {
            cmd.arg("--git-dir").arg(dir);
        }
        cmd.envs(envs.iter().copied());
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::internal(format!("cannot start git: {e}")))?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let work = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut out_reader = (&mut stdout).take(MAX_OUTPUT as u64 + 1);
            let mut err_reader = (&mut stderr).take(64 << 10);
            let (a, b) = tokio::join!(
                out_reader.read_to_end(&mut out),
                err_reader.read_to_end(&mut err)
            );
            a.and(b)?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, out, err))
        };
        let (status, mut out, err) = tokio::time::timeout(self.timeout, work)
            .await
            .map_err(|_| {
                AppError::unavailable(format!("git {} timed out.", args.first().unwrap_or(&"")))
            })?
            .map_err(|e| AppError::internal(format!("git i/o: {e}")))?;
        let truncated = out.len() > MAX_OUTPUT;
        out.truncate(MAX_OUTPUT);
        Ok(Output {
            status: status.code().unwrap_or(-1),
            stdout: out,
            stderr: String::from_utf8_lossy(&err).into_owned(),
            truncated,
        })
    }

    /// Runs a command that must succeed and returns its trimmed stdout.
    pub async fn run_ok(&self, dir: Option<&Path>, args: &[&str]) -> AppResult<String> {
        let out = self.run(dir, args).await?;
        if !out.ok() {
            return Err(AppError::internal(format!(
                "git {} failed: {}",
                args.join(" "),
                out.stderr.trim()
            )));
        }
        Ok(out.text().trim().to_string())
    }

    pub async fn version(&self) -> AppResult<String> {
        self.run_ok(None, &["version"]).await
    }

    /// Creates an empty bare repository owned by Branvil.
    pub async fn init_bare(&self, dir: &Path, default_branch: &str) -> AppResult<()> {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent).map_err(AppError::internal)?;
        }
        let dir_text = dir.to_string_lossy().to_string();
        let head = format!("--initial-branch={default_branch}");
        self.run_ok(None, &["init", "--bare", "--quiet", &head, &dir_text])
            .await?;
        self.configure(dir).await?;
        // Repository-provided hooks never run: the hooks path points at
        // Branvil's directory, so the sample hooks are dead weight.
        let _ = std::fs::remove_dir_all(dir.join("hooks"));
        Ok(())
    }

    /// (Re)applies the configuration every managed repository carries.
    pub async fn configure(&self, dir: &Path) -> AppResult<()> {
        let hooks = self.layout.hooks().to_string_lossy().to_string();
        for (key, value) in [
            ("core.hooksPath", hooks.as_str()),
            ("core.logAllRefUpdates", "always"),
            ("http.receivepack", "true"),
            ("http.uploadpack", "true"),
            ("receive.fsckObjects", "true"),
            ("receive.denyDeleteCurrent", "true"),
            ("transfer.hideRefs", "refs/branvil"),
            ("uploadpack.allowAnySHA1InWant", "false"),
        ] {
            self.run_ok(Some(dir), &["config", key, value]).await?;
        }
        Ok(())
    }

    pub async fn set_head(&self, dir: &Path, branch: &str) -> AppResult<()> {
        let target = format!("refs/heads/{branch}");
        self.run_ok(Some(dir), &["symbolic-ref", "HEAD", &target])
            .await
            .map(|_| ())
    }

    /// Every ref of the repository (including hidden ones) with its object.
    pub async fn all_refs(&self, dir: &Path) -> AppResult<BTreeMap<String, String>> {
        let text = self
            .run_ok(
                Some(dir),
                &["for-each-ref", "--format=%(objectname) %(refname)"],
            )
            .await?;
        Ok(text
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(sha, name)| (name.to_string(), sha.to_string()))
            .collect())
    }

    /// Branch name → tip of every branch.
    pub async fn branch_tips(&self, dir: &Path) -> AppResult<BTreeMap<String, String>> {
        Ok(self
            .all_refs(dir)
            .await?
            .into_iter()
            .filter_map(|(name, sha)| {
                name.strip_prefix("refs/heads/")
                    .map(|b| (b.to_string(), sha))
            })
            .collect())
    }

    pub async fn commit_exists(&self, dir: &Path, sha: &str) -> AppResult<bool> {
        let out = self
            .run(Some(dir), &["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .await?;
        Ok(out.ok())
    }

    /// `(sha, author name, author date, parents, message)` of a commit.
    pub async fn commit_info(
        &self,
        dir: &Path,
        sha: &str,
    ) -> AppResult<Option<(String, String, String, Vec<String>, String)>> {
        let spec = format!("{sha}^{{commit}}");
        let out = self
            .run(
                Some(dir),
                &[
                    "show",
                    "-s",
                    "--no-color",
                    "--format=%H%x00%an%x00%aI%x00%P%x00%B",
                    &spec,
                    "--",
                ],
            )
            .await?;
        if !out.ok() {
            return Ok(None);
        }
        let text = out.text();
        let mut parts = text.splitn(5, '\0');
        let mut next = || parts.next().unwrap_or("").to_string();
        let (sha, author, date, parents, message) = (next(), next(), next(), next(), next());
        let parents = parents.split_whitespace().map(str::to_string).collect();
        Ok(Some((
            sha,
            author,
            date,
            parents,
            message.trim_end().to_string(),
        )))
    }

    pub async fn merge_base(&self, dir: &Path, a: &str, b: &str) -> AppResult<Option<String>> {
        let out = self.run(Some(dir), &["merge-base", a, b]).await?;
        Ok(out.ok().then(|| out.text().trim().to_string()))
    }

    /// True when `ancestor` is reachable from `descendant`.
    pub async fn is_ancestor(
        &self,
        dir: &Path,
        ancestor: &str,
        descendant: &str,
    ) -> AppResult<bool> {
        let out = self
            .run(
                Some(dir),
                &["merge-base", "--is-ancestor", ancestor, descendant],
            )
            .await?;
        match out.status {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(AppError::internal(format!(
                "git merge-base: {}",
                out.stderr.trim()
            ))),
        }
    }

    /// Commits reachable from `from` and not from `not`.
    pub async fn count_between(&self, dir: &Path, not: &str, from: &str) -> AppResult<u64> {
        let range = format!("{not}..{from}");
        let text = self
            .run_ok(Some(dir), &["rev-list", "--count", &range])
            .await?;
        text.parse().map_err(AppError::internal)
    }

    /// Conditional ref update: moves `refname` to `new` only if it is `old`.
    /// Returns false when the ref was not at `old`.
    pub async fn update_ref(
        &self,
        dir: &Path,
        refname: &str,
        new: &str,
        old: &str,
        reason: &str,
    ) -> AppResult<bool> {
        let out = self
            .run(Some(dir), &["update-ref", "-m", reason, refname, new, old])
            .await?;
        Ok(out.ok())
    }

    /// Paths changed in `range` (`base...head` diffs from their merge base).
    pub async fn changed_paths(&self, dir: &Path, range: &str) -> AppResult<Vec<String>> {
        let out = self
            .run(
                Some(dir),
                &[
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    "-M",
                    "--name-only",
                    "-z",
                    range,
                    "--",
                ],
            )
            .await?;
        if !out.ok() {
            return Err(AppError::internal(format!(
                "git diff: {}",
                out.stderr.trim()
            )));
        }
        Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect())
    }

    /// The per-file diff of `range` (`base...head` diffs from the merge base).
    pub async fn diff_files(
        &self,
        dir: &Path,
        range: &str,
        patch_limit: usize,
    ) -> AppResult<Vec<FileChange>> {
        let common = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "-M"];
        let status_args: Vec<&str> = common
            .iter()
            .copied()
            .chain(["--name-status", "-z", range, "--"])
            .collect();
        let numstat_args: Vec<&str> = common
            .iter()
            .copied()
            .chain(["--numstat", "-z", range, "--"])
            .collect();
        let patch_args: Vec<&str> = common.iter().copied().chain([range, "--"]).collect();
        let status = self.run(Some(dir), &status_args).await?;
        let numstat = self.run(Some(dir), &numstat_args).await?;
        let patch = self.run(Some(dir), &patch_args).await?;
        for out in [&status, &numstat, &patch] {
            if !out.ok() {
                return Err(AppError::internal(format!(
                    "git diff: {}",
                    out.stderr.trim()
                )));
            }
        }
        let mut files = parse_name_status(&status.stdout);
        let stats = parse_numstat(&numstat.stdout);
        let chunks = split_patch(&patch.stdout);
        for (i, file) in files.iter_mut().enumerate() {
            match stats.get(i) {
                Some(Some((a, d))) => {
                    file.additions = *a;
                    file.deletions = *d;
                }
                Some(None) => file.binary = true,
                None => {}
            }
            if file.binary {
                file.patch = None;
                continue;
            }
            match chunks.get(i) {
                Some(chunk) => {
                    let text = hunks_of(chunk);
                    if text.len() > patch_limit {
                        let mut cut = patch_limit;
                        while !text.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        file.patch = Some(text[..cut].to_string());
                        file.patch_complete = false;
                    } else {
                        file.patch = Some(text);
                    }
                }
                None => {
                    file.patch = Some(String::new());
                    file.patch_complete = !patch.truncated;
                }
            }
        }
        Ok(files)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FileChange {
    pub path: String,
    pub previous_path: Option<String>,
    pub status: &'static str,
    pub additions: u64,
    pub deletions: u64,
    pub patch: Option<String>,
    pub patch_complete: bool,
    #[serde(skip)]
    pub binary: bool,
}

fn parse_name_status(raw: &[u8]) -> Vec<FileChange> {
    let fields: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let code = &fields[i];
        if code.is_empty() {
            i += 1;
            continue;
        }
        let kind = code.as_bytes()[0];
        let (status, previous, path, step) = match kind {
            b'R' | b'C' => (
                if kind == b'R' { "renamed" } else { "added" },
                fields.get(i + 1).cloned(),
                fields.get(i + 2).cloned().unwrap_or_default(),
                3,
            ),
            b'A' => (
                "added",
                None,
                fields.get(i + 1).cloned().unwrap_or_default(),
                2,
            ),
            b'D' => (
                "removed",
                None,
                fields.get(i + 1).cloned().unwrap_or_default(),
                2,
            ),
            _ => (
                "modified",
                None,
                fields.get(i + 1).cloned().unwrap_or_default(),
                2,
            ),
        };
        out.push(FileChange {
            path,
            previous_path: if kind == b'R' { previous } else { None },
            status,
            additions: 0,
            deletions: 0,
            patch: None,
            patch_complete: true,
            binary: false,
        });
        i += step;
    }
    out
}

/// `Some((added, deleted))` per file, `None` for binary files.
fn parse_numstat(raw: &[u8]) -> Vec<Option<(u64, u64)>> {
    let fields: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let field = String::from_utf8_lossy(fields[i]);
        if field.is_empty() {
            i += 1;
            continue;
        }
        let mut parts = field.splitn(3, '\t');
        let add = parts.next().unwrap_or("-");
        let del = parts.next().unwrap_or("-");
        let path = parts.next().unwrap_or("");
        // A rename has an empty path here followed by the old and new paths.
        i += if path.is_empty() { 3 } else { 1 };
        out.push(match (add.parse(), del.parse()) {
            (Ok(a), Ok(d)) => Some((a, d)),
            _ => None,
        });
    }
    out
}

fn split_patch(raw: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(raw);
    let mut chunks: Vec<String> = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            chunks.push(String::new());
        }
        if let Some(last) = chunks.last_mut() {
            last.push_str(line);
        }
    }
    chunks
}

/// The part of a file's diff from its first `@@` line.
fn hunks_of(chunk: &str) -> String {
    let mut offset = 0;
    for line in chunk.split_inclusive('\n') {
        if line.starts_with("@@") {
            return chunk[offset..].to_string();
        }
        offset += line.len();
    }
    String::new()
}

/// Conservative branch-name validation (a subset of `git check-ref-format`).
pub fn valid_branch(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('-')
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.ends_with(".lock")
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && name != "@"
        && !name.split('/').any(|p| p.starts_with('.'))
        && name
            .chars()
            .all(|c| !c.is_control() && !" ~^:?*[\\".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names() {
        assert!(valid_branch("main"));
        assert!(valid_branch("ticket/3"));
        assert!(!valid_branch("a..b"));
        assert!(!valid_branch("-x"));
        assert!(!valid_branch("a b"));
        assert!(!valid_branch("x.lock"));
        assert!(!valid_branch("a/.hidden"));
        assert!(!valid_branch(""));
    }

    #[test]
    fn numstat_handles_renames_and_binaries() {
        let raw = b"1\t2\ta.txt\0-\t-\tbin.png\0".to_vec();
        let mut raw = raw;
        raw.extend_from_slice(b"0\t0\t\0old.txt\0new.txt\0");
        assert_eq!(parse_numstat(&raw), vec![Some((1, 2)), None, Some((0, 0))]);
        let status = b"M\0a.txt\0A\0bin.png\0R100\0old.txt\0new.txt\0";
        let files = parse_name_status(status);
        assert_eq!(files.len(), 3);
        assert_eq!(files[2].status, "renamed");
        assert_eq!(files[2].previous_path.as_deref(), Some("old.txt"));
        assert_eq!(files[2].path, "new.txt");
    }

    #[test]
    fn hunks_start_at_the_first_at_line() {
        let chunk = "diff --git a/x b/x\nindex 1..2\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(hunks_of(chunk), "@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(hunks_of("diff --git a/x b/x\nrename from x\n"), "");
    }
}
