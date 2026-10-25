//! A test service and its descendants share a private process group. Even
//! before exec, a Git child can hold the service's database descriptor after
//! the service dies. Every exit path drains the group before returning.

use std::io;
use std::process::{Child, ChildStdout, Command, ExitStatus};
use std::time::{Duration, Instant};

pub struct ServiceProcess {
    child: Child,
    reaped: bool,
}

impl ServiceProcess {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        Ok(Self {
            child: command.spawn()?,
            reaped: false,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn take_stdout(&mut self) -> ChildStdout {
        self.child.stdout.take().expect("piped stdout")
    }

    pub fn stop(&mut self) -> io::Result<ExitStatus> {
        if self.reaped {
            return self.child.wait();
        }
        #[cfg(unix)]
        self.drain_group()?;
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        while !self.exited()? {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.stop()
    }

    pub fn exit_within(&mut self, limit: Duration) -> io::Result<Option<ExitStatus>> {
        let start = Instant::now();
        while start.elapsed() < limit {
            if self.exited()? {
                return self.stop().map(Some);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.stop()?;
        Ok(None)
    }

    #[cfg(unix)]
    fn exited(&mut self) -> io::Result<bool> {
        // Keep the exited leader unreaped until its group is drained: reaping
        // first would allow its PID (and thus our group ID) to be reused.
        loop {
            // SAFETY: waitid initializes this valid siginfo_t; WNOWAIT leaves
            // our Child as the sole owner of reaping this process.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            };
            if result == 0 {
                // SAFETY: waitid succeeded and populated the SIGCHLD fields.
                return Ok(unsafe { info.si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    #[cfg(not(unix))]
    fn exited(&mut self) -> io::Result<bool> {
        self.child.try_wait().map(|status| status.is_some())
    }

    #[cfg(unix)]
    fn drain_group(&self) -> io::Result<()> {
        let start = Instant::now();
        loop {
            // SAFETY: spawn assigned this child a private group, and we have
            // not reaped its leader, so this ID cannot name a different group.
            let signal_error =
                if unsafe { libc::killpg(self.id() as libc::pid_t, libc::SIGKILL) } != 0 {
                    let error = io::Error::last_os_error();
                    (error.raw_os_error() != Some(libc::ESRCH)).then_some(error)
                } else {
                    None
                };
            let mut ps = Command::new("ps");
            // Linux can show a zombie leader while another thread still owns
            // its descriptors. Include every thread, not just each leader.
            #[cfg(target_os = "linux")]
            ps.args(["-eLo", "pgid=,stat="]);
            #[cfg(not(target_os = "linux"))]
            ps.args(["-axo", "pgid=,stat="]);
            let output = ps.output()?;
            if !output.status.success() {
                return Err(io::Error::other("cannot inspect service process group"));
            }
            let listing = String::from_utf8(output.stdout).map_err(io::Error::other)?;
            let mut live = false;
            for line in listing.lines() {
                let mut fields = line.split_whitespace();
                let group: u32 = fields
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| io::Error::other("invalid ps group"))?;
                let state = fields
                    .next()
                    .ok_or_else(|| io::Error::other("missing ps state"))?;
                // Orphan zombies may await the host's init; their descriptors
                // are already closed. Waiting for killpg(0) would hang on them.
                live |= group == self.id() && !state.starts_with('Z');
            }
            if !live {
                // macOS may return EPERM for a group containing only zombies.
                // Success means there are no live owners, not that a signal
                // was delivered to an already-dead process.
                return Ok(());
            }
            if let Some(error) = signal_error {
                return Err(error);
            }
            if start.elapsed() >= Duration::from_secs(10) {
                return Err(io::Error::other("service process group did not exit"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        if !self.reaped {
            // Also runs when reading the startup banner panics. Do not panic
            // again while unwinding; explicit waits report cleanup errors.
            let _ = self.stop();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::Stdio;

    const FIXTURE: &str = "common::process::tests::lock_holder_fixture";

    fn command(dir: &Path, depth: u8) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", FIXTURE, "--nocapture"])
            .env("BRANVIL_TEST_LOCK_DIR", dir)
            .env("BRANVIL_TEST_LOCK_DEPTH", depth.to_string())
            .stdout(Stdio::piped());
        command
    }

    fn ready(stdout: ChildStdout) {
        let mut lines = BufReader::new(stdout).lines();
        assert!(lines.any(|line| line.unwrap() == "lock holder ready"));
    }

    // Spawned only by the regression below. Each generation holds a real
    // redb lock; descendants outlive a leader's natural exit unless drained.
    #[test]
    fn lock_holder_fixture() {
        let Some(dir) = std::env::var_os("BRANVIL_TEST_LOCK_DIR") else {
            return;
        };
        let dir = Path::new(&dir);
        let depth: u8 = std::env::var("BRANVIL_TEST_LOCK_DEPTH")
            .unwrap()
            .parse()
            .unwrap();
        let _db = redb::Database::create(dir.join(format!("{depth}.redb"))).unwrap();
        let _child = if depth < 2 {
            // Inherit the group: ServiceProcess::spawn is only for the root.
            let mut child = command(dir, depth + 1).spawn().unwrap();
            ready(child.stdout.take().unwrap());
            Some(child)
        } else {
            None
        };
        println!("lock holder ready");
        loop {
            // TempDir removes the directory even after a regression assertion
            // fails. Exit then, rather than leaving mutation-test orphans.
            if !dir.exists() {
                return;
            }
            if depth == 0
                && let Ok(code) = std::fs::read_to_string(dir.join("exit"))
            {
                std::process::exit(code.parse().unwrap());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn every_exit_path_releases_descendant_locks_before_returning() {
        for mode in [
            "crash", "timeout", "success", "failure", "bounded", "drop", "panic",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut process = ServiceProcess::spawn(&mut command(dir.path(), 0)).unwrap();
            assert_eq!(
                unsafe { libc::getpgid(process.id() as libc::pid_t) },
                process.id() as libc::pid_t
            );
            ready(process.take_stdout());
            // The descendant locks really are held, so this test detects a
            // parent-only kill or a parent-only natural wait.
            for depth in 0..=2 {
                assert!(redb::Database::open(dir.path().join(format!("{depth}.redb"))).is_err());
            }
            match mode {
                "crash" => assert_eq!(process.stop().unwrap().signal(), Some(libc::SIGKILL)),
                "timeout" => assert_eq!(process.exit_within(Duration::ZERO).unwrap(), None),
                "success" | "failure" | "bounded" => {
                    let code = if mode == "failure" { 17 } else { 0 };
                    std::fs::write(dir.path().join("exit"), code.to_string()).unwrap();
                    let start = Instant::now();
                    while !process.exited().unwrap() {
                        assert!(start.elapsed() < Duration::from_secs(10));
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    // Observing exit must leave the leader unreaped, pinning
                    // the group ID until all descendants have been drained.
                    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                    assert_eq!(
                        unsafe {
                            libc::waitid(
                                libc::P_PID,
                                process.id() as libc::id_t,
                                &mut info,
                                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                            )
                        },
                        0
                    );
                    assert_eq!(unsafe { info.si_pid() }, process.id() as libc::pid_t);
                    let status = if mode == "bounded" {
                        process
                            .exit_within(Duration::from_secs(10))
                            .unwrap()
                            .unwrap()
                    } else {
                        process.wait().unwrap()
                    };
                    assert_eq!(status.code(), Some(code));
                    assert_eq!(process.stop().unwrap(), status);
                }
                "drop" => drop(process),
                "panic" => {
                    assert!(
                        std::panic::catch_unwind(move || {
                            let _process = process;
                            panic!("simulate a rejected startup banner");
                        })
                        .is_err()
                    );
                }
                _ => unreachable!(),
            }
            // No retry or sleep: returning means all owners released the DBs.
            for depth in 0..=2 {
                redb::Database::open(dir.path().join(format!("{depth}.redb")))
                    .unwrap_or_else(|error| panic!("{mode}, generation {depth}: {error}"));
            }
        }
    }
}
