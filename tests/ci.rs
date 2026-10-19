//! CI against a real service and real Git. The test acts as the runner over
//! the runner API (a scripted runner: it decides each attempt's conclusion),
//! so the service's queue, leases, supersession, checks, and merge gate are
//! exercised exactly as a real runner would see them.

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};

const IMAGE: &str = "docker.io/library/alpine:3.22";
/// The configuration digest the scripted runner reports for the image.
const DIGEST: &str = "sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8";

fn pipeline(jobs: &[&str]) -> String {
    let mut s = String::new();
    for j in jobs {
        s.push_str(&format!(
            "[[jobs]]\nname = \"{j}\"\nimage = \"{IMAGE}\"\ncommands = [\"echo {j}\"]\nartifacts = [{{name = \"report\", paths = [\"out/\"]}}]\n\n"
        ));
    }
    s
}

/// A scripted runner talking to the runner API.
struct Runner {
    url: String,
    token: String,
    http: reqwest::Client,
}

impl Runner {
    async fn new(w: &World, name: &str) -> Runner {
        Runner {
            url: w.server.url.clone(),
            token: w.server.member(&w.owner, name, "runner").await,
            http: reqwest::Client::new(),
        }
    }

    fn at(&self, path: &str) -> String {
        format!("{}/api/v1/runner{path}", self.url)
    }

    async fn claim(&self) -> Option<Value> {
        let resp = self
            .http
            .post(self.at("/claim"))
            .bearer_auth(&self.token)
            .json(&json!({"executor": "test", "executor_state": "ready"}))
            .send()
            .await
            .unwrap();
        match resp.status() {
            StatusCode::NO_CONTENT => None,
            StatusCode::OK => Some(resp.json().await.unwrap()),
            other => panic!("claim: {other} {}", resp.text().await.unwrap()),
        }
    }

    async fn post(&self, c: &Value, what: &str, body: Value) -> (StatusCode, Value) {
        let resp = self
            .http
            .post(self.at(&format!(
                "/attempts/{}/{what}",
                c["attempt_id"].as_str().unwrap()
            )))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let st = resp.status();
        (st, resp.json().await.unwrap_or(Value::Null))
    }

    async fn heartbeat(&self, c: &Value) -> (StatusCode, Value) {
        self.post(c, "heartbeat", json!({"lease": c["lease"]}))
            .await
    }

    async fn finish(&self, c: &Value, conclusion: &str) -> (StatusCode, Value) {
        self.post(
            c,
            "finish",
            json!({"lease": c["lease"], "conclusion": conclusion, "details": format!("scripted {conclusion}"), "image_digest": DIGEST}),
        )
        .await
    }

    async fn raw(
        &self,
        method: reqwest::Method,
        c: &Value,
        path: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Vec<u8>) {
        let resp = self
            .http
            .request(
                method,
                self.at(&format!(
                    "/attempts/{}/{path}",
                    c["attempt_id"].as_str().unwrap()
                )),
            )
            .bearer_auth(&self.token)
            .header("x-branvil-lease", c["lease"].as_str().unwrap())
            .body(body)
            .send()
            .await
            .unwrap();
        (resp.status(), resp.bytes().await.unwrap().to_vec())
    }

    /// Claims every queued attempt and finishes it with `conclusion`.
    async fn drain(&self, conclusion: &str) -> usize {
        let mut n = 0;
        while let Some(c) = self.claim().await {
            assert_eq!(self.finish(&c, conclusion).await.0, 200);
            n += 1;
        }
        n
    }
}

/// Brings the work clone's main to the service's main.
fn sync_main(w: &World) {
    git_as_ok(&w.work, "alice", &w.owner, &["fetch", "-q", "origin"]);
    git_ok(&w.work, &["checkout", "-q", "-B", "main", "origin/main"]);
}

/// Lands files on the protected main through a reviewed, merged pull
/// request (running CI with `runner` when there is any).
async fn land(w: &World, branch: &str, files: &[(&str, &str)], runner: Option<&Runner>) -> String {
    sync_main(w);
    for (path, content) in files {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(w.work.join(parent)).unwrap();
        }
        std::fs::write(w.work.join(path), content).unwrap();
        git_ok(&w.work, &["add", path]);
    }
    let (last, content) = files.last().unwrap();
    let head = w.push_change(branch, last, content);
    let p = w.open_pull(branch, json!([])).await;
    let n = p["number"].as_u64().unwrap();
    if let Some(r) = runner {
        r.drain("success").await;
    }
    let base = format!("{}/pulls/{n}", w.repo_path());
    let (st, v) = w
        .server
        .post(
            &format!("{base}/merge"),
            &w.owner,
            json!({"expected_head": head, "expected_base": p["base_sha"]}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    sync_main(w);
    head
}

async fn set_required(w: &World, checks: &[&str]) {
    let repo = w.server.ok(&w.repo_path(), &w.owner).await;
    let (st, v) = w
        .server
        .patch(
            &w.repo_path(),
            &w.owner,
            json!({"revision": repo["revision"], "required_checks": checks}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
}

async fn checks(w: &World, n: u64) -> Value {
    w.server
        .ok(&format!("{}/pulls/{n}/checks", w.repo_path()), &w.aiko)
        .await
}

fn check<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no check {name} in {v}"))
}

/// Polls `f` until it returns true (the service sweeps every 5 s).
async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..60 {
        if f().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Sets `[ci]` values in the service's configuration (restart to apply).
fn set_ci_config(w: &World, values: &[(&str, i64)]) {
    let path = w.server.data.join("branvil.toml");
    let mut doc: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    let ci = doc
        .entry("ci")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .unwrap();
    for (k, v) in values {
        ci.insert(k.to_string(), toml::Value::Integer(*v));
    }
    std::fs::write(path, toml::to_string(&doc).unwrap()).unwrap();
}

#[tokio::test]
async fn a_head_runs_its_own_pipeline_and_reports_logs_and_artifacts() {
    let w = World::new().await;
    let runner = Runner::new(&w, "runner1").await;
    land(
        &w,
        "setup",
        &[(".branvil/ci.toml", &pipeline(&["test"]))],
        None,
    )
    .await;
    // The run of main's new tip (the merge) is queued; take it out of the way.
    assert_eq!(runner.drain("success").await, 1);
    set_required(&w, &["test"]).await;

    // The head changes its pipeline; its own pipeline runs.
    sync_main(&w);
    let changed = pipeline(&["test"]).replace("echo test", "echo changed");
    std::fs::write(w.work.join(".branvil/ci.toml"), changed).unwrap();
    git_ok(&w.work, &["add", ".branvil/ci.toml"]);
    let head = w.push_change("feature", "f.txt", "one\n");
    let p = w.open_pull("feature", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    let v = checks(&w, n).await;
    assert_eq!(v["checks_expected"], true);
    assert_eq!(v["checks"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(check(&v, "test")["status"], "queued");
    assert!(check(&v, "test")["attempt_id"].is_string());

    let c = runner.claim().await.unwrap();
    assert_eq!(c["sha"], head.as_str());
    assert_eq!(c["job"]["name"], "test");
    assert_eq!(c["job"]["commands"], json!(["echo changed"]));
    assert_eq!(c["job"]["image"], IMAGE);
    // The runner's limits apply: the pipeline gives no time limit.
    assert!(c["job"]["timeout_secs"].is_null(), "{c}");
    assert!(c["job"].get("cpus").is_none(), "{c}");
    assert_eq!(c["repo"], REPO);
    assert_eq!(c["network"], false);
    assert_eq!(c["cache_scope"], "branch:feature");
    assert_eq!(check(&checks(&w, n).await, "test")["status"], "in_progress");

    // The source is the head's tree, as a tar made by the service.
    let (st, tar) = runner.raw(reqwest::Method::GET, &c, "source", vec![]).await;
    assert_eq!(st, 200);
    let tar_path = w.dir.path().join("src.tar");
    std::fs::write(&tar_path, &tar).unwrap();
    let listing = std::process::Command::new("tar")
        .arg("-tf")
        .arg(&tar_path)
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(listing.lines().any(|l| l == "f.txt"), "{listing}");
    assert!(listing.lines().any(|l| l == "README.md"), "{listing}");

    let (st, beat) = runner.heartbeat(&c).await;
    assert_eq!(st, 200, "{beat}");
    assert_eq!(beat["cancel"], false);
    let (st, _) = runner
        .raw(
            reqwest::Method::POST,
            &c,
            "log",
            b"line one\nline two\n".to_vec(),
        )
        .await;
    assert_eq!(st, 200);
    // Artifacts: zip only, declared names only, no path tricks.
    let zip = b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0".to_vec();
    let (st, _) = runner
        .raw(reqwest::Method::PUT, &c, "artifacts/report", zip.clone())
        .await;
    assert_eq!(st, 200);
    let (st, _) = runner
        .raw(reqwest::Method::PUT, &c, "artifacts/other", zip.clone())
        .await;
    assert_eq!(st, 400, "undeclared artifact");
    let (st, _) = runner
        .raw(
            reqwest::Method::PUT,
            &c,
            "artifacts/report",
            b"not a zip".to_vec(),
        )
        .await;
    assert_eq!(st, 400);
    let (st, _) = runner
        .raw(
            reqwest::Method::PUT,
            &c,
            "artifacts/..%2F..%2Fx",
            zip.clone(),
        )
        .await;
    assert!(st == 400 || st == 404, "{st}");
    // Another runner's or a made-up lease changes nothing.
    let mut forged = c.clone();
    forged["lease"] = json!("bvl_forged");
    assert_eq!(runner.finish(&forged, "success").await.0, 409);
    assert_eq!(
        runner
            .raw(reqwest::Method::POST, &forged, "log", b"x".to_vec())
            .await
            .0,
        409
    );

    let (st, fin) = runner
        .post(
            &c,
            "finish",
            json!({"lease": c["lease"], "conclusion": "success", "image_digest": "alpine:3.22"}),
        )
        .await;
    assert_eq!(st, 400, "not a digest: {fin}");
    let (st, fin) = runner.finish(&c, "success").await;
    assert_eq!(st, 200, "{fin}");
    assert_eq!(fin["image_digest"], DIGEST);
    let v = checks(&w, n).await;
    let t = check(&v, "test");
    assert_eq!(
        (t["status"].as_str(), t["conclusion"].as_str()),
        (Some("completed"), Some("success"))
    );
    assert_eq!(t["artifact_names"], json!(["report"]));
    let id = t["attempt_id"].as_str().unwrap();
    assert_eq!(
        t["details_url"].as_str().unwrap(),
        format!("{}/{REPO}/-/jobs/{id}", w.server.url)
    );
    let job = w
        .server
        .ok(&format!("{}/jobs/{id}", w.repo_path()), &w.ken)
        .await;
    assert_eq!(job["job"], "test");
    assert_eq!(job["attempt"], 1);
    assert_eq!(job["sha"], head.as_str());
    assert_eq!(job["image_digest"], DIGEST);
    let log = w
        .server
        .ok(
            &format!("{}/jobs/{id}/log?tail_bytes=9", w.repo_path()),
            &w.ken,
        )
        .await;
    assert_eq!(log["log"], "line two\n");
    assert_eq!(log["log_bytes"], 18);
    assert_eq!(log["truncated"], true);
    let resp = w
        .server
        .http
        .get(format!(
            "{}/api/v1{}/commits/{head}/artifacts/report",
            w.server.url,
            w.repo_path()
        ))
        .bearer_auth(&w.ken)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "application/zip");
    assert_eq!(resp.bytes().await.unwrap().to_vec(), zip);
    // A runner sees no job outside the runner API; another commit has no artifact.
    assert_eq!(
        w.server
            .get(&format!("{}/jobs/{id}", w.repo_path()), &runner.token)
            .await
            .0,
        403
    );
    let base_sha = p["base_sha"].as_str().unwrap();
    assert_eq!(
        w.server
            .get(
                &format!("{}/commits/{base_sha}/artifacts/nothing", w.repo_path()),
                &w.ken
            )
            .await
            .0,
        404
    );

    // The runner API is for runner members only.
    let resp = w
        .server
        .http
        .post(format!("{}/api/v1/runner/claim", w.server.url))
        .bearer_auth(&w.aiko)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);

    // With the check green, the merge goes through.
    let base = format!("{}/pulls/{n}", w.repo_path());
    let (st, v) = w
        .server
        .post(
            &format!("{base}/merge"),
            &w.owner,
            json!({"expected_head": head, "expected_base": p["base_sha"]}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    // The fast-forwarded main keeps the head's run: nothing runs again.
    assert!(runner.claim().await.is_none());
    let diag = w.server.ok("/admin/diagnostics", &w.owner).await;
    assert_eq!(diag["runner"]["state"], "ready", "{diag}");
    assert_eq!(diag["runner"]["runners"][0]["name"], "runner1");
}

#[tokio::test]
async fn a_version_8_run_takes_its_commits_pipeline_as_now_read_after_the_upgrade() {
    let mut w = World::new().await;
    let runner = Runner::new(&w, "runner1").await;
    // The default branch's pipeline gives its time limit: it reads the same.
    let valid = pipeline(&["test"]).replace("commands", "timeout_secs = 600\ncommands");
    land(&w, "setup", &[(".branvil/ci.toml", &valid)], None).await;
    assert_eq!(runner.drain("success").await, 1);
    set_required(&w, &["test"]).await;
    // Heads of open pull requests, each with a pipeline of its own.
    let open_head = |branch: &'static str, ci: String| {
        let w = &w;
        async move {
            sync_main(w);
            std::fs::write(w.work.join(".branvil/ci.toml"), ci).unwrap();
            git_ok(&w.work, &["add", ".branvil/ci.toml"]);
            let head = w.push_change(branch, "f.txt", &format!("{branch}\n"));
            let p = w.open_pull(branch, json!([])).await;
            (head, p)
        }
    };
    // A head whose job sets its CPUs, as version 8 accepted.
    let (head, p) = open_head("feature", valid.replace("commands", "cpus = 2\ncommands")).await;
    let n = p["number"].as_u64().unwrap();
    let base = p["base_sha"].as_str().unwrap().to_string();
    // A head whose job pins its image by digest, as version 8 accepted.
    let pinned_image = format!("docker.io/library/alpine@{DIGEST}");
    let (pinned, _) = open_head("pinned", valid.replace(IMAGE, &pinned_image)).await;
    // A head whose job gives no time limit, its attempt still queued:
    // version 8 recorded the 600 s default.
    let (untimed, _) = open_head("untimed", pipeline(&["test"])).await;

    // What version 8 recorded: jobs with CPUs and memory, attempts without
    // the image they ran on, and the head's job as its file defined it.
    w.server.stop();
    let db = w.server.data.join("branvil.redb");
    as_schema_8(&db);
    let base_run = records(&db, "ci_runs")
        .into_iter()
        .find(|r| r["sha"] == base.as_str())
        .unwrap();
    let mut head_pipeline = base_run["pipeline"].clone();
    head_pipeline["jobs"][0]["cpus"] = json!(2);
    let mut pinned_pipeline = base_run["pipeline"].clone();
    pinned_pipeline["jobs"][0]["image"] = json!(pinned_image);
    edit_records(&db, "ci_runs", |r| {
        if r["sha"] == head.as_str() {
            r["pipeline"] = head_pipeline.clone();
        }
        if r["sha"] == pinned.as_str() {
            r["pipeline"] = pinned_pipeline.clone();
        }
    });
    // A run of a commit that is no longer in the repository.
    let gone = "1".repeat(40);
    let mut lost = base_run.clone();
    lost["sha"] = json!(gone);
    lost["branches"] = json!([]);
    lost["superseded_by"] = json!(base);
    let repo_id = base_run["repo"].as_str().unwrap();
    insert_record(&db, "ci_runs", &key_of(&[repo_id, &gone]), &lost);
    let before = tables(&db);

    // The service does not open it; `admin upgrade` does, keeping a copy.
    let data = w.server.data.clone();
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
        (out.status.success(), text)
    };
    let copies = || {
        std::fs::read_dir(&data)
            .unwrap()
            .filter(|e| {
                let name = e.as_ref().unwrap().file_name();
                name.to_string_lossy().starts_with("branvil-schema-")
            })
            .count()
    };
    let (ok, text) = admin("check");
    assert!(!ok && text.contains("branvil admin upgrade"), "{text}");
    // A Git failure fails the upgrade before anything is written: a pipeline
    // the commit has that Git cannot read, a repository Git cannot open.
    let repo_dir = data.join("repos/acme/demo.git");
    let blob = git_ok(
        &repo_dir,
        &["rev-parse", &format!("{head}:.branvil/ci.toml")],
    );
    let blob = blob.trim();
    let loose = repo_dir.join("objects").join(&blob[..2]).join(&blob[2..]);
    let aside = data.join("blob-aside");
    std::fs::rename(&loose, &aside).unwrap();
    let (ok, text) = admin("upgrade");
    assert!(!ok && text.contains(REPO), "{text}");
    assert_eq!(tables(&db), before);
    assert_eq!(copies(), 0);
    std::fs::rename(&aside, &loose).unwrap();
    let (repos, away) = (data.join("repos"), data.join("repos-away"));
    std::fs::rename(&repos, &away).unwrap();
    let (ok, text) = admin("upgrade");
    assert!(!ok && text.contains(REPO), "{text}");
    assert_eq!(tables(&db), before);
    assert_eq!(copies(), 0);
    std::fs::rename(&away, &repos).unwrap();
    let (ok, text) = admin("upgrade");
    assert!(
        ok && text.contains("the jobs of 2 CI attempt(s) lost their cpus and memory_mib")
            && text.contains("against its commit: 4 took a new one (1 could not be verified")
            && text.contains("1 unfinished attempt(s) of another definition were stopped"),
        "{text}"
    );
    assert_eq!(copies(), 1);
    let copy = text
        .lines()
        .find_map(|l| l.strip_prefix("the database as it was: "))
        .unwrap();
    let copy = std::path::Path::new(copy);
    assert_eq!(tables(copy), before);
    // Only the CI records (and the stopped attempt's queue entry) and the
    // version changed.
    let (mut after, mut expected) = (tables(&db), before.clone());
    for t in ["ci_runs", "ci_attempts", "ix_ci_pending", "meta"] {
        after.remove(t);
        expected.remove(t);
    }
    assert_eq!(after, expected);
    let without_resources = |job: &mut Value| {
        let job = job.as_object_mut().unwrap();
        assert!(job.remove("cpus").is_some() && job.remove("memory_mib").is_some());
    };
    assert!(records(&db, "ix_ci_pending").is_empty());
    // Every attempt's job lost its CPUs and memory and kept its time limit;
    // no attempt names the image it ran on. The untimed head's queued
    // attempt is of another definition now (its pipeline gives no time
    // limit) and was stopped.
    let attempts = records(&db, "ci_attempts");
    assert_eq!(attempts.len(), 2);
    for (mut old, new) in records(copy, "ci_attempts").into_iter().zip(&attempts) {
        without_resources(&mut old["job"]);
        old["image_digest"] = Value::Null;
        if old["sha"] == untimed.as_str() {
            assert_eq!(old["status"], "queued");
            assert_eq!(
                (&new["status"], &new["conclusion"]),
                (&json!("completed"), &json!("cancelled"))
            );
            assert_eq!(
                new["details"],
                "Stopped by the upgrade to schema version 9: the commit's pipeline does not define this job so."
            );
            for k in ["status", "conclusion", "details", "finished_at"] {
                old[k] = new[k].clone();
            }
        }
        assert_eq!(&old, new);
        assert_eq!(new["job"]["timeout_secs"], 600);
    }
    // Each run took its commit's pipeline as read now: the base's reads the
    // same, the head's and the pinned one's are invalid with the reason, the
    // untimed one's has no time limit, the gone commit's cannot be verified.
    let upgraded = records(&db, "ci_runs");
    let original = records(copy, "ci_runs");
    assert_eq!(upgraded.len(), original.len());
    for (mut old, new) in original.into_iter().zip(upgraded) {
        let message = new["pipeline"]["message"].as_str().unwrap_or("");
        if old["sha"] == head.as_str() {
            assert_eq!(new["pipeline"]["state"], "invalid", "{new}");
            assert!(
                message.contains("cpus and memory_mib are not pipeline settings"),
                "{new}"
            );
            old["pipeline"] = new["pipeline"].clone();
        } else if old["sha"] == pinned.as_str() {
            assert_eq!(new["pipeline"]["state"], "invalid", "{new}");
            assert!(message.contains("without a digest"), "{new}");
            old["pipeline"] = new["pipeline"].clone();
        } else if old["sha"] == untimed.as_str() {
            assert_eq!(old["pipeline"]["jobs"][0]["timeout_secs"], 600);
            assert!(
                new["pipeline"]["jobs"][0]["timeout_secs"].is_null(),
                "{new}"
            );
            old["pipeline"] = new["pipeline"].clone();
        } else if old["sha"] == gone.as_str() {
            assert_eq!(new["pipeline"]["state"], "invalid", "{new}");
            assert!(message.contains("could not verify"), "{new}");
            old["pipeline"] = new["pipeline"].clone();
        } else {
            let jobs = old
                .pointer_mut("/pipeline/jobs")
                .and_then(Value::as_array_mut);
            jobs.into_iter().flatten().for_each(without_resources);
        }
        assert_eq!(old, new);
    }
    let (ok, text) = admin("upgrade");
    assert!(
        ok && text.contains("already has schema version 9"),
        "{text}"
    );
    let (ok, text) = admin("check");
    assert!(ok && text.contains("indexes: ok"), "{text}");

    // Started again, the head's pipeline is a failing check with the reason,
    // and the merge waits.
    w.restart(&[]);
    let runner = Runner {
        url: w.server.url.clone(),
        token: runner.token.clone(),
        http: reqwest::Client::new(),
    };
    let v = checks(&w, n).await;
    let invalid = check(&v, "pipeline");
    assert_eq!(invalid["conclusion"], "failure", "{v}");
    assert!(
        invalid["details"]
            .as_str()
            .unwrap()
            .contains("cpus and memory_mib are not pipeline settings"),
        "{v}"
    );
    let (st, v) = w
        .server
        .post(
            &format!("{}/pulls/{n}/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head": head, "expected_base": base}),
        )
        .await;
    assert_eq!(
        (st.as_u16(), v["error"]["code"].as_str()),
        (409, Some("not_mergeable")),
        "{v}"
    );
    // The base's attempt is still its job's current one: it can be retried,
    // with its time limit.
    let id = attempts[0]["id"].as_str().unwrap();
    let old = w
        .server
        .ok(&format!("{}/jobs/{id}", w.repo_path()), &w.ken)
        .await;
    assert_eq!(old["conclusion"], "success");
    assert!(old["image_digest"].is_null(), "{old}");
    let (st, v) = w
        .server
        .post(
            &format!("{}/jobs/{id}/retry", w.repo_path()),
            &w.owner,
            json!({}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    // (The reconciler may have queued the untimed head's job again first.)
    let c = loop {
        let c = runner.claim().await.unwrap();
        if c["sha"] == base.as_str() {
            break c;
        }
        assert_eq!(c["sha"], untimed.as_str(), "{c}");
    };
    assert_eq!(c["job"]["timeout_secs"], 600);
    assert!(c["job"].get("cpus").is_none(), "{c}");
}

#[tokio::test]
async fn rapid_pushes_supersede_older_commits_and_their_late_results_never_count() {
    let w = World::new().await;
    let runner = Runner::new(&w, "runner1").await;
    land(
        &w,
        "setup",
        &[(".branvil/ci.toml", &pipeline(&["build", "test"]))],
        None,
    )
    .await;
    runner.drain("success").await;
    set_required(&w, &["build", "test"]).await;
    sync_main(&w);
    let s1 = w.push_change("feature", "f.txt", "1\n");
    let p = w.open_pull("feature", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    // build of s1 starts; test of s1 is still queued.
    let running = runner.claim().await.unwrap();
    assert_eq!(running["sha"], s1.as_str());
    let s2 = w.push_change("feature", "f.txt", "2\n");
    let s3 = w.push_change("feature", "f.txt", "3\n");
    // The running attempt of s1 is asked to stop and its result is not used.
    let (st, beat) = runner.heartbeat(&running).await;
    assert_eq!(st, 200);
    assert_eq!(beat["cancel"], true);
    let (st, fin) = runner.finish(&running, "success").await;
    assert_eq!(st, 200);
    assert_eq!(fin["conclusion"], "cancelled");
    // s2 never got to run: its attempts were replaced before they started.
    let mut claimed = Vec::new();
    while let Some(c) = runner.claim().await {
        claimed.push(c["sha"].as_str().unwrap().to_string());
        assert_eq!(runner.finish(&c, "success").await.0, 200);
    }
    assert_eq!(
        claimed,
        vec![s3.clone(), s3.clone()],
        "only the newest commit runs"
    );
    assert!(!claimed.contains(&s2), "s2 was replaced before it started");
    let v = checks(&w, n).await;
    assert_eq!(v["head_sha"], s3.as_str());
    assert_eq!(check(&v, "build")["conclusion"], "success");
    assert_eq!(check(&v, "test")["conclusion"], "success");
    // No check of s1 carries over.
    let base = format!("{}/pulls/{n}", w.repo_path());
    let (st, v) = w
        .server
        .post(
            &format!("{base}/merge"),
            &w.owner,
            json!({"expected_head": s1, "expected_base": p["base_sha"]}),
        )
        .await;
    assert_eq!(
        (st.as_u16(), v["error"]["code"].as_str()),
        (409, Some("head_moved"))
    );
}

#[tokio::test]
async fn required_ci_is_never_shown_as_success_or_no_checks() {
    let w = World::new().await;
    let runner = Runner::new(&w, "runner1").await;
    set_required(&w, &["test"]).await;
    // No pipeline on the base: the required check cannot run, and says so.
    let head = w.push_change("feature", "f.txt", "1\n");
    let p = w.open_pull("feature", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    let v = checks(&w, n).await;
    assert_eq!(v["checks_expected"], true);
    let t = check(&v, "test");
    assert_eq!(
        (t["status"].as_str(), t["conclusion"].as_str()),
        (Some("completed"), Some("failure"))
    );
    assert!(
        t["details"].as_str().unwrap().contains(".branvil/ci.toml"),
        "{t}"
    );
    let base = format!("{}/pulls/{n}", w.repo_path());
    let merge = |head: String, base_sha: Value| {
        let (s, url, owner) = (&w.server, format!("{base}/merge"), w.owner.clone());
        async move {
            s.post(
                &url,
                &owner,
                json!({"expected_head": head, "expected_base": base_sha}),
            )
            .await
        }
    };
    let (st, v) = merge(head.clone(), p["base_sha"].clone()).await;
    assert_eq!(
        (st.as_u16(), v["error"]["code"].as_str()),
        (409, Some("not_mergeable")),
        "{v}"
    );

    // The pipeline lands (the setup PR is merged while no check is required).
    set_required(&w, &[]).await;
    w.server
        .post(&format!("{base}/close"), &w.aiko, json!({}))
        .await;
    land(
        &w,
        "setup",
        &[(".branvil/ci.toml", &pipeline(&["test"]))],
        Some(&runner),
    )
    .await;
    runner.drain("success").await;
    set_required(&w, &["test"]).await;

    // A pending, failed, or cancelled required check is never success.
    sync_main(&w);
    let head = w.push_change("feature2", "g.txt", "1\n");
    let p2 = w.open_pull("feature2", json!([])).await;
    let n2 = p2["number"].as_u64().unwrap();
    let base2 = format!("{}/pulls/{n2}", w.repo_path());
    let merge2 = |c: Value| {
        let (s, url, owner, h) = (
            &w.server,
            format!("{base2}/merge"),
            w.owner.clone(),
            head.clone(),
        );
        async move {
            let (st, v) = s
                .post(
                    &url,
                    &owner,
                    json!({"expected_head": h, "expected_base": c}),
                )
                .await;
            (st.as_u16(), v)
        }
    };
    // pending (queued, then in progress)
    assert_eq!(check(&checks(&w, n2).await, "test")["status"], "queued");
    assert_eq!(merge2(p2["base_sha"].clone()).await.0, 409);
    let c = runner.claim().await.unwrap();
    assert_eq!(merge2(p2["base_sha"].clone()).await.0, 409);
    // failure
    runner.finish(&c, "failure").await;
    let t = check(&checks(&w, n2).await, "test").clone();
    assert_eq!(t["conclusion"], "failure");
    assert_eq!(merge2(p2["base_sha"].clone()).await.0, 409);
    // retry → queued attempt 2, then cancelled
    let id = t["attempt_id"].as_str().unwrap();
    let (st, a2) = w
        .server
        .post(
            &format!("{}/jobs/{id}/retry", w.repo_path()),
            &w.aiko,
            json!({}),
        )
        .await;
    assert_eq!(st, 200, "{a2}");
    assert_eq!(a2["attempt"], 2);
    assert_eq!(a2["status"], "queued");
    let (st, _) = w
        .server
        .post(
            &format!("{}/jobs/{id}/retry", w.repo_path()),
            &w.aiko,
            json!({}),
        )
        .await;
    assert_eq!(st, 409, "only the latest completed attempt can be retried");
    let (st, v) = w
        .server
        .post(
            &format!(
                "{}/jobs/{}/cancel",
                w.repo_path(),
                a2["id"].as_str().unwrap()
            ),
            &runner.token,
            json!({}),
        )
        .await;
    assert_eq!(st, 403, "{v}");
    let (st, cancelled) = w
        .server
        .post(
            &format!(
                "{}/jobs/{}/cancel",
                w.repo_path(),
                a2["id"].as_str().unwrap()
            ),
            &w.aiko,
            json!({}),
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(cancelled["conclusion"], "cancelled");
    let t = check(&checks(&w, n2).await, "test").clone();
    assert_eq!(
        (t["attempt"].as_u64(), t["conclusion"].as_str()),
        (Some(2), Some("cancelled"))
    );
    assert_eq!(merge2(p2["base_sha"].clone()).await.0, 409);
    assert!(
        runner.claim().await.is_none(),
        "a cancelled attempt is not handed out"
    );

    // A head that breaks its own pipeline gets a failing pipeline check.
    sync_main(&w);
    std::fs::write(
        w.work.join(".branvil/ci.toml"),
        "[[jobs]]\nname = \"test\"\nprivileged = true\n",
    )
    .unwrap();
    git_ok(&w.work, &["add", ".branvil/ci.toml"]);
    w.push_change("feature3", "h.txt", "1\n");
    let p3 = w.open_pull("feature3", json!([])).await;
    let v = checks(&w, p3["number"].as_u64().unwrap()).await;
    let pl = check(&v, "pipeline");
    assert_eq!(
        (pl["status"].as_str(), pl["conclusion"].as_str()),
        (Some("completed"), Some("failure"))
    );
    assert!(
        pl["details"].as_str().unwrap().contains("privileged"),
        "{pl}"
    );
    assert_eq!(check(&v, "test")["conclusion"], "failure");
    assert!(
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["conclusion"] != "success")
    );

    // A head that drops the required check from its own pipeline runs what is
    // left, and still cannot be merged: the required check fails.
    sync_main(&w);
    std::fs::write(w.work.join(".branvil/ci.toml"), pipeline(&["lint"])).unwrap();
    git_ok(&w.work, &["add", ".branvil/ci.toml"]);
    let head4 = w.push_change("feature4", "i.txt", "1\n");
    let p4 = w.open_pull("feature4", json!([])).await;
    let n4 = p4["number"].as_u64().unwrap();
    let c = runner.claim().await.unwrap();
    assert_eq!(
        (c["sha"].as_str(), c["job"]["name"].as_str()),
        (Some(head4.as_str()), Some("lint"))
    );
    runner.finish(&c, "success").await;
    let v = checks(&w, n4).await;
    assert_eq!(check(&v, "lint")["conclusion"], "success");
    let t = check(&v, "test");
    assert_eq!(
        (t["status"].as_str(), t["conclusion"].as_str()),
        (Some("completed"), Some("failure"))
    );
    assert!(
        t["details"]
            .as_str()
            .unwrap()
            .contains("defines no job named test"),
        "{t}"
    );
    let (st, v) = w
        .server
        .post(
            &format!("{}/pulls/{n4}/merge", w.repo_path()),
            &w.owner,
            json!({"expected_head": head4, "expected_base": p4["base_sha"]}),
        )
        .await;
    assert_eq!(
        (st.as_u16(), v["error"]["code"].as_str()),
        (409, Some("not_mergeable")),
        "{v}"
    );
}

#[tokio::test]
async fn no_runner_means_startup_failure_not_waiting_forever() {
    let mut w = World::new().await;
    set_ci_config(&w, &[("runner_absent_secs", 2)]);
    w.restart(&[]);
    land(
        &w,
        "setup",
        &[(".branvil/ci.toml", &pipeline(&["test"]))],
        None,
    )
    .await;
    sync_main(&w);
    w.push_change("feature", "f.txt", "1\n");
    let p = w.open_pull("feature", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    eventually("startup_failure", || {
        let w = &w;
        async move { check(&checks(w, n).await, "test")["conclusion"] == "startup_failure" }
    })
    .await;
    let t = check(&checks(&w, n).await, "test").clone();
    assert!(
        t["details"].as_str().unwrap().contains("No CI runner"),
        "{t}"
    );
    let diag = w.server.ok("/admin/diagnostics", &w.owner).await;
    assert_eq!(diag["runner"]["state"], "no_runner", "{diag}");
}

#[tokio::test]
async fn an_expired_lease_requeues_once_and_a_restart_handles_orphans() {
    let mut w = World::new().await;
    set_ci_config(&w, &[("lease_secs", 2), ("runner_absent_secs", 600)]);
    w.restart(&[]);
    let runner = Runner::new(&w, "runner1").await;
    land(
        &w,
        "setup",
        &[(".branvil/ci.toml", &pipeline(&["test"]))],
        None,
    )
    .await;
    runner.drain("success").await;
    sync_main(&w);
    w.push_change("feature", "f.txt", "1\n");
    let p = w.open_pull("feature", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    let first = runner.claim().await.unwrap();
    // The runner goes silent; its lease expires and the job is queued again.
    eventually("re-queue", || {
        let w = &w;
        async move { check(&checks(w, n).await, "test")["attempt"] == 2 }
    })
    .await;
    let t = check(&checks(&w, n).await, "test").clone();
    assert_eq!(t["status"], "queued");
    let old = w
        .server
        .ok(
            &format!(
                "{}/jobs/{}",
                w.repo_path(),
                first["attempt_id"].as_str().unwrap()
            ),
            &w.aiko,
        )
        .await;
    assert_eq!(old["conclusion"], "cancelled");
    assert!(
        old["details"].as_str().unwrap().contains("Interrupted"),
        "{old}"
    );
    // The silent runner comes back: its result is refused.
    assert_eq!(runner.finish(&first, "success").await.0, 409);
    assert_eq!(runner.heartbeat(&first).await.0, 409);

    // Attempt 2 is running when the service stops; its lease runs out while
    // it is down. The restarted service interrupts it for good.
    let second = runner.claim().await.unwrap();
    assert_eq!(second["attempt"], 2);
    w.server.stop();
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    w.restart(&[]);
    let runner = Runner {
        url: w.server.url.clone(),
        token: runner.token.clone(),
        http: reqwest::Client::new(),
    };
    eventually("interrupted twice", || {
        let w = &w;
        async move { check(&checks(w, n).await, "test")["conclusion"] == "cancelled" }
    })
    .await;
    let t = check(&checks(&w, n).await, "test").clone();
    assert_eq!(t["attempt"], 2);
    assert!(t["details"].as_str().unwrap().contains("twice"), "{t}");
    assert_eq!(runner.finish(&second, "success").await.0, 409);
    assert!(runner.claim().await.is_none());
    // The indexes still match the records after all of it.
    w.server.stop();
    let out = std::process::Command::new(BIN)
        .args(["admin", "check", "--data-dir"])
        .arg(&w.server.data)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[tokio::test]
async fn a_merge_needs_success_on_the_exact_head_and_refuses_a_moved_base() {
    let w = World::new().await;
    let runner = Runner::new(&w, "runner1").await;
    land(
        &w,
        "setup",
        &[(".branvil/ci.toml", &pipeline(&["test"]))],
        None,
    )
    .await;
    runner.drain("success").await;
    set_required(&w, &["test"]).await;
    sync_main(&w);
    let s1 = w.push_change("feature", "f.txt", "1\n");
    let p = w.open_pull("feature", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    runner.drain("success").await;
    let base = format!("{}/pulls/{n}", w.repo_path());
    // A new head: the old success does not carry over.
    let s2 = w.push_change("feature", "f.txt", "2\n");
    let (st, v) = w
        .server
        .post(
            &format!("{base}/merge"),
            &w.owner,
            json!({"expected_head": s2, "expected_base": p["base_sha"]}),
        )
        .await;
    assert_eq!(
        (st.as_u16(), v["error"]["code"].as_str()),
        (409, Some("not_mergeable")),
        "{v}"
    );
    assert!(v["error"]["message"].as_str().unwrap().contains("test"));
    assert_ne!(s1, s2);
    runner.drain("success").await;
    // The base moves (another pull request lands) after the caller read it.
    let old_base = p["base_sha"].clone();
    land(&w, "other", &[("o.txt", "o\n")], Some(&runner)).await;
    runner.drain("success").await;
    let (st, v) = w
        .server
        .post(
            &format!("{base}/merge"),
            &w.owner,
            json!({"expected_head": s2, "expected_base": old_base}),
        )
        .await;
    assert_eq!(
        (st.as_u16(), v["error"]["code"].as_str()),
        (409, Some("base_moved")),
        "{v}"
    );
    let pull = w.server.ok(&base, &w.aiko).await;
    assert_eq!(pull["merged"], false);
}

#[tokio::test]
async fn the_owner_alone_may_open_the_network_of_jobs() {
    let w = World::new().await;
    let repo = w.server.ok(&w.repo_path(), &w.aiko).await;
    let (st, v) = w
        .server
        .patch(
            &w.repo_path(),
            &w.aiko,
            json!({"revision": repo["revision"], "ci_network": true}),
        )
        .await;
    assert_eq!(st, 403, "{v}");
    let (st, v) = w
        .server
        .patch(
            &w.repo_path(),
            &w.owner,
            json!({"revision": repo["revision"], "ci_network": true}),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["ci_network"], true);
}

#[tokio::test]
async fn ci_records_logs_and_artifacts_survive_a_backup_and_restore() {
    let mut w = World::new().await;
    let runner = Runner::new(&w, "runner1").await;
    land(
        &w,
        "setup",
        &[(".branvil/ci.toml", &pipeline(&["test"]))],
        None,
    )
    .await;
    runner.drain("success").await;
    sync_main(&w);
    let head = w.push_change("feature", "f.txt", "1\n");
    let p = w.open_pull("feature", json!([])).await;
    let n = p["number"].as_u64().unwrap();
    let c = runner.claim().await.unwrap();
    runner
        .raw(
            reqwest::Method::POST,
            &c,
            "log",
            b"kept in the backup\n".to_vec(),
        )
        .await;
    let mut zip = b"PK\x05\x06".to_vec();
    zip.extend_from_slice(&[0; 18]);
    let (st, _) = runner
        .raw(reqwest::Method::PUT, &c, "artifacts/report", zip.clone())
        .await;
    assert_eq!(st, 200);
    assert_eq!(runner.finish(&c, "success").await.0, 200);
    let before = checks(&w, n).await;

    let backup = w.dir.path().join("backup");
    let (st, v) = w
        .server
        .post("/admin/backup", &w.owner, json!({"to": backup}))
        .await;
    assert_eq!(st, 200, "{v}");
    let target = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(BIN)
        .args(["restore", "--from"])
        .arg(&backup)
        .arg("--data-dir")
        .arg(target.path().join("data"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listen = w.server.url.trim_start_matches("http://").to_string();
    w.server.stop();
    w.server = Server::start_on(target.path(), &listen, &[]);
    let after = checks(&w, n).await;
    assert_eq!(after["checks"], before["checks"]);
    let id = check(&after, "test")["attempt_id"]
        .as_str()
        .unwrap()
        .to_string();
    let log = w
        .server
        .ok(&format!("{}/jobs/{id}/log", w.repo_path()), &w.aiko)
        .await;
    assert_eq!(log["log"], "kept in the backup\n");
    let resp = w
        .server
        .http
        .get(format!(
            "{}/api/v1{}/commits/{head}/artifacts/report",
            w.server.url,
            w.repo_path()
        ))
        .bearer_auth(&w.aiko)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.bytes().await.unwrap().to_vec(), zip);
}
