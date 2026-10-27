//! The landing queue serializes gates without requiring authors to update heads.
use crate::common::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

async fn landing(w: &World, number: u64, state: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let p = w
            .server
            .ok(&format!("{}/pulls/{number}", w.repo_path()), &w.owner)
            .await;
        if p["landing"]["state"] == state {
            return p;
        }
        assert!(
            Instant::now() < deadline,
            "landing did not become {state}: {p}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn next_claim(runner: &crate::ci::Runner) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(claim) = runner.claim().await {
            return claim;
        }
        assert!(Instant::now() < deadline, "no queued gate job appeared");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn merge(w: &World, number: u64, p: &Value) -> Value {
    let (status, merged) = w
        .server
        .post(
            &format!("{}/pulls/{number}/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":p["head_sha"],"expected_base":p["base_sha"]}),
        )
        .await;
    assert_eq!(status, 200, "{merged}");
    merged
}

async fn divergent_gate(jobs: &[&str]) -> (World, crate::ci::Runner, Value) {
    let (w, runner, head) = divergent_feature(jobs).await;
    for _ in jobs {
        let claim = next_claim(&runner).await;
        assert_eq!(claim["sha"], head);
        assert_eq!(runner.finish(&claim, "success").await.0, 200);
    }
    let gating = landing(&w, 3, "gating").await;
    (w, runner, gating)
}

/// Pull request 3 (`feature`, at the returned head) behind an advanced base,
/// with `jobs` required and queued for its head.
async fn divergent_feature(jobs: &[&str]) -> (World, crate::ci::Runner, String) {
    use crate::ci::{Runner, pipeline, sync_main};
    let w = World::new().await;
    let runner = Runner::new(&w, "gate-runner").await;
    std::fs::create_dir_all(w.work.join(".branvil")).unwrap();
    w.push_change("pipeline", ".branvil/ci.toml", &pipeline(jobs));
    w.open_pull("pipeline", json!([])).await;
    runner.drain("success").await;
    merge(&w, 1, &landing(&w, 1, "passed").await).await;
    sync_main(&w);
    let head = w.push_change("feature", "feature.txt", "feature\n");
    sync_main(&w);
    w.push_change("advance", "advance.txt", "advance\n");
    w.open_pull("advance", json!([])).await;
    runner.drain("success").await;
    merge(&w, 2, &landing(&w, 2, "passed").await).await;
    let repo = w.server.ok(&w.repo_path(), &w.owner).await;
    let (status, result) = w
        .server
        .patch(
            &w.repo_path(),
            &w.owner,
            json!({"revision":repo["revision"],"required_checks":jobs}),
        )
        .await;
    assert_eq!(status, 200, "{result}");
    w.open_pull("feature", json!([])).await;
    (w, runner, head)
}

async fn retry(w: &World, attempt: &Value) {
    let (status, result) = w
        .server
        .post(
            &format!(
                "{}/jobs/{}/retry",
                w.repo_path(),
                attempt["attempt_id"].as_str().unwrap()
            ),
            &w.owner,
            json!({}),
        )
        .await;
    assert_eq!(status, 200, "{result}");
}

async fn ref_is(w: &World, name: &str, expected: Option<&str>) {
    let bare = w.server.data.join("repos/acme/demo.git");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = git(&bare, &["rev-parse", "--verify", name]);
        if match expected {
            Some(sha) => {
                output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == sha
            }
            None => !output.status.success(),
        } {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} did not become {expected:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn advancing_base_preserves_admission_order_and_waiting_timestamps() {
    let w = World::new().await;
    let first = w.push_change("first", "first.txt", "first\n");
    w.open_pull("first", json!([])).await;
    let leader = landing(&w, 1, "passed").await;
    git_ok(&w.work, &["checkout", "-q", "main"]);
    w.push_change("later", "later.txt", "later\n");
    let (status, later) = w
        .server
        .post(
            &format!("{}/pulls", w.repo_path()),
            &w.aiko,
            json!({"head":"later", "title":"Admitted later", "draft":true}),
        )
        .await;
    assert_eq!(status, 201, "{later}");
    git_ok(&w.work, &["checkout", "-q", "main"]);
    let next_head = w.push_change("next", "next.txt", "next\n");
    w.open_pull("next", json!([])).await;
    let next_before = landing(&w, 3, "waiting").await;
    let (status, result) = w
        .server
        .patch(
            &format!("{}/pulls/2", w.repo_path()),
            &w.owner,
            json!({"revision":later["revision"], "draft":false}),
        )
        .await;
    assert_eq!(status, 200, "{result}");
    let later_before = landing(&w, 2, "waiting").await;
    assert_eq!(next_before["landing"]["position"], 2);
    assert_eq!(later_before["landing"]["position"], 3);
    let (status, result) = w
        .server
        .post(
            &format!("{}/pulls/1/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":first, "expected_base":leader["base_sha"]}),
        )
        .await;
    assert_eq!(status, 200, "{result}");
    let next_after = landing(&w, 3, "passed").await;
    let later_after = landing(&w, 2, "waiting").await;
    for (before, after) in [(&next_before, &next_after), (&later_before, &later_after)] {
        assert_eq!(after["landing"]["sequence"], before["landing"]["sequence"]);
        assert_eq!(
            after["landing"]["queued_at"],
            before["landing"]["queued_at"]
        );
    }
    assert_eq!(next_after["landing"]["position"], 1);
    assert_eq!(later_after["landing"]["position"], 2);
    assert_eq!(next_after["landing"]["base_sha"], first);
    let gate = next_after["landing"]["gate"].as_str().unwrap();
    let bare = w.server.data.join("repos/acme/demo.git");
    assert_eq!(
        git_ok(&bare, &["show", "-s", "--format=%P", gate]),
        format!("{first} {next_head}")
    );
}

#[tokio::test]
async fn an_unrelated_base_advance_rebuilds_gating_and_passed_leaders_in_place() {
    for passed in [false, true] {
        let (mut w, runner, mut before) = divergent_gate(&["test"]).await;
        let old_claim = next_claim(&runner).await;
        if passed {
            assert_eq!(runner.finish(&old_claim, "success").await.0, 200);
            before = landing(&w, 3, "passed").await;
        }
        crate::ci::sync_main(&w);
        let next_head = w.push_change("next", "next.txt", "next\n");
        w.open_pull("next", json!([])).await;
        let claim = next_claim(&runner).await;
        assert_eq!(claim["sha"], next_head);
        assert_eq!(runner.finish(&claim, "success").await.0, 200);
        let waiting = landing(&w, 4, "waiting").await;
        crate::ci::sync_main(&w);
        let moved_base = w.push_change("outside", "outside.txt", "outside\n");
        let bare = w.server.data.join("repos/acme/demo.git");
        w.server.stop();
        git_ok(&bare, &["update-ref", "refs/heads/main", &moved_base]);
        w.restart(&[]);
        let deadline = Instant::now() + Duration::from_secs(15);
        let after = loop {
            let current = landing(&w, 3, "gating").await;
            if current["landing"]["base_sha"] == moved_base {
                break current;
            }
            assert!(
                Instant::now() < deadline,
                "leader did not rebuild for {moved_base}: {current}"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        };
        assert_eq!(after["landing"]["sequence"], before["landing"]["sequence"]);
        assert_eq!(
            after["landing"]["queued_at"],
            before["landing"]["queued_at"]
        );
        assert_eq!(after["landing"]["position"], 1);
        assert_eq!(after["landing"]["base_sha"], moved_base);
        assert_ne!(after["landing"]["gate"], before["landing"]["gate"]);
        let gate = after["landing"]["gate"].as_str().unwrap();
        assert_eq!(
            git_ok(&bare, &["show", "-s", "--format=%P", gate]),
            format!("{moved_base} {}", before["head_sha"].as_str().unwrap())
        );
        let waiting_after = landing(&w, 4, "waiting").await;
        assert_eq!(waiting_after["landing"]["position"], 2);
        assert_eq!(
            waiting_after["landing"]["sequence"],
            waiting["landing"]["sequence"]
        );
        assert_eq!(
            waiting_after["landing"]["queued_at"],
            waiting["landing"]["queued_at"]
        );
        let old_job = w
            .server
            .ok(
                &format!(
                    "{}/jobs/{}",
                    w.repo_path(),
                    old_claim["attempt_id"].as_str().unwrap()
                ),
                &w.owner,
            )
            .await;
        if passed {
            assert_eq!(old_job["conclusion"], "success");
        } else {
            assert_eq!(old_job["cancel_requested"], true, "{old_job}");
        }
    }
}

/// Ctrl-C (SIGINT to the service's process group) while Git holds a new
/// gate's ref in the landing task's update: the stop waits for it, and the
/// gate's ref is there with no lock before the next start repairs anything.
#[cfg(unix)]
#[tokio::test]
async fn ctrl_c_during_a_gate_update_waits_for_it() {
    let (mut w, runner, head) = divergent_feature(&["test"]).await;
    let hold = RefHold::new(&w, "refs/branvil/gate/3");
    let claim = next_claim(&runner).await;
    assert_eq!(claim["sha"], head);
    assert_eq!(runner.finish(&claim, "success").await.0, 200);
    hold.wait().await;
    w.server.interrupt_group();
    let line = w
        .server
        .line_within(Duration::from_secs(10))
        .expect("a stop that waits says so");
    assert!(line.starts_with("branvil stopping: "), "{line}");
    assert!(w.server.running(), "the stop waits for the gate update");
    hold.release();
    let status = w.server.exit_within(Duration::from_secs(10));
    assert!(status.is_some_and(|s| s.success()), "{status:?}");
    let bare = w.server.data.join("repos/acme/demo.git");
    assert!(!bare.join("refs/branvil/gate/3.lock").exists());
    let gate = git_ok(&bare, &["rev-parse", "refs/branvil/gate/3"]);

    w.restart(&[]);
    let gating = landing(&w, 3, "gating").await;
    assert_eq!(gating["landing"]["gate"], gate.as_str());
}

/// A stop signal while startup repairs a gate's ref (a hook holds its
/// update) does not end the service in the middle: startup finishes, the
/// service stops cleanly, and the gate's ref is back with no lock.
#[cfg(unix)]
#[tokio::test]
async fn a_stop_signal_during_startup_waits_for_it() {
    let (mut w, _runner, gating) = divergent_gate(&["test"]).await;
    let gate = gating["landing"]["gate"].as_str().unwrap().to_string();
    w.server.stop();
    let bare = w.server.data.join("repos/acme/demo.git");
    git_ok(&bare, &["update-ref", "-d", "refs/branvil/gate/3"]);
    let hold = RefHold::new(&w, "refs/branvil/gate/3");
    let signal = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !hold.reached() {
            assert!(Instant::now() < deadline, "startup never repaired the gate");
            std::thread::sleep(Duration::from_millis(10));
        }
        let kill = std::process::Command::new("kill")
            .args(["-INT", &hold.service_pid().to_string()])
            .status()
            .unwrap();
        assert!(kill.success());
        hold.release();
    });
    w.server = Server::start(w.dir.path());
    signal.join().unwrap();
    let status = w.server.exit_within(Duration::from_secs(10));
    assert!(status.is_some_and(|s| s.success()), "{status:?}");
    assert!(!bare.join("refs/branvil/gate/3.lock").exists());
    assert_eq!(git_ok(&bare, &["rev-parse", "refs/branvil/gate/3"]), gate);
}

#[tokio::test]
async fn failed_gate_refs_are_recovered_and_removed_when_their_inputs_or_pull_close() {
    for change in ["close", "head", "base"] {
        let (mut w, runner, gating) = divergent_gate(&["test"]).await;
        let claim = next_claim(&runner).await;
        assert_eq!(runner.finish(&claim, "failure").await.0, 200);
        landing(&w, 3, "left").await;
        let gate = gating["landing"]["gate"].as_str().unwrap();
        let bare = w.server.data.join("repos/acme/demo.git");
        ref_is(&w, "refs/branvil/failed-gate/3", Some(gate)).await;
        w.server.stop();
        git_ok(&bare, &["update-ref", "-d", "refs/branvil/failed-gate/3"]);
        git_ok(&bare, &["update-ref", "refs/branvil/failed-gate/999", gate]);
        w.restart(&[]);
        ref_is(&w, "refs/branvil/failed-gate/3", Some(gate)).await;
        ref_is(&w, "refs/branvil/failed-gate/999", None).await;
        match change {
            "close" => {
                let (status, result) = w
                    .server
                    .post(
                        &format!("{}/pulls/3/close", w.repo_path()),
                        &w.owner,
                        json!({}),
                    )
                    .await;
                assert_eq!(status, 200, "{result}");
            }
            "head" => {
                w.push_change("feature", "feature.txt", "updated\n");
            }
            "base" => {
                crate::ci::sync_main(&w);
                let base = w.push_change("outside", "outside.txt", "outside\n");
                w.server.stop();
                git_ok(&bare, &["update-ref", "refs/heads/main", &base]);
                w.restart(&[]);
            }
            _ => unreachable!(),
        }
        ref_is(&w, "refs/branvil/failed-gate/3", None).await;
        w.restart(&[]);
        ref_is(&w, "refs/branvil/failed-gate/3", None).await;
    }
}

#[tokio::test]
async fn retry_requires_cancelled_siblings_and_retains_the_gate_while_waiting_again() {
    let (w, runner, gating) = divergent_gate(&["first", "second"]).await;
    let failed = next_claim(&runner).await;
    assert_eq!(runner.finish(&failed, "failure").await.0, 200);
    let left = landing(&w, 3, "left").await;
    let deadline = Instant::now() + Duration::from_secs(15);
    let cancelled = loop {
        let checks = w
            .server
            .ok(&format!("{}/pulls/3/checks", w.repo_path()), &w.owner)
            .await;
        let cancelled = checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "gate/second")
            .unwrap();
        if cancelled["conclusion"] == "cancelled" {
            break cancelled.clone();
        }
        assert!(
            Instant::now() < deadline,
            "gate sibling was not cancelled: {checks}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    retry(&w, &failed).await;
    let retried = next_claim(&runner).await;
    assert_eq!(runner.finish(&retried, "success").await.0, 200);
    let still = landing(&w, 3, "left").await;
    assert_eq!(still["landing"]["sequence"], left["landing"]["sequence"]);
    let comments = w
        .server
        .ok(&format!("{}/pulls/3/comments", w.repo_path()), &w.owner)
        .await;
    assert_eq!(comments["items"].as_array().unwrap().len(), 1);
    let body = comments["items"][0]["body"].as_str().unwrap();
    assert!(body.contains("cancelled"), "{body}");
    crate::ci::sync_main(&w);
    let next = w.push_change("next", "next.txt", "next\n");
    w.open_pull("next", json!([])).await;
    for _ in 0..2 {
        let claim = next_claim(&runner).await;
        assert_eq!(claim["sha"], next);
        assert_eq!(runner.finish(&claim, "success").await.0, 200);
    }
    landing(&w, 4, "passed").await;
    retry(&w, &cancelled).await;
    let retried = next_claim(&runner).await;
    assert_eq!(runner.finish(&retried, "success").await.0, 200);
    let waiting = landing(&w, 3, "waiting").await;
    let gate = gating["landing"]["gate"].as_str().unwrap();
    assert_eq!(waiting["landing"]["gate"], gate);
    assert_eq!(waiting["landing"]["position"], 2);
    ref_is(&w, "refs/branvil/failed-gate/3", Some(gate)).await;
    merge(&w, 4, &landing(&w, 4, "passed").await).await;
    let rebuilt = landing(&w, 3, "gating").await;
    for _ in 0..2 {
        let claim = next_claim(&runner).await;
        assert_eq!(claim["sha"], rebuilt["landing"]["gate"]);
        assert_eq!(runner.finish(&claim, "success").await.0, 200);
    }
    let passed = landing(&w, 3, "passed").await;
    assert_ne!(
        passed["landing"]["gate"], gate,
        "the base moved before promotion"
    );
    ref_is(&w, "refs/branvil/failed-gate/3", None).await;
}

#[tokio::test]
async fn one_gate_per_base_and_a_stale_head_merges_the_gate_without_changing_its_reviewed_head() {
    let w = World::new().await;
    let first = w.push_change("first", "first.txt", "first\n");
    w.open_pull("first", json!([])).await;
    let passed = landing(&w, 1, "passed").await;
    assert_eq!(passed["landing"]["gate"], first);
    let bare = w.server.data.join("repos/acme/demo.git");
    assert!(
        !git(&bare, &["show-ref", "--verify", "refs/branvil/gate/1"])
            .status
            .success()
    );
    git_ok(&w.work, &["checkout", "-q", "main"]);
    let second = w.push_change("second", "second.txt", "second\n");
    w.open_pull("second", json!([])).await;
    let waiting = landing(&w, 2, "waiting").await;
    assert_eq!(waiting["landing"]["position"], 2);
    assert_eq!(waiting["landing"]["first"]["number"], 1);
    assert!(waiting["landing"]["gate"].is_null());
    let queue = w
        .server
        .ok(&format!("{}/landing", w.repo_path()), &w.aiko)
        .await;
    assert_eq!(queue["items"][0]["number"], 1);
    assert_eq!(queue["items"][1]["number"], 2);
    let (st, denied) = w
        .server
        .post(
            &format!("{}/pulls/2/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":second,"expected_base":waiting["base_sha"]}),
        )
        .await;
    assert_eq!(st, 409, "{denied}");
    let (st, merged) = w
        .server
        .post(
            &format!("{}/pulls/1/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":first,"expected_base":passed["base_sha"]}),
        )
        .await;
    assert_eq!(st, 200, "{merged}");
    let passed = landing(&w, 2, "passed").await;
    let gate = passed["landing"]["gate"].as_str().unwrap();
    assert_ne!(gate, second);
    assert_eq!(
        git_ok(&bare, &["show", "-s", "--format=%P", gate]),
        format!("{first} {second}")
    );
    let (st, merged) = w
        .server
        .post(
            &format!("{}/pulls/2/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":second,"expected_base":first}),
        )
        .await;
    assert_eq!(st, 200, "{merged}");
    assert_eq!(merged["merge_sha"], gate);
    assert_eq!(merged["head_sha"], second);
    assert_eq!(merged["landing"]["state"], "left");
    assert_eq!(merged["landing"]["reason"]["code"], "merged");
    assert_eq!(git_ok(&bare, &["rev-parse", "main"]), gate);
    assert!(
        !git(&bare, &["show-ref", "--verify", "refs/heads/second"])
            .status
            .success()
    );
    assert!(
        !git(&bare, &["show-ref", "--verify", "refs/branvil/gate/2"])
            .status
            .success()
    );
}

#[tokio::test]
async fn conflicts_leave_a_check_and_a_branvil_comment_without_rejoining_until_the_head_moves() {
    let w = World::new().await;
    w.push_change("feature", "README.md", "feature\n");
    git_ok(&w.work, &["checkout", "-q", "main"]);
    let base_head = w.push_change("base-change", "README.md", "main\n");
    let base_pull = w.open_pull("base-change", json!([])).await;
    let (st, merged) = w
        .server
        .post(
            &format!("{}/pulls/1/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":base_head,"expected_base":base_pull["base_sha"]}),
        )
        .await;
    assert_eq!(st, 200, "{merged}");
    w.open_pull("feature", json!([])).await;
    let p = landing(&w, 2, "left").await;
    assert_eq!(p["landing"]["reason"]["code"], "conflict");
    assert_eq!(p["landing"]["reason"]["files"], json!(["README.md"]));
    let checks = w
        .server
        .ok(&format!("{}/pulls/2/checks", w.repo_path()), &w.aiko)
        .await;
    assert!(
        checks["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "gate" && c["conclusion"] == "failure")
    );
    let comments = w
        .server
        .ok(&format!("{}/pulls/2/comments", w.repo_path()), &w.aiko)
        .await;
    assert_eq!(comments["items"].as_array().unwrap().len(), 1);
    assert_eq!(comments["items"][0]["author"], "branvil");
    assert!(
        comments["items"][0]["body"]
            .as_str()
            .unwrap()
            .contains("README.md")
    );
    let (st, _) = w
        .server
        .post(
            &format!("{}/pulls/2/comments", w.repo_path()),
            &w.aiko,
            json!({"body":"Still waiting"}),
        )
        .await;
    assert_eq!(st, 201);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let still = landing(&w, 2, "left").await;
    assert_eq!(still["landing"]["queued_at"], p["landing"]["queued_at"]);
    let (st, rejected) = w
        .server
        .post(
            "/members",
            &w.owner,
            json!({"name":"BrAnViL", "kind":"agent"}),
        )
        .await;
    assert_eq!(st, 400, "{rejected}");
}

#[tokio::test]
async fn interrupted_gate_ref_creation_recovers_from_the_queue_record() {
    for point in ["gate_before_update_ref", "gate_after_update_ref"] {
        let mut w = World::new().await;
        let head = w.push_change("feature", "feature.txt", "feature\n");
        git_ok(&w.work, &["checkout", "-q", "main"]);
        let advance = w.push_change("advance", "advance.txt", "advance\n");
        let p = w.open_pull("advance", json!([])).await;
        let (st, result) = w
            .server
            .post(
                &format!("{}/pulls/1/merge", w.repo_path()),
                &w.owner,
                json!({"expected_head":advance,"expected_base":p["base_sha"]}),
            )
            .await;
        assert_eq!(st, 200, "{result}");
        let (st, _) = w
            .server
            .post(
                &format!("{}/pulls", w.repo_path()),
                &w.aiko,
                json!({"head":"feature", "title":"Recover gate", "draft":true}),
            )
            .await;
        assert_eq!(st, 201);
        w.restart(&[("BRANVIL_FAILPOINT", point)]);
        let p = w
            .server
            .ok(&format!("{}/pulls/2", w.repo_path()), &w.owner)
            .await;
        let _ = w
            .server
            .http
            .patch(format!("{}/api/v1{}/pulls/2", w.server.url, w.repo_path()))
            .bearer_auth(&w.owner)
            .json(&json!({"revision":p["revision"], "draft":false}))
            .send()
            .await;
        assert!(!w.server.wait_exit().success());
        w.restart(&[]);
        let p = landing(&w, 2, "passed").await;
        assert_eq!(p["head_sha"], head);
        let gate = p["landing"]["gate"].as_str().unwrap();
        assert_ne!(gate, head);
        let bare = w.server.data.join("repos/acme/demo.git");
        assert_eq!(git_ok(&bare, &["rev-parse", "refs/branvil/gate/2"]), gate);
    }
}

#[tokio::test]
async fn failed_gates_wait_for_a_successful_retry_and_passed_gates_recheck_retries() {
    let (w, runner, gating) = divergent_gate(&["test"]).await;
    let head = &gating["head_sha"];
    let advance = &gating["base_sha"];
    let claim = next_claim(&runner).await;
    assert_eq!(claim["sha"], gating["landing"]["gate"]);
    assert_eq!(runner.finish(&claim, "failure").await.0, 200);
    let left = landing(&w, 3, "left").await;
    assert_eq!(left["landing"]["reason"]["code"], "gate_failed");
    // Departure is saved before the next ref is made and the old one removed.
    ref_is(
        &w,
        "refs/branvil/failed-gate/3",
        gating["landing"]["gate"].as_str(),
    )
    .await;
    ref_is(&w, "refs/branvil/gate/3", None).await;
    let _ = w
        .server
        .post(
            &format!("{}/pulls/3/comments", w.repo_path()),
            &w.aiko,
            json!({"body":"Retry next"}),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        landing(&w, 3, "left").await["landing"]["sequence"],
        left["landing"]["sequence"]
    );
    let (st, result) = w
        .server
        .post(
            &format!(
                "{}/jobs/{}/retry",
                w.repo_path(),
                claim["attempt_id"].as_str().unwrap()
            ),
            &w.owner,
            json!({}),
        )
        .await;
    assert_eq!(st, 200, "{result}");
    let retry = next_claim(&runner).await;
    assert_eq!(runner.finish(&retry, "success").await.0, 200);
    let passed = landing(&w, 3, "passed").await;
    assert_eq!(passed["landing"]["gate"], gating["landing"]["gate"]);
    ref_is(&w, "refs/branvil/failed-gate/3", None).await;
    ref_is(
        &w,
        "refs/branvil/gate/3",
        gating["landing"]["gate"].as_str(),
    )
    .await;
    assert!(
        passed["landing"]["sequence"].as_u64().unwrap()
            > left["landing"]["sequence"].as_u64().unwrap()
    );
    let (st, result) = w
        .server
        .post(
            &format!(
                "{}/jobs/{}/retry",
                w.repo_path(),
                retry["attempt_id"].as_str().unwrap()
            ),
            &w.owner,
            json!({}),
        )
        .await;
    assert_eq!(st, 200, "{result}");
    landing(&w, 3, "gating").await;
    let (st, denied) = w
        .server
        .post(
            &format!("{}/pulls/3/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":head,"expected_base":advance}),
        )
        .await;
    assert_eq!(st, 409, "{denied}");
    let retry = next_claim(&runner).await;
    assert_eq!(runner.finish(&retry, "failure").await.0, 200);
    assert_eq!(
        landing(&w, 3, "left").await["landing"]["reason"]["code"],
        "gate_failed"
    );
}

#[tokio::test]
async fn queue_api_exposes_titles() {
    let w = World::new().await;
    for n in 1..=2 {
        git_ok(&w.work, &["checkout", "-q", "main"]);
        w.push_change(&format!("feature-{n}"), &format!("{n}.txt"), "change");
        w.open_pull(&format!("feature-{n}"), json!([])).await;
    }
    landing(&w, 2, "waiting").await;
    let p = landing(&w, 1, "passed").await;
    assert_eq!(p["landing"]["title"], p["title"]);
    assert_eq!(p["landing"]["first"]["next"], "owner_merge");
    assert!(p["landing"].get("published").is_none());
    let queue = w
        .server
        .ok(&format!("{}/landing", w.repo_path()), &w.owner)
        .await;
    assert_eq!(queue["items"][0], p["landing"]);
}
