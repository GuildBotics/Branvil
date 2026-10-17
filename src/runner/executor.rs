//! Executors: where a job's commands run. The one implementation is
//! microsandbox (one microVM per job), driven through the pinned `msb` CLI.
//! There is no host executor: when the executor is unusable, jobs conclude
//! `startup_failure`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, watch};

use crate::ci::pipeline::JobDef;

/// The `msb` version this runner accepts (the CLI flags and behaviour it was
/// verified against).
pub const MSB_VERSION: &str = "0.6.17";

#[derive(Clone, Debug)]
pub struct Health {
    pub ready: bool,
    pub detail: Option<String>,
}

/// One job, with limits already applied.
pub struct JobRun<'a> {
    pub attempt_id: &'a str,
    pub job: &'a JobDef,
    /// The host directory mounted at `/work`: the commit's tree.
    pub work: &'a Path,
    /// An empty host directory of this job only, mounted at [`LOG_MOUNT`]:
    /// the job's output is written there, so the runner reads it while the
    /// job runs and keeps it when the job is stopped.
    pub log_dir: &'a Path,
    pub network: bool,
    /// Environment of the job's commands (no secrets: CI metadata only).
    pub env: Vec<(String, String)>,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The commands ran; the exit code of the first failing one (or 0).
    Exited(i32),
    TimedOut,
    Cancelled,
    /// The commands never started (the reason).
    StartupFailure(String),
}

pub trait Executor: Send + Sync {
    fn name(&self) -> &'static str;
    fn health(&self) -> impl Future<Output = Health> + Send;
    fn image_present(&self, image: &str) -> impl Future<Output = Result<bool, String>> + Send;
    /// Runs the job; output goes to `log`, `cancel` turning true stops it.
    fn run(
        &self,
        job: JobRun<'_>,
        log: mpsc::Sender<Vec<u8>>,
        cancel: watch::Receiver<bool>,
    ) -> impl Future<Output = Outcome> + Send;
    /// Removes leftovers of earlier runs of this runner.
    fn sweep(&self) -> impl Future<Output = ()> + Send;
}

// ---------------------------------------------------------------- microsandbox

/// Where an accepted `msb` lives, or why none was accepted.
#[derive(Clone, Debug)]
pub enum Msb {
    Found(PathBuf),
    Missing(String),
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

/// `~/.branvil` (short on every OS, outside OS temp directories).
pub fn branvil_home() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".branvil")
}

/// The candidates in order: the configured binary, GuildBotics' deployed
/// copy, Branvil's own copy. The first that reports exactly the pinned
/// version wins; nothing is ever downloaded.
pub fn resolve_msb(configured: Option<&Path>) -> Msb {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(c) = configured {
        candidates.push(c.to_path_buf());
    }
    if let Some(home) = home_dir() {
        candidates.push(home.join(".guildbotics/data/msb/bin").join(msb_file()));
    }
    candidates.push(branvil_home().join("msb/bin").join(msb_file()));
    let mut notes = Vec::new();
    for c in candidates {
        if !c.is_file() {
            notes.push(format!("{}: not found", c.display()));
            continue;
        }
        match std::process::Command::new(&c)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
        {
            Ok(out) => {
                let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if v == format!("msb {MSB_VERSION}") {
                    return Msb::Found(c);
                }
                notes.push(format!(
                    "{}: reports '{v}', not msb {MSB_VERSION}",
                    c.display()
                ));
            }
            Err(e) => notes.push(format!("{}: cannot run ({e})", c.display())),
        }
    }
    Msb::Missing(format!(
        "No usable msb {MSB_VERSION} found ({}). Install GuildBotics' runtime or run `branvil runner install-msb --from <msb>`.",
        notes.join("; ")
    ))
}

fn msb_file() -> &'static str {
    if cfg!(windows) { "msb.exe" } else { "msb" }
}

pub struct Microsandbox {
    msb: Msb,
    /// Branvil's own `MSB_HOME` (image cache, sandbox records).
    home: PathBuf,
    /// Label value that marks this runner's sandboxes.
    runner: String,
    health: tokio::sync::Mutex<Option<(Instant, Health)>>,
}

impl Microsandbox {
    pub fn new(msb: Msb, home: PathBuf, runner: &str) -> Self {
        Self {
            msb,
            home,
            runner: runner.to_string(),
            health: tokio::sync::Mutex::new(None),
        }
    }

    pub fn msb(&self) -> &Msb {
        &self.msb
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// An `msb` command with Branvil's `MSB_HOME`, an empty Docker
    /// configuration (no host registry credentials), and only the host
    /// environment variables the runtime needs.
    pub fn command(&self) -> Option<tokio::process::Command> {
        let Msb::Found(path) = &self.msb else {
            return None;
        };
        let docker = self.home.join("docker-config-empty");
        let _ = std::fs::create_dir_all(&docker);
        let mut cmd = tokio::process::Command::new(path);
        cmd.env_clear();
        for key in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "TMPDIR",
            "LANG",
            "SystemRoot",
            "USERPROFILE",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
        ] {
            if let Some(v) = std::env::var_os(key) {
                cmd.env(key, v);
            }
        }
        cmd.env("MSB_HOME", &self.home)
            .env("DOCKER_CONFIG", &docker)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        Some(cmd)
    }

    async fn output(&self, args: &[&str]) -> Result<std::process::Output, String> {
        let mut cmd = self.command().ok_or_else(|| self.missing())?;
        cmd.args(args);
        tokio::time::timeout(Duration::from_secs(120), cmd.output())
            .await
            .map_err(|_| format!("msb {} timed out", args.first().unwrap_or(&"")))?
            .map_err(|e| format!("cannot run msb: {e}"))
    }

    fn missing(&self) -> String {
        match &self.msb {
            Msb::Missing(m) => m.clone(),
            Msb::Found(_) => String::new(),
        }
    }

    fn sandbox_name(id: &str) -> String {
        format!("bv-job-{}", id.to_lowercase())
    }

    async fn remove(&self, name: &str) {
        let _ = self.output(&["remove", "--force", "-q", name]).await;
    }
}

/// Where a job's log directory is mounted in the VM.
pub const LOG_MOUNT: &str = "/.branvil-log";

/// The shell script a job runs: its output into the log file, a start
/// marker, then each command in its own `sh -c`, stopping at the first
/// failure with its exit code.
///
/// The output goes to a file of the log mount, not to `msb run`'s stdout:
/// msb 0.6.17 hands a command's stdout and stderr over only when the command
/// exits, so a running job's log would stay empty and a stopped job's output
/// (a timeout, a cancellation) would be lost.
pub fn script(commands: &[String]) -> String {
    let mut s = format!("exec >>{LOG_MOUNT}/out 2>&1\nprintf '\\036branvil:started\\n'\n");
    for c in commands {
        let q = c.replace('\'', "'\\''");
        s.push_str(&format!(
            "printf '\\n$ %s\\n' '{q}'\n/bin/sh -c '{q}' || exit $?\n"
        ));
    }
    s
}

const MARKER: &[u8] = b"\x1ebranvil:started\n";

/// A job's log file as the runner reads it: the start marker is taken out
/// (the startup time is noted in its place); everything else goes to the log.
struct JobLog<'a> {
    started_at: Instant,
    started: bool,
    pending: Vec<u8>,
    log: &'a mpsc::Sender<Vec<u8>>,
}

impl JobLog<'_> {
    /// Reads what was appended to `file` since the last read, at most 1 MiB
    /// at a time so that a job writing fast cannot hold off its timeout.
    async fn read_from(&mut self, file: &mut tokio::fs::File) {
        let mut buf = vec![0u8; 64 << 10];
        for _ in 0..16 {
            match file.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => self.feed(&buf[..n]).await,
            }
        }
    }

    async fn feed(&mut self, bytes: &[u8]) {
        if self.started {
            let _ = self.log.send(bytes.to_vec()).await;
            return;
        }
        self.pending.extend_from_slice(bytes);
        if let Some(pos) = self.pending.windows(MARKER.len()).position(|w| w == MARKER) {
            self.started = true;
            if pos > 0 {
                let _ = self.log.send(self.pending[..pos].to_vec()).await;
            }
            let note = format!(
                "[branvil] sandbox started in {} ms\n",
                self.started_at.elapsed().as_millis()
            );
            let _ = self.log.send(note.into_bytes()).await;
            let _ = self
                .log
                .send(self.pending[pos + MARKER.len()..].to_vec())
                .await;
            self.pending.clear();
        } else if self.pending.len() > 4096 {
            let keep = self.pending.split_off(self.pending.len() - MARKER.len());
            let _ = self
                .log
                .send(std::mem::replace(&mut self.pending, keep))
                .await;
        }
    }

    async fn flush(&mut self) {
        if !self.pending.is_empty() {
            let _ = self.log.send(std::mem::take(&mut self.pending)).await;
        }
    }
}

impl Executor for Microsandbox {
    fn name(&self) -> &'static str {
        "microsandbox"
    }

    async fn health(&self) -> Health {
        let mut cached = self.health.lock().await;
        if let Some((at, h)) = cached.as_ref()
            && at.elapsed() < Duration::from_secs(if h.ready { 300 } else { 30 })
        {
            return h.clone();
        }
        let h = match &self.msb {
            Msb::Missing(m) => Health {
                ready: false,
                detail: Some(m.clone()),
            },
            Msb::Found(_) => match std::fs::create_dir_all(&self.home) {
                Err(e) => Health {
                    ready: false,
                    detail: Some(format!("cannot create {}: {e}", self.home.display())),
                },
                Ok(()) => match self.output(&["doctor"]).await {
                    Ok(out) if out.status.success() => Health {
                        ready: true,
                        detail: None,
                    },
                    Ok(out) => Health {
                        ready: false,
                        detail: Some(format!(
                            "msb doctor reports a problem: {}{}",
                            tail(&out.stdout, 1024),
                            tail(&out.stderr, 1024)
                        )),
                    },
                    Err(e) => Health {
                        ready: false,
                        detail: Some(e),
                    },
                },
            },
        };
        *cached = Some((Instant::now(), h.clone()));
        h
    }

    async fn image_present(&self, image: &str) -> Result<bool, String> {
        let out = self.output(&["image", "inspect", image]).await?;
        Ok(out.status.success())
    }

    async fn run(
        &self,
        job: JobRun<'_>,
        log: mpsc::Sender<Vec<u8>>,
        mut cancel: watch::Receiver<bool>,
    ) -> Outcome {
        let Some(mut cmd) = self.command() else {
            return Outcome::StartupFailure(self.missing());
        };
        let work = job.work.to_string_lossy().to_string();
        let log_dir = job.log_dir.to_string_lossy().to_string();
        if let Some(dir) = [&work, &log_dir]
            .into_iter()
            .find(|d| d.contains(':') || d.contains(','))
        {
            return Outcome::StartupFailure(format!(
                "the work directory {dir} contains ':' or ',', which msb mount specifications cannot carry"
            ));
        }
        // Created here, before the VM exists, and read through this handle
        // only: whatever the job later puts at that path (a link, another
        // file) is never opened on the host.
        let mut output = match tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(job.log_dir.join("out"))
            .await
        {
            Ok(f) => f,
            Err(e) => return Outcome::StartupFailure(format!("cannot create the job log: {e}")),
        };
        let name = Self::sandbox_name(job.attempt_id);
        let j = job.job;
        cmd.args(["run", "--name", &name, "--quiet", "--pull", "never"])
            .args(["--label", &format!("branvil.runner={}", self.runner)])
            .args(["--label", &format!("branvil.job={}", job.attempt_id)])
            .args(["--cpus", &j.cpus.to_string()])
            .args(["--memory", &format!("{}M", j.memory_mib)])
            .args(["--mount-dir", &format!("{work}:/work")])
            .args(["--mount-dir", &format!("{log_dir}:{LOG_MOUNT}")])
            .args(["--workdir", "/work"])
            // Backstop: the VM itself ends shortly after the job's timeout.
            .args(["--max-duration", &format!("{}s", j.timeout_secs + 60)]);
        if job.network {
            // Admin-approved: public destinations only (never the host or
            // private networks). Hostname rules are best effort.
            cmd.args(["--net", "public"]);
        } else {
            cmd.arg("--no-net");
        }
        for (k, v) in &job.env {
            cmd.args(["--env", &format!("{k}={v}")]);
        }
        cmd.arg(&j.image)
            .args(["--", "/bin/sh", "-c", &script(&j.commands)])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut job_log = JobLog {
            started_at: Instant::now(),
            started: false,
            pending: Vec::new(),
            log: &log,
        };
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Outcome::StartupFailure(format!("cannot start msb: {e}")),
        };
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let (tx_err, mut rx_err) = mpsc::channel::<Vec<u8>>(64);
        let err_reader = tokio::spawn(async move {
            let mut buf = vec![0u8; 16 << 10];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 || tx_err.send(buf[..n].to_vec()).await.is_err() {
                    break;
                }
            }
        });
        let mut stderr_tail: Vec<u8> = Vec::new();
        let mut out_buf = vec![0u8; 16 << 10];
        let deadline = tokio::time::sleep(Duration::from_secs(j.timeout_secs));
        tokio::pin!(deadline);
        let mut tail = tokio::time::interval(Duration::from_millis(50));
        let mut out_open = true;
        let mut err_open = true;
        let mut stop: Option<Outcome> = None;
        while out_open || err_open {
            tokio::select! {
                _ = tail.tick() => job_log.read_from(&mut output).await,
                // msb's own words (the job's output is in the log file).
                n = stdout.read(&mut out_buf), if out_open => match n {
                    Ok(0) | Err(_) => out_open = false,
                    Ok(n) => {
                        let _ = log.send(out_buf[..n].to_vec()).await;
                    }
                },
                e = rx_err.recv(), if err_open => match e {
                    Some(bytes) => {
                        stderr_tail.extend_from_slice(&bytes);
                        if stderr_tail.len() > 4096 {
                            stderr_tail.drain(..stderr_tail.len() - 4096);
                        }
                        let _ = log.send(bytes).await;
                    }
                    None => err_open = false,
                },
                _ = &mut deadline => {
                    stop = Some(Outcome::TimedOut);
                    break;
                }
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        stop = Some(Outcome::Cancelled);
                        break;
                    }
                }
            }
        }
        let outcome = match stop {
            Some(o) => {
                // Killing `msb run` stops the VM (its watchdog watches the
                // parent), and every process inside it with it.
                let _ = child.kill().await;
                o
            }
            None => match child.wait().await {
                Ok(status) => {
                    // Everything the job wrote is in the file once it ended.
                    job_log.read_from(&mut output).await;
                    if job_log.started {
                        Outcome::Exited(status.code().unwrap_or(-1))
                    } else {
                        Outcome::StartupFailure(format!(
                            "msb run ended ({status}) before the commands started: {}",
                            String::from_utf8_lossy(&stderr_tail).trim()
                        ))
                    }
                }
                Err(e) => Outcome::StartupFailure(format!("waiting for msb failed: {e}")),
            },
        };
        // What the job wrote up to the moment it was stopped.
        job_log.read_from(&mut output).await;
        job_log.flush().await;
        err_reader.abort();
        self.remove(&name).await;
        outcome
    }

    async fn sweep(&self) {
        if matches!(self.msb, Msb::Found(_)) {
            let label = format!("branvil.runner={}", self.runner);
            match self
                .output(&["remove", "--force", "-q", "--label", &label])
                .await
            {
                Ok(out) if out.status.success() => {}
                Ok(out) => tracing::warn!(
                    "removing leftover sandboxes failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => tracing::warn!("removing leftover sandboxes failed: {e}"),
            }
        }
    }
}

fn tail(bytes: &[u8], n: usize) -> String {
    let start = bytes.len().saturating_sub(n);
    String::from_utf8_lossy(&bytes[start..]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_quotes_every_command() {
        let s = script(&["echo 'a b'".into(), "exit 3".into()]);
        assert!(s.starts_with("exec >>/.branvil-log/out 2>&1\nprintf '\\036branvil:started\\n'\n"));
        assert!(s.contains("/bin/sh -c 'echo '\\''a b'\\''' || exit $?"));
        assert!(s.contains("/bin/sh -c 'exit 3' || exit $?"));
    }

    #[test]
    fn a_missing_msb_is_reported_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("msb");
        std::fs::write(&fake, "#!/bin/sh\necho 'msb 0.0.1'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The configured candidate has the wrong version; whatever else is
        // found must report exactly the pinned version.
        match resolve_msb(Some(&fake)) {
            Msb::Found(p) => assert_ne!(p, fake),
            Msb::Missing(m) => assert!(m.contains("not msb 0.6.17") || cfg!(windows), "{m}"),
        }
    }
}
