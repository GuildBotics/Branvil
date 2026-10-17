//! A stand-in for GitHub: the REST endpoints Branvil's `github` provider uses
//! (with `ETag` / `304` like GitHub), GitHub Apps (a JWT verified with the
//! app's public key buys an installation token that expires), and bare
//! repositories on disk as the Git side (`<root>/<owner>/<name>.git`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
use aws_lc_rs::rsa::KeySize;
use aws_lc_rs::signature::{KeyPair, RSA_PKCS1_2048_8192_SHA256, RsaKeyPair, UnparsedPublicKey};
use axum::Router;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use super::git_ok;

#[derive(Clone, Debug)]
pub struct FakePull {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub open: bool,
    pub merge_sha: Option<String>,
    /// The login that created it.
    pub author: String,
}

/// A GitHub App registered with the stand-in.
pub struct FakeApp {
    pub slug: String,
    pub installation: String,
    /// PKCS#1 `RSAPublicKey`.
    pub public_key: Vec<u8>,
}

/// An app's key pair: the PEM Branvil is given and the public half.
pub struct AppKey {
    /// PKCS#1 (`RSA PRIVATE KEY`), as GitHub hands it out.
    pub pem: String,
    pub public_key: Vec<u8>,
}

impl AppKey {
    pub fn generate() -> AppKey {
        let pair = RsaKeyPair::generate(KeySize::Rsa2048).unwrap();
        let pkcs8: Pkcs8V1Der = pair.as_der().unwrap();
        let body = STANDARD.encode(pkcs1(pkcs8.as_ref()));
        let lines: Vec<&str> = body
            .as_bytes()
            .chunks(64)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect();
        AppKey {
            pem: format!(
                "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----\n",
                lines.join("\n")
            ),
            public_key: pair.public_key().as_ref().to_vec(),
        }
    }
}

/// The `RSAPrivateKey` inside a PKCS#8 `PrivateKeyInfo`
/// (`SEQUENCE { version, algorithm, OCTET STRING key }`).
fn pkcs1(pkcs8: &[u8]) -> Vec<u8> {
    fn tlv(d: &[u8]) -> (u8, &[u8], &[u8]) {
        let (len, head) = if d[1] < 0x80 {
            (d[1] as usize, 2)
        } else {
            let n = (d[1] & 0x7f) as usize;
            let len = d[2..2 + n].iter().fold(0, |a, b| a << 8 | *b as usize);
            (len, 2 + n)
        };
        (d[0], &d[head..head + len], &d[head + len..])
    }
    let (_, info, _) = tlv(pkcs8);
    let (_, _, rest) = tlv(info);
    let (_, _, rest) = tlv(rest);
    let (tag, key, _) = tlv(rest);
    assert_eq!(tag, 0x04, "an OCTET STRING");
    key.to_vec()
}

#[derive(Default)]
pub struct State_ {
    /// token → login (installation tokens included while they are valid)
    pub logins: HashMap<String, String>,
    /// app id → app
    pub apps: HashMap<String, FakeApp>,
    /// Seconds an installation token is valid (GitHub: an hour).
    pub token_lifetime: i64,
    /// The installation tokens issued that have not expired.
    pub issued: Vec<String>,
    pub pulls: Vec<FakePull>,
    /// commit → check runs (GitHub's JSON)
    pub checks: HashMap<String, Vec<Value>>,
    /// check run id → log text
    pub logs: HashMap<u64, String>,
    /// (method, path, login) of every request
    pub requests: Vec<(String, String, String)>,
    /// GET answers that were `304`.
    pub not_modified: usize,
    /// Refuse every pull request creation (`422`).
    pub refuse_create: bool,
    /// Create the next pull request but answer `502` (the answer is lost).
    pub lose_next_create: bool,
}

pub struct FakeGitHub {
    pub url: String,
    pub root: PathBuf,
    pub state: Arc<Mutex<State_>>,
}

type S = (Arc<Mutex<State_>>, PathBuf);

fn login(state: &State_, headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    state.logins.get(token).cloned()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(json!({"message": "Bad credentials"})),
    )
        .into_response()
}

/// Answers `body` with an `ETag` of its content, or `304` when the client
/// already has it.
fn conditional(state: &mut State_, headers: &HeaderMap, body: Value) -> Response {
    let etag = format!("\"{:x}\"", md5ish(&body.to_string()));
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        state.not_modified += 1;
        return StatusCode::NOT_MODIFIED.into_response();
    }
    ([("ETag", etag)], axum::Json(body)).into_response()
}

fn md5ish(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn pull_json(p: &FakePull, owner_repo: &str) -> Value {
    json!({
        "number": p.number,
        "html_url": format!("https://github.test/{owner_repo}/pull/{}", p.number),
        "state": if p.open { "open" } else { "closed" },
        "merged": p.merge_sha.is_some(),
        "merged_at": p.merge_sha.as_ref().map(|_| "2026-10-16T00:00:00Z"),
        "merge_commit_sha": p.merge_sha,
        "title": p.title,
        "body": p.body,
        "head": {"ref": p.head},
        "base": {"ref": p.base},
        "user": {"login": p.author},
    })
}

fn tip(root: &Path, repo: &str, branch: &str) -> Option<String> {
    let dir = root.join(format!("{repo}.git"));
    let out = std::process::Command::new("git")
        .args(["--git-dir"])
        .arg(&dir)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn record(st: &mut State_, method: &str, path: &str, headers: &HeaderMap) -> Option<String> {
    let who = login(st, headers);
    st.requests.push((
        method.to_string(),
        path.to_string(),
        who.clone().unwrap_or_default(),
    ));
    who
}

/// The app id of a valid app JWT (RS256, signed by that app's key, not
/// expired, at most ten minutes long), as GitHub checks it.
fn app_of(st: &State_, headers: &HeaderMap) -> Option<String> {
    let jwt = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let [head, claims, signature] = jwt.split('.').collect::<Vec<_>>()[..] else {
        return None;
    };
    let decode = |part: &str| -> Option<Value> {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).ok()?).ok()
    };
    let (h, c) = (decode(head)?, decode(claims)?);
    let id = c["iss"].as_u64()?.to_string();
    let app = st.apps.get(&id)?;
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, &app.public_key)
        .verify(
            format!("{head}.{claims}").as_bytes(),
            &URL_SAFE_NO_PAD.decode(signature).ok()?,
        )
        .ok()?;
    let now = chrono::Utc::now().timestamp();
    let (iat, exp) = (c["iat"].as_i64()?, c["exp"].as_i64()?);
    (h["alg"] == "RS256" && iat <= now && now < exp && exp - iat <= 600).then_some(id)
}

async fn app_get(State((state, _)): State<S>, headers: HeaderMap) -> Response {
    let mut st = state.lock().unwrap();
    record(&mut st, "GET", "/app", &headers);
    match app_of(&st, &headers) {
        Some(id) => {
            let slug = st.apps[&id].slug.clone();
            conditional(&mut st, &headers, json!({"id": id, "slug": slug}))
        }
        None => unauthorized(),
    }
}

async fn access_tokens(
    State((state, _)): State<S>,
    UrlPath(installation): UrlPath<String>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock().unwrap();
    let path = format!("/app/installations/{installation}/access_tokens");
    record(&mut st, "POST", &path, &headers);
    let Some(id) = app_of(&st, &headers) else {
        return unauthorized();
    };
    if st.apps[&id].installation != installation {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"message": "Not Found"})),
        )
            .into_response();
    }
    // Unique for the stand-in's life (the request count only grows).
    let token = format!("ghs_{installation}_{}", st.requests.len());
    let login = format!("{}[bot]", st.apps[&id].slug);
    let lifetime = if st.token_lifetime > 0 {
        st.token_lifetime
    } else {
        3600
    };
    let expires = chrono::Utc::now() + chrono::Duration::seconds(lifetime);
    st.logins.insert(token.clone(), login);
    st.issued.push(token.clone());
    (
        StatusCode::CREATED,
        axum::Json(json!({"token": token, "expires_at": expires.to_rfc3339()})),
    )
        .into_response()
}

async fn user(State((state, _)): State<S>, headers: HeaderMap) -> Response {
    let mut st = state.lock().unwrap();
    match record(&mut st, "GET", "/user", &headers) {
        Some(l) => conditional(&mut st, &headers, json!({"login": l})),
        None => unauthorized(),
    }
}

async fn git_ref(
    State((state, root)): State<S>,
    UrlPath((owner, name, branch)): UrlPath<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock().unwrap();
    let path = format!("/repos/{owner}/{name}/git/ref/heads/{branch}");
    if record(&mut st, "GET", &path, &headers).is_none() {
        return unauthorized();
    }
    match tip(&root, &format!("{owner}/{name}"), &branch) {
        Some(sha) => conditional(
            &mut st,
            &headers,
            json!({"ref": format!("refs/heads/{branch}"), "object": {"sha": sha}}),
        ),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"message": "Not Found"})),
        )
            .into_response(),
    }
}

async fn pulls_list(
    State((state, _)): State<S>,
    UrlPath((owner, name)): UrlPath<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock().unwrap();
    let repo = format!("{owner}/{name}");
    if record(&mut st, "GET", &format!("/repos/{repo}/pulls"), &headers).is_none() {
        return unauthorized();
    }
    let head = q
        .get("head")
        .and_then(|h| h.split_once(':'))
        .map(|(_, b)| b.to_string());
    let list: Vec<Value> = st
        .pulls
        .iter()
        .filter(|p| p.repo == repo && p.open && head.as_ref().is_none_or(|h| &p.head == h))
        .map(|p| pull_json(p, &repo))
        .collect();
    conditional(&mut st, &headers, Value::Array(list))
}

async fn pulls_create(
    State((state, root)): State<S>,
    UrlPath((owner, name)): UrlPath<(String, String)>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut st = state.lock().unwrap();
    let repo = format!("{owner}/{name}");
    let Some(who) = record(&mut st, "POST", &format!("/repos/{repo}/pulls"), &headers) else {
        return unauthorized();
    };
    let head = body["head"].as_str().unwrap_or_default().to_string();
    if st.refuse_create || tip(&root, &repo, &head).is_none() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            axum::Json(json!({"message": "Validation Failed: head"})),
        )
            .into_response();
    }
    let p = FakePull {
        repo: repo.clone(),
        number: st.pulls.iter().filter(|p| p.repo == repo).count() as u64 + 100,
        head,
        base: body["base"].as_str().unwrap_or_default().into(),
        title: body["title"].as_str().unwrap_or_default().into(),
        body: body["body"].as_str().unwrap_or_default().into(),
        open: true,
        merge_sha: None,
        author: who,
    };
    let v = pull_json(&p, &repo);
    st.pulls.push(p);
    if std::mem::take(&mut st.lose_next_create) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    (StatusCode::CREATED, axum::Json(v)).into_response()
}

async fn pull_get(
    State((state, _)): State<S>,
    UrlPath((owner, name, n)): UrlPath<(String, String, u64)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock().unwrap();
    let repo = format!("{owner}/{name}");
    if record(
        &mut st,
        "GET",
        &format!("/repos/{repo}/pulls/{n}"),
        &headers,
    )
    .is_none()
    {
        return unauthorized();
    }
    match st
        .pulls
        .iter()
        .find(|p| p.repo == repo && p.number == n)
        .cloned()
    {
        Some(p) => conditional(&mut st, &headers, pull_json(&p, &repo)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn pull_patch(
    State((state, _)): State<S>,
    UrlPath((owner, name, n)): UrlPath<(String, String, u64)>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> Response {
    let mut st = state.lock().unwrap();
    let repo = format!("{owner}/{name}");
    let Some(who) = record(
        &mut st,
        "PATCH",
        &format!("/repos/{repo}/pulls/{n}"),
        &headers,
    ) else {
        return unauthorized();
    };
    let Some(p) = st
        .pulls
        .iter_mut()
        .find(|p| p.repo == repo && p.number == n)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if p.author != who {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"message": "not the author"})),
        )
            .into_response();
    }
    if let Some(t) = body["title"].as_str() {
        p.title = t.into();
    }
    if let Some(b) = body["body"].as_str() {
        p.body = b.into();
    }
    if let Some(s) = body["state"].as_str() {
        p.open = s == "open";
    }
    axum::Json(pull_json(p, &repo)).into_response()
}

async fn check_runs(
    State((state, _)): State<S>,
    UrlPath((owner, name, sha)): UrlPath<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock().unwrap();
    let path = format!("/repos/{owner}/{name}/commits/{sha}/check-runs");
    if record(&mut st, "GET", &path, &headers).is_none() {
        return unauthorized();
    }
    let runs = st.checks.get(&sha).cloned().unwrap_or_default();
    conditional(
        &mut st,
        &headers,
        json!({"total_count": runs.len(), "check_runs": runs}),
    )
}

async fn job_log(
    State((state, _)): State<S>,
    UrlPath((owner, name, id)): UrlPath<(String, String, u64)>,
    headers: HeaderMap,
) -> Response {
    let mut st = state.lock().unwrap();
    let path = format!("/repos/{owner}/{name}/actions/jobs/{id}/logs");
    if record(&mut st, "GET", &path, &headers).is_none() {
        return unauthorized();
    }
    match st.logs.get(&id) {
        Some(log) => log.clone().into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

impl FakeGitHub {
    pub async fn start(root: &Path) -> FakeGitHub {
        let state = Arc::new(Mutex::new(State_::default()));
        let app = Router::new()
            .route("/user", get(user))
            .route("/app", get(app_get))
            .route("/app/installations/{id}/access_tokens", post(access_tokens))
            .route(
                "/repos/{owner}/{name}/git/ref/heads/{*branch}",
                get(git_ref),
            )
            .route(
                "/repos/{owner}/{name}/pulls",
                get(pulls_list).post(pulls_create),
            )
            .route(
                "/repos/{owner}/{name}/pulls/{n}",
                get(pull_get).patch(pull_patch),
            )
            .route(
                "/repos/{owner}/{name}/commits/{sha}/check-runs",
                get(check_runs),
            )
            .route("/repos/{owner}/{name}/actions/jobs/{id}/logs", get(job_log))
            .with_state((state.clone(), root.to_path_buf()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        FakeGitHub {
            url,
            root: root.to_path_buf(),
            state,
        }
    }

    pub fn account(&self, token: &str, login: &str) {
        self.state
            .lock()
            .unwrap()
            .logins
            .insert(token.into(), login.into());
    }

    /// Registers a GitHub App installed once (as `installation`), whose
    /// tokens act as `<slug>[bot]`.
    pub fn app(&self, app_id: &str, installation: &str, slug: &str, key: &AppKey) {
        self.state.lock().unwrap().apps.insert(
            app_id.into(),
            FakeApp {
                slug: slug.into(),
                installation: installation.into(),
                public_key: key.public_key.clone(),
            },
        );
    }

    /// Every installation token issued so far stops working (they expired).
    pub fn expire_installation_tokens(&self) {
        let mut st = self.state.lock().unwrap();
        for token in std::mem::take(&mut st.issued) {
            st.logins.remove(&token);
        }
    }

    /// The installation tokens issued so far that still work.
    pub fn installation_tokens(&self) -> Vec<String> {
        self.state.lock().unwrap().issued.clone()
    }

    /// Creates `<owner>/<name>` as a bare copy of `from`'s branches.
    pub fn repository(&self, repo: &str, from: &Path) -> PathBuf {
        let dir = self.dir(repo);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        git_ok(
            from,
            &[
                "clone",
                "-q",
                "--bare",
                from.to_str().unwrap(),
                dir.to_str().unwrap(),
            ],
        );
        dir
    }

    pub fn dir(&self, repo: &str) -> PathBuf {
        self.root.join(format!("{repo}.git"))
    }

    pub fn tip(&self, repo: &str, branch: &str) -> Option<String> {
        tip(&self.root, repo, branch)
    }

    /// Pull request `number` of `octo/demo`.
    pub fn pull(&self, number: u64) -> FakePull {
        self.pull_of("octo/demo", number)
    }

    pub fn pull_of(&self, repo: &str, number: u64) -> FakePull {
        self.pulls(repo)
            .into_iter()
            .find(|p| p.number == number)
            .expect("a pull request")
    }

    pub fn pulls(&self, repo: &str) -> Vec<FakePull> {
        self.state
            .lock()
            .unwrap()
            .pulls
            .iter()
            .filter(|p| p.repo == repo)
            .cloned()
            .collect()
    }

    pub fn set(&self, change: impl FnOnce(&mut State_)) {
        change(&mut self.state.lock().unwrap());
    }

    /// Merges a pull request as GitHub's "Squash and merge" does: one new
    /// commit on the base with the head's tree.
    pub fn squash_merge(&self, repo: &str, number: u64) -> String {
        let p = self.pull_of(repo, number);
        let dir = self.dir(repo);
        let d = dir.to_str().unwrap();
        let base = tip(&self.root, repo, &p.base).unwrap();
        // The tree of merging the head into the base, as GitHub computes it.
        let head_ref = format!("refs/heads/{}", p.head);
        let merged = git_ok(
            &dir,
            &[
                "--git-dir",
                d,
                "merge-tree",
                "--write-tree",
                "--allow-unrelated-histories",
                &base,
                &head_ref,
            ],
        );
        let tree = merged.lines().next().unwrap().to_string();
        let sha = git_ok(
            &dir,
            &[
                "--git-dir",
                d,
                "-c",
                "user.name=GitHub",
                "-c",
                "user.email=noreply@github.test",
                "commit-tree",
                &tree,
                "-p",
                &base,
                "-m",
                &format!("{} (#{number})", p.title),
            ],
        );
        git_ok(
            &dir,
            &[
                "--git-dir",
                d,
                "update-ref",
                &format!("refs/heads/{}", p.base),
                &sha,
                &base,
            ],
        );
        self.mark_merged(repo, number, &sha);
        sha
    }

    /// Merges a pull request as "Rebase and merge" does: the head's commits
    /// made again on the base (here the base has not moved, so each keeps its
    /// tree), the last of them being the merge commit.
    pub fn rebase_merge(&self, repo: &str, number: u64) -> String {
        let p = self.pull_of(repo, number);
        let dir = self.dir(repo);
        let d = dir.to_str().unwrap();
        let base = tip(&self.root, repo, &p.base).unwrap();
        let range = format!("{base}..refs/heads/{}", p.head);
        let commits = git_ok(&dir, &["--git-dir", d, "rev-list", "--reverse", &range]);
        let mut parent = base.clone();
        for c in commits.lines() {
            let tree = git_ok(
                &dir,
                &["--git-dir", d, "rev-parse", &format!("{c}^{{tree}}")],
            );
            let message = format!("rebased {c}");
            parent = git_ok(
                &dir,
                &[
                    "--git-dir",
                    d,
                    "-c",
                    "user.name=GitHub",
                    "-c",
                    "user.email=noreply@github.test",
                    "commit-tree",
                    &tree,
                    "-p",
                    &parent,
                    "-m",
                    &message,
                ],
            );
        }
        let base_ref = format!("refs/heads/{}", p.base);
        git_ok(
            &dir,
            &["--git-dir", d, "update-ref", &base_ref, &parent, &base],
        );
        self.mark_merged(repo, number, &parent);
        parent
    }

    fn mark_merged(&self, repo: &str, number: u64, sha: &str) {
        let mut st = self.state.lock().unwrap();
        let q = st
            .pulls
            .iter_mut()
            .find(|q| q.repo == repo && q.number == number)
            .unwrap();
        q.open = false;
        q.merge_sha = Some(sha.to_string());
    }

    /// Closes or reopens pull request `number` of `octo/demo`.
    pub fn set_open(&self, number: u64, open: bool) {
        let mut st = self.state.lock().unwrap();
        st.pulls
            .iter_mut()
            .find(|p| p.repo == "octo/demo" && p.number == number)
            .unwrap()
            .open = open;
    }

    pub fn check(&self, sha: &str, id: u64, name: &str, conclusion: &str, log: &str) {
        let mut st = self.state.lock().unwrap();
        st.checks.entry(sha.into()).or_default().push(json!({
            "id": id,
            "name": name,
            "status": "completed",
            "conclusion": conclusion,
            "html_url": format!("https://github.test/runs/{id}"),
            "output": {"title": format!("{name}: {conclusion}")},
        }));
        st.logs.insert(id, log.into());
    }

    /// Requests whose method and path match, with the login that made them.
    pub fn requests(&self, method: &str, path_part: &str) -> Vec<(String, String)> {
        self.state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|(m, p, _)| m == method && p.contains(path_part))
            .map(|(_, p, l)| (p.clone(), l.clone()))
            .collect()
    }

    pub fn not_modified(&self) -> usize {
        self.state.lock().unwrap().not_modified
    }

    /// Points a data directory's `branvil.toml` at this stand-in.
    pub fn configure(&self, data: &Path) {
        let path = data.join("branvil.toml");
        let mut doc: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let upstream = doc
            .entry("upstream")
            .or_insert_with(|| toml::Value::Table(Default::default()))
            .as_table_mut()
            .unwrap();
        // The tests poll explicitly (`/upstream/refresh`).
        upstream.insert("poll_active_secs".into(), toml::Value::Integer(3600));
        upstream.insert("poll_idle_secs".into(), toml::Value::Integer(3600));
        let mut github = toml::Table::new();
        github.insert("api_url".into(), toml::Value::String(self.url.clone()));
        github.insert(
            "web_url".into(),
            toml::Value::String("https://github.test".into()),
        );
        github.insert(
            "git_url".into(),
            toml::Value::String(self.root.to_string_lossy().into_owned()),
        );
        upstream.insert("github".into(), toml::Value::Table(github));
        std::fs::write(&path, toml::to_string(&doc).unwrap()).unwrap();
    }
}
