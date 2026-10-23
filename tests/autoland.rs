//! Landing pull requests automatically against a real service: a repository
//! with `auto_land` merges (or, with an upstream, publishes) an open pull
//! request that is not a draft once its required approvals and checks are
//! there, publishes one once, tells the webhook of an automatic publish once,
//! and keeps the webhook's URL a secret. Also the upgrade from version 13.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode as Status;
use axum::routing::post;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::ci::{Runner, eventually, pipeline, sync_main};
use crate::common::*;
use crate::pulls::review;
use crate::upstream::{
    GH, branch_on_github, commit_on_github, followed, publish, refresh, with_aiko_account,
};

const OTHER: &str = "acme/other";

/// Changes `acme/demo`'s settings (at its current revision).
async fn settings(w: &World, patch: Value) -> Value {
    let repo = w.server.ok(&w.repo_path(), &w.owner).await;
    let mut body = patch;
    body["revision"] = repo["revision"].clone();
    let (st, v) = w.server.patch(&w.repo_path(), &w.owner, body).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v
}

async fn pull(w: &World, n: u64) -> Value {
    w.server
        .ok(&format!("{}/pulls/{n}", w.repo_path()), &w.owner)
        .await
}

async fn merged(w: &World, n: u64) {
    eventually(&format!("!{n} merged"), || async {
        pull(w, n).await["merged"] == true
    })
    .await;
}

fn number(p: &Value) -> u64 {
    p["number"].as_u64().unwrap()
}

/// `acme/other` with a `main` of its own, landing by itself when
/// `auto_land`; returns its clone.
async fn other_repo(w: &World, auto_land: bool) -> PathBuf {
    let s = &w.server;
    let (st, v) = s.post("/repos", &w.owner, json!({"path": OTHER})).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let (st, v) = s
        .patch(
            &format!("/repos/{}", encode(OTHER)),
            &w.owner,
            json!({"revision": 1, "required_approvals": 0, "auto_land": auto_land}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let dir = w.dir.path().join("other");
    std::fs::create_dir_all(&dir).unwrap();
    git_ok(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "other\n").unwrap();
    git_ok(&dir, &["add", "README.md"]);
    git_ok(&dir, &["commit", "-qm", "initial"]);
    git_ok(&dir, &["remote", "add", "origin", &s.remote(OTHER)]);
    git_as_ok(&dir, "alice", &w.owner, &["push", "-q", "origin", "main"]);
    dir
}

/// Returns once everything that changed before was swept: a pull request
/// opened in `acme/other` (`other_repo` landing by itself) lands, and one
/// task sweeps the changes in the order they came.
async fn swept(w: &World, other: &Path) {
    let k = git_ok(other, &["rev-list", "--count", "HEAD"]);
    let branch = format!("s{k}");
    std::fs::write(other.join("s.txt"), &k).unwrap();
    git_ok(other, &["add", "s.txt"]);
    git_ok(other, &["commit", "-qm", &branch]);
    git_as_ok(
        other,
        "aiko",
        &w.aiko,
        &["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")],
    );
    let pulls = format!("/repos/{}/pulls", encode(OTHER));
    let (st, v) = w
        .server
        .post(&pulls, &w.aiko, json!({"head": branch, "title": "Sweep"}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let n = number(&v["pull"]);
    eventually("the sweep", || async {
        w.server.ok(&format!("{pulls}/{n}"), &w.owner).await["merged"] == true
    })
    .await;
}

/// A Slack-like Incoming Webhook: keeps what it receives and answers with
/// `answers` in turn, then 200.
struct Hook {
    received: Arc<Mutex<Vec<(u16, Value)>>>,
}

type HookState = (Arc<Mutex<Vec<(u16, Value)>>>, Arc<Mutex<VecDeque<u16>>>);

async fn hook_receive(
    State((received, answers)): State<HookState>,
    axum::Json(body): axum::Json<Value>,
) -> Status {
    let status = answers.lock().unwrap().pop_front().unwrap_or(200);
    received.lock().unwrap().push((status, body));
    Status::from_u16(status).unwrap()
}

impl Hook {
    async fn start(w: &World, answers: &[u16]) -> Hook {
        let received = Arc::new(Mutex::new(Vec::new()));
        let state: HookState = (
            received.clone(),
            Arc::new(Mutex::new(answers.iter().copied().collect())),
        );
        let app = axum::Router::new()
            .route("/services/T0/hook-9f3c", post(hook_receive))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/services/T0/hook-9f3c",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let (st, v) = w
            .server
            .put("/admin/webhook", &w.owner, json!({"url": url}))
            .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        Hook { received }
    }

    fn received(&self) -> Vec<(u16, Value)> {
        self.received.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn a_pull_request_merges_itself_once_approved_and_its_required_check_succeeded() {
    let w = World::new().await;
    let runner = Runner::new(&w, "runner1").await;
    settings(
        &w,
        json!({"auto_land": true, "required_checks": ["test"], "required_approvals": 1}),
    )
    .await;
    std::fs::create_dir_all(w.work.join(".branvil")).unwrap();
    std::fs::write(w.work.join(".branvil/ci.toml"), pipeline(&["test"])).unwrap();
    git_ok(&w.work, &["add", ".branvil/ci.toml"]);

    // The approval first, then the check.
    let head = w.push_change("one", "one.txt", "1\n");
    let n = number(&w.open_pull("one", json!([])).await);
    let at = format!("{}/pulls/{n}", w.repo_path());
    assert_eq!(review(&w, &at, &w.ken, "approve", &head).await, 201);
    // `test` has not run, so nothing can land.
    assert_eq!(pull(&w, n).await["state"], "open");
    assert!(runner.drain("success").await >= 1);
    merged(&w, n).await;
    assert_eq!(pull(&w, n).await["merge_sha"], head.as_str());

    // The check first, then the approval.
    sync_main(&w);
    let head = w.push_change("two", "two.txt", "2\n");
    let n = number(&w.open_pull("two", json!([])).await);
    let at = format!("{}/pulls/{n}", w.repo_path());
    assert!(runner.drain("success").await >= 1);
    // 0 of 1 approvals, so nothing can land.
    assert_eq!(pull(&w, n).await["state"], "open");
    assert_eq!(review(&w, &at, &w.ken, "approve", &head).await, 201);
    merged(&w, n).await;
}

#[tokio::test]
async fn a_draft_lands_only_once_it_is_marked_ready() {
    let w = World::new().await;
    let other = other_repo(&w, true).await;
    settings(&w, json!({"auto_land": true})).await;
    w.push_change("draft", "d.txt", "d\n");
    let (st, v) = w
        .server
        .post(
            &format!("{}/pulls", w.repo_path()),
            &w.aiko,
            json!({"head": "draft", "title": "Draft", "draft": true}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let n = number(&v["pull"]);
    swept(&w, &other).await;
    assert_eq!(pull(&w, n).await["state"], "open");
    let (st, v) = w
        .server
        .patch(
            &format!("{}/pulls/{n}", w.repo_path()),
            &w.owner,
            json!({"draft": false}),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    merged(&w, n).await;
}

#[tokio::test]
async fn only_the_owner_turns_landing_on_and_it_lands_what_waits_even_at_start() {
    let mut w = World::new().await;
    let other = other_repo(&w, true).await;
    // Off: a pull request that can land stays.
    let n = {
        w.push_change("one", "one.txt", "1\n");
        number(&w.open_pull("one", json!([])).await)
    };
    swept(&w, &other).await;
    assert_eq!(pull(&w, n).await["state"], "open");
    let repo = w.server.ok(&w.repo_path(), &w.owner).await;
    let (st, _) = w
        .server
        .patch(
            &w.repo_path(),
            &w.aiko,
            json!({"revision": repo["revision"], "auto_land": true}),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // On: it lands.
    assert_eq!(
        settings(&w, json!({"auto_land": true})).await["auto_land"],
        true
    );
    merged(&w, n).await;

    // A pull request that can land while the service is down lands at start.
    settings(&w, json!({"auto_land": false})).await;
    sync_main(&w);
    w.push_change("two", "two.txt", "2\n");
    let n = number(&w.open_pull("two", json!([])).await);
    w.server.stop();
    edit_records(&w.server.data.join("branvil.redb"), "repos", |r| {
        if r["path"] == REPO {
            r["auto_land"] = json!(true);
        }
    });
    w.restart(&[]);
    merged(&w, n).await;
}

#[tokio::test]
async fn a_head_behind_its_base_waits_until_it_is_updated() {
    let w = World::new().await;
    let other = other_repo(&w, true).await;
    settings(&w, json!({"auto_land": true})).await;
    w.push_change("b", "b.txt", "b\n");
    git_ok(&w.work, &["checkout", "-q", "main"]);
    w.push_change("a", "a.txt", "a\n");
    merged(&w, number(&w.open_pull("a", json!([])).await)).await;
    // b does not contain main's new tip.
    let n = number(&w.open_pull("b", json!([])).await);
    swept(&w, &other).await;
    assert_eq!(pull(&w, n).await["state"], "open");
    git_as_ok(&w.work, "alice", &w.owner, &["fetch", "-q", "origin"]);
    git_ok(&w.work, &["checkout", "-q", "b"]);
    git_ok(&w.work, &["rebase", "-q", "origin/main"]);
    git_as_ok(
        &w.work,
        "aiko",
        &w.aiko,
        &["push", "-q", "origin", "+HEAD:refs/heads/b"],
    );
    merged(&w, n).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pull_request_publishes_itself_as_its_author_and_the_webhook_is_told_once() {
    let (w, gh) = followed().await;
    let hook = Hook::start(&w, &[]).await;
    let other = other_repo(&w, true).await;
    settings(&w, json!({"auto_land": true})).await;
    let head = w.push_change("feat", "f.txt", "one\n");
    let n = number(&w.open_pull("feat", json!([])).await);
    // Its author has no GitHub account yet; once it has, it is published.
    swept(&w, &other).await;
    assert!(pull(&w, n).await["upstream"].is_null());
    with_aiko_account(&w).await;
    eventually("the publish", || async {
        pull(&w, n).await["upstream"]["number"].is_u64()
    })
    .await;
    let p = pull(&w, n).await;
    let x = &p["upstream"];
    assert_eq!((&x["auto"], &x["account"]), (&json!(true), &json!("aiko")));
    let there = gh.pull(x["number"].as_u64().unwrap());
    assert_eq!(there.author, "aiko-bot");
    assert_eq!(gh.tip(GH, "feat").as_deref(), Some(head.as_str()));
    eventually("the notice", || async {
        pull(&w, n).await["upstream"]["notified_at"].is_string()
    })
    .await;
    let received = hook.received();
    assert_eq!(received.len(), 1, "{received:?}");
    let text = received[0].1["text"].as_str().unwrap();
    for part in [
        p["url"].as_str().unwrap(),
        x["url"].as_str().unwrap(),
        "PR feat",
    ] {
        assert!(text.contains(part), "{text}");
    }

    // Later changes are carried to the host; the webhook hears nothing more.
    w.push_change("feat", "f.txt", "two\n");
    refresh(&w).await;
    swept(&w, &other).await;
    assert_eq!(hook.received().len(), 1);
    assert_eq!(pull(&w, n).await["state"], "open", "people merge it there");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_automatic_publish_the_host_refuses_is_left_to_the_owner() {
    let (w, gh) = followed().await;
    with_aiko_account(&w).await;
    let other = other_repo(&w, true).await;
    // GitHub has a branch feat that Branvil did not push.
    let main = gh.tip(GH, "main").unwrap();
    w.push_change("feat", "f.txt", "one\n");
    let theirs = commit_on_github(&gh, "main", "someone's work");
    branch_on_github(&gh, "feat", &theirs);
    branch_on_github(&gh, "main", &main);
    settings(&w, json!({"auto_land": true})).await;
    let n = number(&w.open_pull("feat", json!([])).await);
    eventually("the refusal", || async {
        pull(&w, n).await["upstream"]["stopped"].is_string()
    })
    .await;
    assert_eq!(pull(&w, n).await["upstream"]["auto"], true);
    assert_eq!(gh.tip(GH, "feat").as_deref(), Some(theirs.as_str()));
    assert!(gh.pulls(GH).is_empty());

    // Nothing automatic replaces it, whatever comes later.
    let ours = w.push_change("feat", "f.txt", "two\n");
    refresh(&w).await;
    swept(&w, &other).await;
    assert!(pull(&w, n).await["upstream"]["stopped"].is_string());
    assert_eq!(gh.tip(GH, "feat").as_deref(), Some(theirs.as_str()));
    assert!(gh.pulls(GH).is_empty());

    // The owner's publish replaces it.
    let (st, v) = publish(&w, n).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["upstream"]["auto"], false);
    assert_eq!(gh.tip(GH, "feat").as_deref(), Some(ours.as_str()));
    assert_eq!(gh.pulls(GH).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_notice_is_sent_again_and_a_manual_publish_tells_nobody() {
    let (w, _gh) = followed().await;
    with_aiko_account(&w).await;
    let hook = Hook::start(&w, &[500]).await;
    let other = other_repo(&w, true).await;
    settings(&w, json!({"auto_land": true})).await;
    w.push_change("feat", "f.txt", "one\n");
    let n = number(&w.open_pull("feat", json!([])).await);
    eventually("the failure", || async {
        w.server.ok("/admin/webhook", &w.owner).await["problem"].is_string()
    })
    .await;
    let problem = w.server.ok("/admin/webhook", &w.owner).await["problem"].clone();
    assert!(problem.as_str().unwrap().contains("500"), "{problem}");
    assert!(!problem.to_string().contains("hook-9f3c"), "{problem}");
    assert!(pull(&w, n).await["upstream"]["notified_at"].is_null());
    // The next change of the repository sends it again.
    let (st, _) = w
        .server
        .post(
            &format!("{}/pulls/{n}/comments", w.repo_path()),
            &w.ken,
            json!({"body": "looks good"}),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    eventually("the notice", || async {
        pull(&w, n).await["upstream"]["notified_at"].is_string()
    })
    .await;
    assert!(w.server.ok("/admin/webhook", &w.owner).await["problem"].is_null());
    let received = hook.received();
    assert_eq!(
        received.iter().map(|r| r.0).collect::<Vec<_>>(),
        [500, 200],
        "delivered once"
    );
    assert_eq!(received[0].1, received[1].1);

    // The owner's publish tells nobody.
    settings(&w, json!({"auto_land": false})).await;
    git_ok(&w.work, &["checkout", "-q", "main"]);
    w.push_change("two", "two.txt", "2\n");
    let m = number(&w.open_pull("two", json!([])).await);
    let (st, v) = publish(&w, m).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["upstream"]["auto"], false);
    swept(&w, &other).await;
    assert_eq!(hook.received().len(), 2);
    assert!(pull(&w, m).await["upstream"]["notified_at"].is_null());
}

#[tokio::test]
async fn the_webhook_url_is_a_secret_shown_only_by_its_host() {
    let w = World::new().await;
    let s = &w.server;
    let unset = json!({"set": false, "host": null, "problem": null});
    assert_eq!(s.ok("/admin/webhook", &w.owner).await, unset);
    let secret = "hook-2b8d";
    let url = format!("https://hooks.example.test/services/T0/{secret}");
    let (st, _) = s.get("/admin/webhook", &w.aiko).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s.put("/admin/webhook", &w.aiko, json!({"url": url})).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for bad in ["ftp://hooks.example.test/x", "not a url"] {
        let (st, v) = s.put("/admin/webhook", &w.owner, json!({"url": bad})).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(!v.to_string().contains(bad), "{v}");
    }
    let set = json!({"set": true, "host": "hooks.example.test", "problem": null});
    let (st, v) = s.put("/admin/webhook", &w.owner, json!({"url": url})).await;
    assert_eq!((st, &v), (StatusCode::OK, &set));
    assert_eq!(s.ok("/admin/webhook", &w.owner).await, set);
    let file = s.data.join("secrets/webhook.url");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), url);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let (st, v) = s.delete("/admin/webhook", &w.owner).await;
    assert_eq!((st, &v), (StatusCode::OK, &unset));
    assert!(!file.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_upgrade_from_version_13_lands_nothing_by_itself_and_told_nobody() {
    let (mut w, _gh) = followed().await;
    with_aiko_account(&w).await;
    // A published pull request, and one that is not.
    w.push_change("feat", "f.txt", "one\n");
    let published = number(&w.open_pull("feat", json!([])).await);
    assert_eq!(publish(&w, published).await.0, StatusCode::OK);
    git_ok(&w.work, &["checkout", "-q", "main"]);
    w.push_change("plain", "p.txt", "p\n");
    w.open_pull("plain", json!([])).await;
    // A card, whose index entry the failing check below takes away.
    w.ticket("One").await;
    // A second repository, with a merge interrupted after its ref update.
    let other = other_repo(&w, false).await;
    git_ok(&other, &["checkout", "-q", "-b", "fix"]);
    std::fs::write(other.join("fix.txt"), "fix\n").unwrap();
    git_ok(&other, &["add", "fix.txt"]);
    git_ok(&other, &["commit", "-qm", "fix"]);
    git_as_ok(&other, "aiko", &w.aiko, &["push", "-q", "origin", "fix"]);
    let at = format!("/repos/{}/pulls", encode(OTHER));
    let (st, v) = w
        .server
        .post(&at, &w.aiko, json!({"head": "fix", "title": "Fix"}))
        .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let interrupted = format!("{at}/{}", number(&v["pull"]));
    let input =
        json!({"expected_head": v["pull"]["head_sha"], "expected_base": v["pull"]["base_sha"]});
    w.restart(&[("BRANVIL_FAILPOINT", "merge_after_update_ref")]);
    let _ = w
        .server
        .http
        .post(format!("{}/api/v1{interrupted}/merge", w.server.url))
        .bearer_auth(&w.owner)
        .json(&input)
        .send()
        .await;
    assert!(!w.server.wait_exit().success());

    let data = w.server.data.clone();
    let db = data.join("branvil.redb");
    as_schema_13(&db);
    let before = tables(&db);
    let old_repos = records(&db, "repos");
    let old_pulls = records(&db, "pulls");
    let old_journal = records(&db, "ref_journal");
    assert_eq!((old_repos.len(), old_journal.len()), (2, 1));
    let admin = |what: &str| {
        let out = std::process::Command::new(BIN)
            .args(["admin", what, "--data-dir"])
            .arg(&data)
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.code().unwrap(), text)
    };
    let (code, text) = admin("check");
    assert!(
        code != 0 && text.contains("branvil admin upgrade"),
        "{text}"
    );
    // A record it does not expect stops it before anything is written, the
    // copy of the database included.
    let other_labels = |labels: Value| {
        edit_records(&db, "repos", |r| {
            if r["path"] == OTHER {
                r["labels"] = labels.clone();
            }
        })
    };
    other_labels(json!("x"));
    // The exit status says what happened (scripts/serve.sh reads it): 1, the
    // database is left as it was.
    let (code, text) = admin("upgrade");
    assert!(
        code == 1 && text.contains("a repository does not decode"),
        "{text}"
    );
    let copies = std::fs::read_dir(&data)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("branvil-schema-")
        })
        .count();
    assert_eq!(copies, 0);
    let (code, text) = admin("check");
    assert!(
        code != 0 && text.contains("branvil admin upgrade"),
        "{text}"
    );
    other_labels(json!([]));
    let (code, text) = admin("upgrade");
    // scripts/serve.sh tells the owner that its log names the copy.
    let copy = format!(
        "\nthe database as it was: {}",
        data.join("branvil-schema-13-").display()
    );
    assert!(text.contains(&copy), "{text}");
    let next = format!(
        "\n`branvil admin check --data-dir {0}` has to pass before `branvil serve --data-dir {0}` starts it",
        data.display()
    );
    assert!(text.contains(&next), "{text}");
    let summary = "2 repository(ies) land nothing by themselves, 1 published pull request(s) were published by the owner, 1 pending merge(s) changed; nothing else changed";
    assert!(code == 3 && text.contains(summary), "{text}");
    // Every changed record is what version 13 wrote with this version's
    // fields, and only those records and the version changed.
    let upgraded = |old: Vec<Value>, f: fn(&mut Value)| -> Vec<Value> {
        old.into_iter()
            .map(|mut r| {
                f(&mut r);
                r
            })
            .collect()
    };
    assert_eq!(
        records(&db, "repos"),
        upgraded(old_repos, |r| r["auto_land"] = json!(false))
    );
    assert_eq!(
        records(&db, "pulls"),
        upgraded(old_pulls, |p| if p["upstream"].is_object() {
            p["upstream"]["auto"] = json!(false);
            p["upstream"]["notified_at"] = Value::Null;
        })
    );
    assert_eq!(
        records(&db, "ref_journal"),
        upgraded(old_journal, |e| {
            e["op"].as_object_mut().unwrap().remove("merged_by");
        })
    );
    let (mut after, mut expected) = (tables(&db), before);
    for t in ["repos", "pulls", "ref_journal", "meta"] {
        after.remove(t);
        expected.remove(t);
    }
    assert_eq!(after, expected);
    // Until a check passes, upgrade answers 3 with nothing left to upgrade,
    // and serve refuses the database.
    let unchecked = "has not passed `branvil admin check` since";
    let (code, text) = admin("upgrade");
    assert!(code == 3 && text.contains(unchecked), "{text}");
    let mut serve = std::process::Command::new(BIN)
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(&data)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let refused = loop {
        if let Some(status) = serve.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            serve.kill().unwrap();
            panic!("serve did not refuse an unchecked database");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut serve.stderr.take().unwrap(), &mut err).unwrap();
    assert!(!refused.success() && err.contains(unchecked), "{err}");
    // A check that fails leaves the mark.
    {
        let database = redb::Database::open(&db).unwrap();
        let txn = database.begin_write().unwrap();
        {
            let def: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("ix_cards");
            let mut cards = txn.open_table(def).unwrap();
            let key = redb::ReadableTable::iter(&cards)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .0
                .value()
                .to_vec();
            cards.remove(key.as_slice()).unwrap();
        }
        txn.commit().unwrap();
    }
    let (code, text) = admin("check");
    assert!(code == 1 && text.contains("index problem"), "{text}");
    let (code, text) = admin("upgrade");
    assert!(code == 3 && text.contains(unchecked), "{text}");
    let (code, text) = admin("reindex");
    assert!(code == 0, "{text}");
    // A passing check clears it.
    let (code, text) = admin("check");
    assert!(code == 0 && text.contains("indexes: ok"), "{text}");
    // 0: the database already has this version, and nothing changed.
    let upgraded = tables(&db);
    let (code, text) = admin("upgrade");
    assert!(
        code == 0 && text.contains("already has schema version"),
        "{text}"
    );
    assert_eq!(tables(&db), upgraded);
    w.restart(&[]);
    // The interrupted merge completed at start; every repository lands
    // nothing by itself, and the published pull request was the owner's.
    let s = &w.server;
    assert_eq!(s.ok(&interrupted, &w.owner).await["merged"], true);
    let all = s.ok("/repos", &w.owner).await;
    let all = all["items"].as_array().unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|r| r["auto_land"] == false), "{all:?}");
    let x = pull(&w, published).await["upstream"].clone();
    assert_eq!(
        (&x["auto"], &x["notified_at"]),
        (&json!(false), &Value::Null)
    );
}
