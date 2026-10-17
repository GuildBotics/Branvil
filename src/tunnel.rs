//! `branvil tunnel`: forwards a local loopback port to the Branvil on the Hub
//! machine through the system OpenSSH client, and starts it again whenever it
//! exits. Authentication, host keys, and the host itself come from the user's
//! OpenSSH configuration; Branvil stores none of them.

use std::time::{Duration, Instant};

use anyhow::bail;

/// A connection that lasted this long resets the reconnect delay.
const STABLE: Duration = Duration::from_secs(60);

pub struct Options {
    pub via: String,
    pub port: u16,
    pub remote_port: u16,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
}

/// Refuses destinations `ssh` could read as an option or that carry
/// whitespace or control characters.
pub fn validate_destination(via: &str) -> anyhow::Result<()> {
    if via.is_empty() || via.len() > 255 {
        bail!("--via must be 1-255 characters");
    }
    if via.starts_with('-') {
        bail!("--via must not start with '-' (it would be read as an ssh option)");
    }
    if via.chars().any(|c| c.is_whitespace() || c.is_control()) {
        bail!("--via must not contain spaces or control characters");
    }
    Ok(())
}

/// The `ssh` arguments of one connection.
pub fn ssh_args(o: &Options) -> Vec<String> {
    vec![
        "-N".into(),
        "-T".into(),
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=3".into(),
        // A resident tunnel has nobody to answer a prompt.
        "-o".into(),
        "BatchMode=yes".into(),
        "-L".into(),
        format!("127.0.0.1:{}:127.0.0.1:{}", o.port, o.remote_port),
        "--".into(),
        o.via.clone(),
    ]
}

fn next_delay(current: Duration, o: &Options) -> Duration {
    (current * 2).clamp(o.backoff_min, o.backoff_max)
}

/// Runs until interrupted (Ctrl-C, SIGTERM).
pub async fn run(o: Options) -> anyhow::Result<()> {
    validate_destination(&o.via)?;
    if o.port == 0 || o.remote_port == 0 {
        bail!("ports must be 1-65535");
    }
    if o.port != o.remote_port {
        eprintln!(
            "branvil tunnel: warning: the local port {} differs from the remote port {}; \
             Branvil accepts only the Host of its base URL, which is the same on every machine",
            o.port, o.remote_port
        );
    }
    let args = ssh_args(&o);
    let mut delay = o.backoff_min;
    let stop = shutdown();
    tokio::pin!(stop);
    loop {
        eprintln!(
            "branvil tunnel: connecting 127.0.0.1:{} → {} 127.0.0.1:{}",
            o.port, o.via, o.remote_port
        );
        let started = Instant::now();
        let mut child = match tokio::process::Command::new("ssh")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => bail!("cannot start ssh (is OpenSSH installed and on PATH?): {e}"),
        };
        let probe = probe(o.port);
        tokio::pin!(probe);
        let status = loop {
            tokio::select! {
                status = child.wait() => break status,
                _ = &mut probe => {}
                _ = &mut stop => {
                    let _ = child.kill().await;
                    eprintln!("branvil tunnel: stopped");
                    return Ok(());
                }
            }
        };
        let lasted = started.elapsed();
        if lasted >= STABLE {
            delay = o.backoff_min;
        }
        let code = status
            .map(|s| s.code().map_or("a signal".to_string(), |c| c.to_string()))
            .unwrap_or_else(|e| e.to_string());
        eprintln!(
            "branvil tunnel: down (ssh exited with {code} after {}s); reconnecting in {}ms",
            lasted.as_secs(),
            delay.as_millis()
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = &mut stop => {
                eprintln!("branvil tunnel: stopped");
                return Ok(());
            }
        }
        delay = next_delay(delay, &o);
    }
}

/// Reports once whether Branvil answers through the forward.
async fn probe(port: u16) {
    let url = format!("http://127.0.0.1:{port}/api/v1/health");
    let client = reqwest::Client::new();
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                eprintln!("branvil tunnel: up (Branvil answers at http://127.0.0.1:{port})");
                break;
            }
            Ok(r) => {
                eprintln!(
                    "branvil tunnel: up, but the service answered {} (is the port the base URL's?)",
                    r.status().as_u16()
                );
                break;
            }
            Err(_) => {}
        }
    }
    std::future::pending::<()>().await
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            via: "hub".into(),
            port: 7878,
            remote_port: 7878,
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }

    #[test]
    fn destinations_cannot_be_options() {
        assert!(validate_destination("hub").is_ok());
        assert!(validate_destination("alice@hub.example").is_ok());
        assert!(validate_destination("-oProxyCommand=x").is_err());
        assert!(validate_destination("hub -v").is_err());
        assert!(validate_destination("").is_err());
    }

    #[test]
    fn forwards_loopback_to_loopback() {
        let args = ssh_args(&options());
        assert!(
            args.windows(2)
                .any(|w| w == ["-L", "127.0.0.1:7878:127.0.0.1:7878"])
        );
        assert_eq!(args[args.len() - 2..], ["--", "hub"]);
    }

    #[test]
    fn delays_double_up_to_the_cap() {
        let o = options();
        assert_eq!(
            next_delay(Duration::from_secs(1), &o),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_delay(Duration::from_secs(40), &o),
            Duration::from_secs(60)
        );
    }
}
