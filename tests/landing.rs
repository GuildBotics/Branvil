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
    use crate::ci::{Runner, pipeline, sync_main};
    let w = World::new().await;
    let runner = Runner::new(&w, "gate-runner").await;
    std::fs::create_dir_all(w.work.join(".branvil")).unwrap();
    let pipeline_head = w.push_change("pipeline", ".branvil/ci.toml", &pipeline(&["test"]));
    let p = w.open_pull("pipeline", json!([])).await;
    runner.drain("success").await;
    let (st, result) = w
        .server
        .post(
            &format!("{}/pulls/1/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":pipeline_head,"expected_base":p["base_sha"]}),
        )
        .await;
    assert_eq!(st, 200, "{result}");
    sync_main(&w);
    let head = w.push_change("feature", "feature.txt", "feature\n");
    sync_main(&w);
    let advance = w.push_change("advance", "advance.txt", "advance\n");
    let p = w.open_pull("advance", json!([])).await;
    runner.drain("success").await;
    let (st, result) = w
        .server
        .post(
            &format!("{}/pulls/2/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head":advance,"expected_base":p["base_sha"]}),
        )
        .await;
    assert_eq!(st, 200, "{result}");
    let repo = w.server.ok(&w.repo_path(), &w.owner).await;
    let (st, result) = w
        .server
        .patch(
            &w.repo_path(),
            &w.owner,
            json!({"revision":repo["revision"],"required_checks":["test"]}),
        )
        .await;
    assert_eq!(st, 200, "{result}");
    w.open_pull("feature", json!([])).await;
    let claim = next_claim(&runner).await;
    assert_eq!(claim["sha"], head);
    assert_eq!(runner.finish(&claim, "success").await.0, 200);
    let gating = landing(&w, 3, "gating").await;
    let claim = next_claim(&runner).await;
    assert_eq!(claim["sha"], gating["landing"]["gate"]);
    assert_eq!(runner.finish(&claim, "failure").await.0, 200);
    let left = landing(&w, 3, "left").await;
    assert_eq!(left["landing"]["reason"]["code"], "gate_failed");
    let bare = w.server.data.join("repos/acme/demo.git");
    assert_eq!(
        git_ok(&bare, &["rev-parse", "refs/branvil/failed-gate/3"]),
        gating["landing"]["gate"].as_str().unwrap()
    );
    assert!(
        !git(&bare, &["show-ref", "--verify", "refs/branvil/gate/3"])
            .status
            .success()
    );
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
