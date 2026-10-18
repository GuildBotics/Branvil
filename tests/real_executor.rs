//! The real executor, end to end: a real `branvil serve`, a real
//! `branvil runner` process, and microVMs started by the pinned `msb`.
//!
//! Opt-in (it needs the msb 0.6.17 runtime, a hypervisor, and the image
//! below already in Branvil's image cache):
//!
//! ```sh
//! branvil runner pull docker.io/library/alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8
//! BRANVIL_REAL_EXECUTOR=1 cargo test --test main real_executor:: -- --nocapture
//! ```

use crate::common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};

const IMAGE: &str = "docker.io/library/alpine@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8";

fn enabled() -> bool {
    std::env::var("BRANVIL_REAL_EXECUTOR").as_deref() == Ok("1")
}

fn home() -> String {
    std::env::var("HOME").unwrap()
}

fn msb() -> String {
    format!("{}/.guildbotics/data/msb/bin/msb", home())
}

fn msb_home() -> String {
    format!("{}/.branvil/msb-home", home())
}

/// The sandboxes of Branvil's MSB_HOME that the runner `runner` labelled,
/// names only. Other runs may use the same MSB_HOME at the same time (or have
/// left sandboxes behind when they were killed), so this run looks only at
/// its own.
fn sandboxes(runner: &str) -> Vec<String> {
    let out = Command::new(msb())
        .args(["list", "--format", "json", "--label"])
        .arg(format!("branvil.runner={runner}"))
        .env("MSB_HOME", msb_home())
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(json!([]));
    v.as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str().map(str::to_string))
        .collect()
}

/// RSS in KiB of the processes whose command line contains `needle`.
fn rss_of(needle: &str) -> Vec<u64> {
    let out = Command::new("ps")
        .args(["-ax", "-o", "rss=,command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(needle))
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

/// The sandbox names of the attempts in `checks` (`bv-job-<attempt id>`).
fn job_names(checks: &Value) -> Vec<String> {
    checks["checks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["attempt_id"].as_str())
        .map(|id| format!("bv-job-{}", id.to_lowercase()))
        .collect()
}

/// RSS in KiB of the `msb <verb> --name <name>` processes of these jobs.
fn jobs_rss(verb: &str, names: &[String]) -> Vec<u64> {
    names
        .iter()
        .flat_map(|n| rss_of(&format!("msb {verb} --name {n} ")))
        .collect()
}

async fn job_log(w: &World, c: &Value) -> String {
    let id = c["attempt_id"].as_str().unwrap();
    w.server
        .ok(
            &format!("{}/jobs/{id}/log?tail_bytes=1000000", w.repo_path()),
            &w.aiko,
        )
        .await["log"]
        .as_str()
        .unwrap()
        .to_string()
}

fn pipeline(port: &str) -> String {
    let h = home();
    format!(
        r#"
[[jobs]]
name = "probe"
image = "{IMAGE}"
timeout_secs = 120
memory_mib = 512
commands = [
  "echo stdout-line; echo stderr-line >&2",
  "test -f /work/README.md",
  "test ! -e '{h}'",
  "test ! -e /var/run/docker.sock",
  "if wget -q -T 5 -O /dev/null http://example.com; then echo NETWORK-OPEN; exit 90; fi",
  "if nc -z -w 3 1.1.1.1 443; then echo NETWORK-OPEN; exit 91; fi",
  "if wget -q -T 3 -O /dev/null http://host.microsandbox.internal:{port}/; then echo HOST-OPEN; exit 92; fi",
  "env | sort",
  "mkdir -p out && echo report > out/report.txt",
]
artifacts = [{{name = "report", paths = ["out/"]}}]

[[jobs]]
name = "exit3"
image = "{IMAGE}"
commands = ["echo before", "exit 3", "echo never"]

[[jobs]]
name = "slow"
image = "{IMAGE}"
timeout_secs = 5
commands = ["echo before-timeout", "sleep 600 & sleep 600"]
"#
    )
}

#[tokio::test]
async fn jobs_run_in_microvms_without_network_or_host_files() {
    if !enabled() {
        eprintln!("skipped: set BRANVIL_REAL_EXECUTOR=1 to run the real executor test");
        return;
    }
    let w = World::new().await;
    let s = &w.server;
    let runner_name = format!("real-{}", std::process::id());
    let (st, _) = s
        .post(
            "/members",
            &w.owner,
            json!({"name": runner_name, "kind": "runner"}),
        )
        .await;
    assert_eq!(st, 201);
    let (_, t) = s
        .post(
            &format!("/members/{runner_name}/tokens"),
            &w.owner,
            json!({"label": "t"}),
        )
        .await;
    let token = t["token"].as_str().unwrap().to_string();

    // The pipeline lands on main through a reviewed pull request; main's new
    // tip (also the tip of `setup`) gets the run.
    let port = s.url.rsplit(':').next().unwrap().to_string();
    std::fs::create_dir_all(w.work.join(".branvil")).unwrap();
    let head = w.push_change("setup", ".branvil/ci.toml", &pipeline(&port));
    let p = w.open_pull("setup", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    let base = format!("{}/pulls/{n}", w.repo_path());
    let (st, v) = s
        .post(
            &format!("{base}/merge"),
            &w.owner,
            json!({"expected_head": head, "expected_base": p["base_sha"]}),
        )
        .await;
    assert_eq!(st, 200, "{v}");

    let work_dir = w.dir.path().join("runner");
    let mut child = Command::new(BIN)
        .args(["runner", "--url", &s.url, "--work-dir"])
        .arg(&work_dir)
        .env("BRANVIL_TOKEN", &token)
        .env("BRANVIL_LOG", "branvil=debug")
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(w.dir.path().join("runner.log")).unwrap())
        .spawn()
        .unwrap();

    let runner_cmd = format!("--work-dir {}", work_dir.display());
    let started = Instant::now();
    let mut peak_rss: u64 = 0;
    let mut peak_msb_run: u64 = 0;
    let mut peak_runner: u64 = 0;
    let mut done = Value::Null;
    let mut names = Vec::new();
    let mut seen_running = false;
    while started.elapsed() < Duration::from_secs(300) {
        peak_rss = peak_rss.max(jobs_rss("sandbox", &names).into_iter().max().unwrap_or(0));
        peak_msb_run = peak_msb_run.max(jobs_rss("run", &names).into_iter().max().unwrap_or(0));
        peak_runner = peak_runner.max(rss_of(&runner_cmd).into_iter().max().unwrap_or(0));
        let v = s.ok(&format!("{base}/checks"), &w.aiko).await;
        names = job_names(&v);
        // The log of a running job is readable while it runs.
        if let Some(c) = v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "slow" && c["status"] == "in_progress")
            && job_log(&w, c).await.contains("before-timeout")
        {
            seen_running = true;
        }
        if v["checks"].as_array().unwrap().len() == 3
            && v["checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["status"] == "completed")
        {
            done = v;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let total = started.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    let runner_log = std::fs::read_to_string(w.dir.path().join("runner.log")).unwrap_or_default();
    assert!(
        !done.is_null(),
        "jobs did not finish; runner log:\n{runner_log}"
    );
    let by = |name: &str| {
        done["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap()
            .clone()
    };
    let probe = by("probe");
    let probe_log = job_log(&w, &probe).await;
    println!("--- probe log ---\n{probe_log}");
    assert_eq!(
        probe["conclusion"], "success",
        "{probe}\n{probe_log}\n{runner_log}"
    );
    assert!(probe_log.contains("stdout-line"));
    assert!(probe_log.contains("stderr-line"));
    assert!(
        !probe_log
            .lines()
            .any(|l| l == "NETWORK-OPEN" || l == "HOST-OPEN"),
        "the job reached a network"
    );
    assert!(probe_log.contains("bad address 'example.com'"), "no DNS");
    assert!(
        probe_log.contains("BRANVIL_SHA="),
        "job metadata is in the environment"
    );
    assert!(
        !probe_log.contains("BRANVIL_TOKEN"),
        "no token in the job environment"
    );
    let startup_ms: u64 = probe_log
        .lines()
        .find_map(|l| l.strip_prefix("[branvil] sandbox started in "))
        .and_then(|r| r.strip_suffix(" ms"))
        .and_then(|n| n.parse().ok())
        .expect("startup time line");
    assert_eq!(probe["artifact_names"], json!(["report"]));
    let resp = s
        .http
        .get(format!(
            "{}/api/v1{}/commits/{head}/artifacts/report",
            s.url,
            w.repo_path()
        ))
        .bearer_auth(&w.aiko)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let zip = resp.bytes().await.unwrap();
    let zip_path = w.dir.path().join("report.zip");
    std::fs::write(&zip_path, &zip).unwrap();
    let listing = Command::new("unzip")
        .arg("-l")
        .arg(&zip_path)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&listing.stdout).contains("out/report.txt"));

    let exit3 = by("exit3");
    let exit3_log = job_log(&w, &exit3).await;
    assert_eq!(exit3["conclusion"], "failure", "{exit3}\n{exit3_log}");
    assert!(
        exit3["details"].as_str().unwrap().contains("code 3"),
        "{exit3}"
    );
    assert!(
        exit3_log.contains("before") && !exit3_log.contains("never\n"),
        "{exit3_log}"
    );

    let slow = by("slow");
    assert_eq!(slow["conclusion"], "timed_out", "{slow}");
    // What the job wrote before it was stopped is kept, and was shown live.
    let slow_log = job_log(&w, &slow).await;
    assert!(slow_log.contains("before-timeout"), "{slow_log}");
    assert!(seen_running, "the running job's log stayed empty");
    let job: Value = s
        .ok(
            &format!(
                "{}/jobs/{}",
                w.repo_path(),
                slow["attempt_id"].as_str().unwrap()
            ),
            &w.aiko,
        )
        .await;
    let ran = chrono::DateTime::parse_from_rfc3339(job["finished_at"].as_str().unwrap()).unwrap()
        - chrono::DateTime::parse_from_rfc3339(job["started_at"].as_str().unwrap()).unwrap();
    assert!(ran.num_seconds() < 30, "the timeout stopped the job: {ran}");

    // Nothing is left behind: no sandbox record, no VM process.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let left = sandboxes(&runner_name);
    assert!(left.is_empty(), "sandboxes left: {left:?}");
    assert!(
        jobs_rss("sandbox", &names).is_empty(),
        "a job VM is still running"
    );
    println!(
        "MEASURE startup_ms={startup_ms} peak_vm_rss_kib={peak_rss} peak_msb_run_rss_kib={peak_msb_run} peak_runner_rss_kib={peak_runner} three_jobs_total_ms={}",
        total.as_millis()
    );
}
