//! Restart and crash recovery: redb reopened from disk, journaled merges
//! interrupted between Git and the database, and pushes whose post-receive
//! never reached the database. Crashes are simulated with
//! `BRANVIL_FAILPOINT`, which aborts the process at a named point. This is
//! process-kill testing, not power-loss testing.

use crate::common;

use common::*;
use serde_json::{Value, json};

async fn ready_pull(w: &World) -> (String, String, String) {
    let s = &w.server;
    let t = w.ticket("Recover").await;
    let one = format!("/repos/{}/tickets/1", encode(REPO));
    s.put(
        &format!("{one}/board"),
        &w.aiko,
        json!({"lane": "in_progress"}),
    )
    .await;
    let head = w.push_change("feature", "f.txt", "one\n");
    let p = w
        .open_pull("feature", json!([{"url": t["url"], "closes": true}]))
        .await;
    let base = format!("{}/pulls/{}", w.repo_path(), p["number"]);
    (base, head, p["base_sha"].as_str().unwrap().to_string())
}

fn bare_main(w: &World) -> String {
    let bare = w.server.data.join("repos/acme/demo.git");
    git_ok(
        &bare,
        &[
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "refs/heads/main",
        ],
    )
}

async fn pending_ops(w: &World) -> u64 {
    w.server.ok("/admin/diagnostics", &w.owner).await["storage"]["pending_ref_operations"]
        .as_u64()
        .unwrap()
}

#[tokio::test]
async fn records_survive_a_restart() {
    let mut w = World::new().await;
    let t = w.ticket("Persist").await;
    w.push_change("feature", "f.txt", "x");
    let p = w
        .open_pull("feature", json!([{"url": t["url"], "closes": true}]))
        .await;
    w.server
        .post(
            &format!("{}/pulls/{}/comments", w.repo_path(), p["number"]),
            &w.ken,
            json!({"body": "kept"}),
        )
        .await;
    w.restart(&[]);
    let s = &w.server;
    let ticket = s
        .ok(&format!("/repos/{}/tickets/1", encode(REPO)), &w.aiko)
        .await;
    assert_eq!(ticket["title"], "Persist");
    let pull = s.ok(&format!("{}/pulls/1", w.repo_path()), &w.aiko).await;
    assert_eq!(pull["tickets"][0]["closes"], true);
    let comments = s
        .ok(&format!("{}/pulls/1/comments", w.repo_path()), &w.aiko)
        .await;
    assert_eq!(comments["items"][0]["body"], "kept");
    // Tokens still work, numbering continues.
    assert_eq!(w.ticket("Next").await["key"], "2");
}

#[tokio::test]
async fn a_merge_interrupted_after_the_ref_update_completes_at_startup() {
    let mut w = World::new().await;
    let (base, head, main) = ready_pull(&w).await;
    w.restart(&[("BRANVIL_FAILPOINT", "merge_after_update_ref")]);
    let url = format!("{}/api/v1{base}/merge", w.server.url);
    let result = w
        .server
        .http
        .post(url)
        .bearer_auth(&w.owner)
        .json(&json!({"expected_head": head, "expected_base": main}))
        .send()
        .await;
    assert!(result.is_err() || !result.unwrap().status().is_success());
    assert!(
        !w.server.wait_exit().success(),
        "the failpoint aborts the service"
    );
    // Git moved; the database did not record it yet.
    assert_eq!(bare_main(&w), head);

    w.restart(&[]);
    let s = &w.server;
    let pull = s.ok(&base, &w.aiko).await;
    assert_eq!(pull["merged"], true, "{pull}");
    assert_eq!(pull["merge_sha"], head.as_str());
    let ticket = s
        .ok(&format!("/repos/{}/tickets/1", encode(REPO)), &w.aiko)
        .await;
    assert_eq!(ticket["state"], "closed");
    assert_eq!(ticket["lane"], "done");
    assert_eq!(pending_ops(&w).await, 0);
}

#[tokio::test]
async fn a_merge_interrupted_before_the_ref_update_is_dropped_at_startup() {
    let mut w = World::new().await;
    let (base, head, main) = ready_pull(&w).await;
    w.restart(&[("BRANVIL_FAILPOINT", "merge_before_update_ref")]);
    let _ = w
        .server
        .http
        .post(format!("{}/api/v1{base}/merge", w.server.url))
        .bearer_auth(&w.owner)
        .json(&json!({"expected_head": head, "expected_base": main}))
        .send()
        .await;
    assert!(!w.server.wait_exit().success());
    assert_eq!(bare_main(&w), main);

    w.restart(&[]);
    let s = &w.server;
    let pull = s.ok(&base, &w.aiko).await;
    assert_eq!(pull["merged"], false);
    assert_eq!(pull["state"], "open");
    assert_eq!(pending_ops(&w).await, 0);
    // The merge can be retried and succeeds.
    let (st, v) = s
        .post(
            &format!("{base}/merge"),
            &w.owner,
            json!({"expected_head": head, "expected_base": main}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
}

#[tokio::test]
async fn a_push_the_service_did_not_record_is_found_at_startup() {
    let mut w = World::new().await;
    w.push_change("feature", "f.txt", "one\n");
    let p = w.open_pull("feature", json!([])).await;
    let before: Value = w
        .server
        .ok(&format!("{}/pulls/1", w.repo_path()), &w.aiko)
        .await;
    w.restart(&[("BRANVIL_FAILPOINT", "post_receive_before_record")]);
    // The push lands in Git; the service aborts before recording it.
    let w2 = &w;
    let file = w2.work.join("f.txt");
    std::fs::write(&file, "one\ntwo\n").unwrap();
    git_ok(&w.work, &["commit", "-qam", "unrecorded"]);
    let _ = git_as(
        &w.work,
        "aiko",
        &w.aiko,
        &["push", "-q", "origin", "HEAD:refs/heads/feature"],
    );
    assert!(!w.server.wait_exit().success());
    let new_tip = git_ok(&w.work, &["rev-parse", "HEAD"]);

    w.restart(&[]);
    let after = w
        .server
        .ok(&format!("{}/pulls/1", w.repo_path()), &w.aiko)
        .await;
    assert_eq!(after["head_sha"], new_tip.as_str());
    assert_ne!(after["head_sha"], p["head_sha"]);
    assert!(
        after["updated_at"].as_str().unwrap() > before["updated_at"].as_str().unwrap(),
        "the startup rescan recorded the new head as activity"
    );
}
