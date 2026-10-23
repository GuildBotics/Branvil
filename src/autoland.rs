//! Landing pull requests automatically (`docs/architecture.md`, "Landing
//! automatically"): in a repository with `auto_land`, an open pull request
//! that is not a draft lands by itself once it can, merged here or, with an
//! upstream, published to the host. A publish made this way is told once to
//! the owner's webhook, since people merge it there.

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{FutureExt, StreamExt};
use serde::{Deserialize, Serialize};

use crate::app::{App, Shared};
use crate::auth::Caller;
use crate::domain::member_by_name;
use crate::domain::pulls::{self, MergeInput, load_pull};
use crate::error::{AppError, AppResult};
use crate::events::Notice;
use crate::http::J;
use crate::model::{Published, Pull, Repo};
use crate::store::{self, Key, Rd};
use crate::util;

/// Every repository is looked at again this often, whatever changed.
const TICK: Duration = Duration::from_secs(60);

/// The webhook's client and how its last send went.
pub struct Autoland {
    client: reqwest::Client,
    /// Why the last send to the webhook failed (the next sweep tries again).
    problem: Mutex<Option<String>>,
}

impl Default for Autoland {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent(concat!("branvil/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("an HTTP client"),
            problem: Mutex::new(None),
        }
    }
}

/// The repositories a sweep looks at.
#[derive(Default)]
struct Dirty {
    all: bool,
    paths: BTreeSet<String>,
}

impl Dirty {
    fn add(&mut self, notice: Notice) {
        match notice {
            Notice::Reset => self.all = true,
            Notice::Change(c) if ["pull", "check", "repo", "upstream"].contains(&c.kind) => {
                self.paths.insert(c.path);
            }
            Notice::Change(_) => {}
        }
    }
}

/// Starts the one task that lands pull requests: after the changes that can
/// make one landable (it drains what arrived, then sweeps their repositories
/// once), at start, and every `TICK` (every repository). Sweeps never overlap.
/// Ends when the service shuts down. Call once, after startup recovery.
pub fn start(app: Shared) {
    let mut events = Box::pin(app.events.subscribe());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let mut dirty = Dirty::default();
            tokio::select! {
                _ = tick.tick() => dirty.all = true,
                notice = events.next() => match notice {
                    Some(n) => dirty.add(n),
                    None => return,
                },
            }
            loop {
                match events.next().now_or_never() {
                    Some(Some(n)) => dirty.add(n),
                    Some(None) => return,
                    None => break,
                }
            }
            sweep(&app, &dirty).await;
        }
    });
}

async fn sweep(app: &App, dirty: &Dirty) {
    let repos = match app
        .store
        .read(|r| r.prefix_all::<Repo>(store::REPOS, &[]))
        .await
    {
        Ok(r) => r,
        Err(e) => return tracing::error!("autoland: {e}"),
    };
    for repo in repos
        .iter()
        .filter(|r| dirty.all || dirty.paths.contains(&r.path))
    {
        if let Err(e) = sweep_repo(app, repo).await {
            tracing::warn!("autoland: {}: {e}", repo.path);
        }
    }
}

/// Published automatically, there, and the webhook not yet told.
fn awaits_notice(x: &Published) -> bool {
    x.auto && x.number.is_some() && x.notified_at.is_none()
}

/// Tells the webhook of the repository's automatic publishes it was not told
/// of, then lands what it can when it lands automatically.
async fn sweep_repo(app: &App, repo: &Repo) -> AppResult<()> {
    let r2 = repo.clone();
    let (notices, candidates) = app
        .store
        .read(move |r| {
            let mut notices = Vec::new();
            let mut candidates = Vec::new();
            for p in open_pulls(r, &r2)? {
                if p.upstream.as_ref().is_some_and(awaits_notice) {
                    notices.push(p.number);
                }
                if r2.auto_land && !p.draft && worth_trying(r, &r2, &p)? {
                    candidates.push(p);
                }
            }
            Ok((notices, candidates))
        })
        .await?;
    for number in notices {
        notify(app, repo, number).await?;
    }
    if candidates.is_empty() {
        return Ok(());
    }
    let tips = pulls::tips(app, repo).await?;
    for p in candidates {
        let input = MergeInput {
            expected_head: tips.get(&p.head).cloned().unwrap_or_default(),
            expected_base: tips.get(&p.base).cloned().unwrap_or_default(),
        };
        let (done, landed) = match repo.upstream {
            None => (
                "merged",
                pulls::merge_pull(app, repo, p.number, input).await,
            ),
            Some(_) => (
                "published",
                crate::upstream::ops::publish_pull(app, repo, p.number, input, true).await,
            ),
        };
        match landed {
            Ok(_) => tracing::info!("autoland: {} !{} {done}", repo.path, p.number),
            // Not yet: the next change or tick tries again.
            Err(e)
                if ["not_mergeable", "head_moved", "base_moved", "no_account"]
                    .contains(&e.code) =>
            {
                tracing::debug!("autoland: {} !{}: {e}", repo.path, p.number)
            }
            Err(e) => tracing::warn!("autoland: {} !{}: {e}", repo.path, p.number),
        }
    }
    Ok(())
}

fn open_pulls(r: &impl Rd, repo: &Repo) -> AppResult<Vec<Pull>> {
    let prefix = Key::new().s(&repo.id).bytes();
    let mut out = Vec::new();
    for (_, n) in r.prefix_raw(store::IX_OPEN_PULLS, &prefix, None, usize::MAX)? {
        let n = String::from_utf8_lossy(&n)
            .parse()
            .map_err(AppError::internal)?;
        out.push(load_pull(r, repo, n)?);
    }
    Ok(out)
}

/// Whether landing `p` is worth trying: a publish needs a pull request into
/// the default branch, never published before (Branvil publishes it once),
/// whose author has an account on the host. Merging needs nothing more.
fn worth_trying(r: &impl Rd, repo: &Repo, p: &Pull) -> AppResult<bool> {
    let Some(up) = &repo.upstream else {
        return Ok(true);
    };
    Ok(p.base == repo.default_branch
        && p.upstream.is_none()
        && member_by_name(r, &p.author)?
            .accounts
            .iter()
            .any(|a| a.provider == up.provider))
}

// ---------------------------------------------------------------- the webhook

/// Tells the webhook, once, that Branvil published pull request `number`.
/// Without a webhook nothing is sent; a failure is kept and tried again.
async fn notify(app: &App, repo: &Repo, number: u64) -> AppResult<()> {
    let Ok(url) = std::fs::read_to_string(app.layout.webhook()) else {
        return Ok(());
    };
    // The poller rewrites the record under the same lock.
    let _guard = app.upstream.pull_lock(&repo.id, number).await;
    let r2 = repo.clone();
    let p = app.store.read(move |r| load_pull(r, &r2, number)).await?;
    let Some(mut x) = p.upstream.filter(awaits_notice) else {
        return Ok(());
    };
    let text = format!(
        "{} ({} !{number}) was published to {} automatically and awaits a merge there.\n{}\n{}",
        p.title,
        repo.path,
        app.upstream.host_name(&x.provider),
        app.pull_url(&repo.path, number),
        x.url.clone().unwrap_or_default()
    );
    let sent = app
        .autoland
        .client
        .post(url.trim())
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        // The URL is a secret: it stays out of the message.
        .map_err(|e| e.without_url().to_string())
        .and_then(|r| match r.status() {
            s if s.is_success() => Ok(()),
            s => Err(format!("the webhook answered {s}")),
        });
    if let Err(why) = sent {
        tracing::warn!(
            "autoland: telling the webhook of {} !{number} failed: {why}",
            repo.path
        );
        *app.autoland.problem.lock().unwrap() = Some(why);
        return Ok(());
    }
    *app.autoland.problem.lock().unwrap() = None;
    x.notified_at = Some(util::now());
    let r2 = repo.clone();
    app.store
        .write(move |w| pulls::store_published(w, &r2, number, x, None))
        .await?;
    app.events.publish("pull", &repo.path, number.to_string());
    Ok(())
}

/// The webhook as the API shows it: never its URL, which is a credential of
/// another system.
#[derive(Serialize)]
struct WebhookView {
    set: bool,
    /// The host of its URL.
    host: Option<String>,
    /// Why the last send failed.
    problem: Option<String>,
}

fn webhook_view(app: &App) -> WebhookView {
    let host = std::fs::read_to_string(app.layout.webhook()).ok().map(|u| {
        reqwest::Url::parse(u.trim())
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default()
    });
    WebhookView {
        set: host.is_some(),
        host,
        problem: app.autoland.problem.lock().unwrap().clone(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebhookInput {
    url: String,
}

async fn webhook_get(State(app): State<Shared>, c: Caller) -> AppResult<Response> {
    c.require_owner()?;
    Ok(axum::Json(webhook_view(&app)).into_response())
}

async fn webhook_put(
    State(app): State<Shared>,
    c: Caller,
    J(input): J<WebhookInput>,
) -> AppResult<Response> {
    c.require_owner()?;
    let url = reqwest::Url::parse(input.url.trim())
        .ok()
        .filter(|u| ["http", "https"].contains(&u.scheme()) && u.host_str().is_some())
        .ok_or_else(|| AppError::invalid("The webhook URL must be an http or https URL."))?;
    crate::config::write_secret(&app.layout.webhook(), url.as_str().as_bytes())
        .map_err(AppError::internal)?;
    *app.autoland.problem.lock().unwrap() = None;
    Ok(axum::Json(webhook_view(&app)).into_response())
}

async fn webhook_delete(State(app): State<Shared>, c: Caller) -> AppResult<Response> {
    c.require_owner()?;
    let _ = std::fs::remove_file(app.layout.webhook());
    *app.autoland.problem.lock().unwrap() = None;
    Ok(axum::Json(webhook_view(&app)).into_response())
}

/// The webhook's routes, merged into `/api/v1`.
pub fn routes() -> Router<Shared> {
    Router::new().route(
        "/admin/webhook",
        get(webhook_get).put(webhook_put).delete(webhook_delete),
    )
}
