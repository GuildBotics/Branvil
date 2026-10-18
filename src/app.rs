//! The process-wide state shared by every request.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ci::Ci;
use crate::config::{Config, Layout};
use crate::domain::members::Codes;
use crate::events::Events;
use crate::git::Git;
use crate::store::Store;

pub struct App {
    pub config: Config,
    /// The permanent base URL (`http://127.0.0.1:7878`), never ending in `/`.
    pub base_url: String,
    pub layout: Layout,
    pub store: Store,
    pub git: Git,
    /// Smart HTTP transfers in progress (see `LimitsConfig::git_http_concurrency`).
    pub transfers: Arc<tokio::sync::Semaphore>,
    pub events: Events,
    pub ci: Ci,
    /// The upstream code host: following it and publishing to it.
    pub upstream: crate::upstream::Upstream,
    /// Signs collection cursors.
    pub cursor_key: Vec<u8>,
    /// Shared with the hook processes this service spawns (through the Git
    /// child's environment); authenticates their callbacks.
    pub hook_secret: String,
    /// Loopback URL the hooks call back on (the bound address).
    pub hook_url: String,
    /// The one boundary per repository for ref changes Branvil makes and for
    /// recording ref changes.
    repo_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Requests that carry the same `Idempotency-Key` of the same member run
    /// one at a time; other keys do not wait for them.
    pub idempotency: KeyedLocks,
    /// When a token's `last_used_at` was last written (to write it at most
    /// once a minute).
    pub token_touched: Mutex<HashMap<String, std::time::Instant>>,
    /// Unused entry-link codes, in memory only: a restart invalidates them.
    pub entry_codes: Mutex<Codes<String>>,
    /// Unused codes of local apps the owner allowed, in memory only.
    pub app_codes: Mutex<Codes<crate::domain::apps::Grant>>,
    /// (repo dir, base, head) → changed file count.
    pub changed_files: Mutex<HashMap<(String, String, String), u64>>,
    /// Test-only crash point (`BRANVIL_FAILPOINT`).
    failpoint: Option<String>,
}

pub type Shared = Arc<App>;

/// One async lock per key. An entry lives only while someone holds or waits
/// for its lock, so the map never grows past the requests in flight.
#[derive(Default)]
pub struct KeyedLocks(Mutex<HashMap<Vec<u8>, Arc<tokio::sync::Mutex<()>>>>);

pub struct KeyedGuard<'a> {
    locks: &'a KeyedLocks,
    key: Vec<u8>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl KeyedLocks {
    pub async fn lock(&self, key: &[u8]) -> KeyedGuard<'_> {
        let mutex = self
            .0
            .lock()
            .unwrap()
            .entry(key.to_vec())
            .or_default()
            .clone();
        // Created before waiting, so a cancelled waiter removes its entry too.
        let mut held = KeyedGuard {
            locks: self,
            key: key.to_vec(),
            guard: None,
        };
        held.guard = Some(mutex.lock_owned().await);
        held
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

impl Drop for KeyedGuard<'_> {
    fn drop(&mut self) {
        let mut map = self.locks.0.lock().unwrap();
        self.guard.take();
        if map
            .get(&self.key)
            .is_some_and(|m| Arc::strong_count(m) == 1)
        {
            map.remove(&self.key);
        }
    }
}

impl App {
    pub fn new(
        config: Config,
        base_url: String,
        store: Store,
        cursor_key: Vec<u8>,
        hook_url: String,
    ) -> Self {
        let layout = config.layout();
        let git = Git::new(
            layout.clone(),
            config.limits.git_concurrency,
            Duration::from_secs(config.limits.git_command_timeout_secs),
        );
        let events = Events::new();
        let ci = Ci::new(
            store.clone(),
            git.clone(),
            events.clone(),
            base_url.clone(),
            layout.clone(),
            config.ci.clone(),
            config.file.clone(),
        );
        Self {
            base_url,
            layout,
            store,
            git,
            transfers: Arc::new(tokio::sync::Semaphore::new(
                config.limits.git_http_concurrency.max(1),
            )),
            events,
            ci,
            upstream: crate::upstream::Upstream::new(&config.upstream),
            cursor_key,
            hook_secret: crate::util::random_secret::<32>(),
            hook_url,
            repo_locks: Mutex::new(HashMap::new()),
            idempotency: KeyedLocks::default(),
            token_touched: Mutex::new(HashMap::new()),
            entry_codes: Mutex::default(),
            app_codes: Mutex::default(),
            changed_files: Mutex::new(HashMap::new()),
            failpoint: std::env::var("BRANVIL_FAILPOINT")
                .ok()
                .filter(|v| !v.is_empty()),
            config,
        }
    }

    pub fn repo_lock(&self, repo_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.repo_locks.lock().unwrap();
        locks
            .entry(repo_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Simulates a crash at a named point when `BRANVIL_FAILPOINT` names it.
    /// Used by the recovery tests; inert otherwise.
    pub fn failpoint(&self, name: &str) {
        if self.failpoint.as_deref() == Some(name) {
            eprintln!("branvil: failpoint {name} reached, aborting");
            std::process::abort();
        }
    }

    pub fn pull_url(&self, repo: &str, number: u64) -> String {
        format!("{}/{repo}/-/pulls/{number}", self.base_url)
    }
    pub fn repo_url(&self, repo: &str) -> String {
        format!("{}/{repo}", self.base_url)
    }
    pub fn clone_url(&self, repo: &str) -> String {
        format!("{}/git/{repo}.git", self.base_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keyed_locks_serialize_one_key_only_and_forget_idle_keys() {
        let locks = Arc::new(KeyedLocks::default());
        let a = locks.lock(b"a").await;
        // Another key is not blocked by `a`.
        let b = tokio::time::timeout(Duration::from_secs(1), locks.lock(b"b"))
            .await
            .expect("another key waits for nobody");
        drop(b);
        // The same key waits until `a` is released.
        let l2 = locks.clone();
        let waiter = tokio::spawn(async move {
            let _g = l2.lock(b"a").await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "the same key must wait");
        drop(a);
        waiter.await.unwrap();
        assert_eq!(locks.len(), 0, "released keys are forgotten");
        // A waiter that gives up leaves nothing behind either.
        let held = locks.lock(b"c").await;
        let gave_up = tokio::time::timeout(Duration::from_millis(20), locks.lock(b"c")).await;
        assert!(gave_up.is_err());
        drop(held);
        assert_eq!(locks.len(), 0);
    }
}
