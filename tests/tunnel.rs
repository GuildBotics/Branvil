//! `branvil tunnel` with a fake `ssh` first on PATH: the script records its
//! arguments and exits, so the tunnel's argv and its reconnects are observed
//! without opening any SSH connection.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::BIN;

const END: &str = "--end-of-invocation--";

fn fake_ssh(dir: &Path) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = bin.join("ssh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$FAKE_SSH_LOG\"; done\nprintf '%s\\n' '{END}' >> \"$FAKE_SSH_LOG\"\nexit 255\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn invocations(log: &Path) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let mut out = Vec::new();
    let mut current = Vec::new();
    for line in text.lines() {
        if line == END {
            out.push(std::mem::take(&mut current));
        } else {
            current.push(line.to_string());
        }
    }
    out
}

fn tunnel(dir: &Path, args: &[&str]) -> Command {
    let bin = fake_ssh(dir);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new(BIN);
    cmd.arg("tunnel")
        .args(args)
        .env("PATH", path)
        .env("FAKE_SSH_LOG", dir.join("ssh.log"))
        .env("HOME", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn the_tunnel_runs_ssh_with_a_loopback_forward_and_reconnects() {
    let dir = tempfile::tempdir().unwrap();
    let port = free_port().to_string();
    let mut child = tunnel(
        dir.path(),
        &[
            "--via",
            "hub.example",
            "--port",
            &port,
            "--remote-port",
            &port,
            "--backoff-min-ms",
            "50",
            "--backoff-max-ms",
            "200",
        ],
    )
    .spawn()
    .unwrap();
    let log = dir.path().join("ssh.log");
    let started = Instant::now();
    while invocations(&log).len() < 3 {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "ssh was not restarted: {:?}",
            invocations(&log)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let calls = invocations(&log);
    let forward = format!("127.0.0.1:{port}:127.0.0.1:{port}");
    let expected = vec![
        "-N",
        "-T",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "BatchMode=yes",
        "-L",
        forward.as_str(),
        "--",
        "hub.example",
    ];
    for call in &calls {
        assert_eq!(call, &expected);
    }
    assert!(stderr.contains("connecting"), "{stderr}");
    assert!(stderr.contains("ssh exited with 255"), "{stderr}");
    assert!(stderr.contains("reconnecting in"), "{stderr}");
    // Nothing is written besides what the fake ssh logged: no key or host
    // record of Branvil's own.
    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["bin", "ssh.log"]);
}

#[test]
fn an_option_like_destination_is_refused_before_ssh_runs() {
    let dir = tempfile::tempdir().unwrap();
    for via in ["-oProxyCommand=touch /tmp/x", "hub example"] {
        let out = tunnel(dir.path(), &[&format!("--via={via}")])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{via}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--via must not"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(invocations(&dir.path().join("ssh.log")).is_empty());
}
