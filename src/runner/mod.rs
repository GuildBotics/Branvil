//! `branvil runner`: the CI runner, a separate process from the service. It
//! claims attempts over the runner API, fetches the commit as a tarball made
//! by the service (it never touches repositories or the database), runs the
//! job in a microVM, streams the log, uploads artifacts, and reports the
//! conclusion, renewing its lease while it works. See `docs/ci.md`.

pub mod executor;
pub mod workspace;
pub mod zip;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Subcommand;
use serde::Deserialize;
use tokio::sync::{mpsc, watch};

use crate::ci::config::ExecutorConfig;
use crate::ci::pipeline::JobDef;
use executor::{Executor, Health, ImageCheck, JobRun, Microsandbox, Outcome};

#[derive(Subcommand)]
pub enum RunnerCommand {
    /// Create a runner member and print its token once (needs the owner's token)
    Register {
        /// Name of the runner member, e.g. runner1
        name: String,
    },
    /// Pull an image into the runner's image cache (jobs never pull)
    Pull {
        /// Image reference, preferably pinned by digest
        image: String,
    },
    /// Show which msb is used and whether the executor is ready
    Doctor,
    /// Copy an msb binary (and the libkrunfw next to it) into ~/.branvil/msb
    InstallMsb {
        /// Path of an msb binary that reports the pinned version
        #[arg(long)]
        from: PathBuf,
    },
}

/// What the runner process needs, resolved from flags, environment, and the
/// optional configuration.
pub struct Settings {
    pub url: String,
    pub token: Option<String>,
    pub work_dir: PathBuf,
    pub executor: ExecutorConfig,
    pub msb_path: Option<PathBuf>,
    pub msb_home: PathBuf,
}

impl Settings {
    pub fn new(
        url: Option<String>,
        token: Option<String>,
        work_dir: Option<PathBuf>,
        config: Option<&crate::config::Config>,
    ) -> anyhow::Result<Self> {
        let ci = config.map(|c| c.ci.clone()).unwrap_or_default();
        let url = url
            .or_else(|| config.and_then(|c| c.base_url.clone()))
            .unwrap_or_else(|| format!("http://{}", crate::config::DEFAULT_LISTEN));
        Ok(Self {
            url: crate::config::normalize_base_url(&url)?,
            token,
            work_dir: work_dir
                .or(ci.executor.work_dir.clone())
                .unwrap_or_else(|| executor::branvil_home().join("runner")),
            msb_path: ci.microsandbox.msb_path.clone(),
            msb_home: ci
                .microsandbox
                .home
                .clone()
                .unwrap_or_else(|| executor::branvil_home().join("msb-home")),
            executor: ci.executor,
        })
    }

    fn microsandbox(&self, runner: &str) -> Microsandbox {
        Microsandbox::new(
            executor::resolve_msb(self.msb_path.as_deref()),
            self.msb_home.clone(),
            runner,
        )
    }

    fn api(&self) -> anyhow::Result<Api> {
        let token = self
            .token
            .clone()
            .context("a runner token is needed: pass --token or set BRANVIL_TOKEN (`branvil runner register`)")?;
        Ok(Api {
            url: self.url.clone(),
            token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .build()?,
        })
    }
}

// ---------------------------------------------------------------- service client

pub struct Api {
    url: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Debug)]
pub enum ApiError {
    /// The lease is not current any more: stop, report nothing.
    Lost,
    /// The token is not valid (revoked): the runner stops.
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Lost => write!(f, "the lease is no longer current"),
            ApiError::Unauthorized => write!(f, "the runner token is not valid (revoked?)"),
            ApiError::Other(m) => write!(f, "{m}"),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Claim {
    pub attempt_id: String,
    pub lease: String,
    pub lease_secs: u64,
    pub repo: String,
    pub sha: String,
    pub attempt: u64,
    pub job: JobDef,
    pub cache_scope: String,
    pub network: bool,
}

impl Api {
    pub fn new(url: &str, token: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    fn at(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.url)
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response, ApiError> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        if status == reqwest::StatusCode::CONFLICT {
            return Err(ApiError::Lost);
        }
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        Err(ApiError::Other(format!(
            "{} ({})",
            body["error"]["message"]
                .as_str()
                .unwrap_or("request failed"),
            status.as_u16()
        )))
    }

    fn net(e: reqwest::Error) -> ApiError {
        ApiError::Other(format!("Branvil is not reachable: {e}"))
    }

    pub async fn me(&self) -> Result<serde_json::Value, ApiError> {
        let resp = self
            .http
            .get(self.at("/me"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(Self::net)?;
        Self::check(resp).await?.json().await.map_err(Self::net)
    }

    pub async fn claim(&self, executor: &str, health: &Health) -> Result<Option<Claim>, ApiError> {
        let body = serde_json::json!({
            "executor": executor,
            "executor_state": if health.ready { "ready" } else { "unavailable" },
            "executor_detail": health.detail,
        });
        let resp = self
            .http
            .post(self.at("/runner/claim"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(Self::net)?;
        let resp = Self::check(resp).await?;
        if resp.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        resp.json().await.map(Some).map_err(Self::net)
    }

    pub async fn source(&self, c: &Claim) -> Result<Vec<u8>, ApiError> {
        let resp = self
            .http
            .get(self.at(&format!("/runner/attempts/{}/source", c.attempt_id)))
            .bearer_auth(&self.token)
            .header(crate::ci::api::LEASE_HEADER, &c.lease)
            .send()
            .await
            .map_err(Self::net)?;
        Ok(Self::check(resp)
            .await?
            .bytes()
            .await
            .map_err(Self::net)?
            .to_vec())
    }

    /// Returns whether the service asks the runner to stop.
    pub async fn heartbeat(&self, c: &Claim) -> Result<bool, ApiError> {
        let resp = self
            .http
            .post(self.at(&format!("/runner/attempts/{}/heartbeat", c.attempt_id)))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"lease": c.lease}))
            .send()
            .await
            .map_err(Self::net)?;
        let v: serde_json::Value = Self::check(resp).await?.json().await.map_err(Self::net)?;
        Ok(v["cancel"].as_bool().unwrap_or(false))
    }

    /// Returns whether the service truncated the log.
    pub async fn log(&self, c: &Claim, bytes: Vec<u8>) -> Result<bool, ApiError> {
        let resp = self
            .http
            .post(self.at(&format!("/runner/attempts/{}/log", c.attempt_id)))
            .bearer_auth(&self.token)
            .header(crate::ci::api::LEASE_HEADER, &c.lease)
            .body(bytes)
            .send()
            .await
            .map_err(Self::net)?;
        let v: serde_json::Value = Self::check(resp).await?.json().await.map_err(Self::net)?;
        Ok(v["truncated"].as_bool().unwrap_or(false))
    }

    pub async fn artifact(&self, c: &Claim, name: &str, data: Vec<u8>) -> Result<(), ApiError> {
        let resp = self
            .http
            .put(self.at(&format!(
                "/runner/attempts/{}/artifacts/{name}",
                c.attempt_id
            )))
            .bearer_auth(&self.token)
            .header(crate::ci::api::LEASE_HEADER, &c.lease)
            .body(data)
            .send()
            .await
            .map_err(Self::net)?;
        Self::check(resp).await.map(|_| ())
    }

    pub async fn finish(
        &self,
        c: &Claim,
        conclusion: &str,
        details: Option<String>,
    ) -> Result<(), ApiError> {
        let resp = self
            .http
            .post(self.at(&format!("/runner/attempts/{}/finish", c.attempt_id)))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"lease": c.lease, "conclusion": conclusion, "details": details}))
            .send()
            .await
            .map_err(Self::net)?;
        Self::check(resp).await.map(|_| ())
    }
}

// ---------------------------------------------------------------- commands

pub async fn register(url: &str, owner_token: &str, name: &str) -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let call = |method: reqwest::Method, path: String, body: serde_json::Value| {
        http.request(method, format!("{url}/api/v1{path}"))
            .bearer_auth(owner_token)
            .json(&body)
            .send()
    };
    let resp = call(
        reqwest::Method::POST,
        "/members".into(),
        serde_json::json!({"name": name, "kind": "runner"}),
    )
    .await
    .with_context(|| format!("Branvil is not reachable at {url}"))?;
    if !resp.status().is_success() {
        let v: serde_json::Value = resp.json().await.unwrap_or_default();
        bail!(
            "creating runner member {name}: {}",
            v["error"]["message"].as_str().unwrap_or("failed")
        );
    }
    let resp = call(
        reqwest::Method::POST,
        format!("/members/{}/tokens", crate::util::encode_segment(name)),
        serde_json::json!({"label": "runner"}),
    )
    .await?;
    let v: serde_json::Value = resp.json().await.unwrap_or_default();
    let token = v["token"]
        .as_str()
        .context("the service did not return a token")?;
    println!("{token}");
    Ok(())
}

pub async fn pull(settings: &Settings, image: &str) -> anyhow::Result<()> {
    if !crate::ci::pipeline::valid_image(image) {
        bail!("'{image}' is not an image reference");
    }
    let ms = settings.microsandbox("pull");
    let Some(mut cmd) = ms.command() else {
        bail!(
            "{}",
            match ms.msb() {
                executor::Msb::Missing(m) => m.clone(),
                executor::Msb::Found(_) => String::new(),
            }
        );
    };
    let _lock = ms
        .lock_images(true, || {
            eprintln!("waiting for another image pull or load to finish")
        })
        .await
        .map_err(anyhow::Error::msg)?;
    // Bounded like the GUI's pulls: jobs wait for the lock it holds.
    let status = tokio::time::timeout(
        executor::IMAGE_TRANSFER,
        cmd.args(["pull", image])
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status(),
    )
    .await
    .with_context(|| format!("msb pull {image} timed out"))??;
    if !status.success() {
        bail!("msb pull {image} failed ({status})");
    }
    Ok(())
}

pub async fn doctor(settings: &Settings) -> anyhow::Result<u8> {
    let ms = settings.microsandbox("doctor");
    match ms.msb() {
        executor::Msb::Found(p) => println!("msb: {} (msb {})", p.display(), executor::MSB_VERSION),
        executor::Msb::Missing(m) => println!("msb: {m}"),
    }
    println!("MSB_HOME: {}", ms.home().display());
    println!("work directory: {}", settings.work_dir.display());
    let h = ms.health().await;
    println!(
        "executor: {}{}",
        if h.ready { "ready" } else { "unavailable" },
        h.detail.map(|d| format!(" ({d})")).unwrap_or_default()
    );
    Ok(if h.ready { 0 } else { 1 })
}

/// Copies an msb binary that reports the pinned version, and the libkrunfw
/// library next to it (`../lib/`), into `~/.branvil/msb`.
pub fn install_msb(from: &Path) -> anyhow::Result<()> {
    match executor::resolve_msb(Some(from)) {
        executor::Msb::Found(p) if p == from => {}
        _ => bail!(
            "{} is not an msb {} binary",
            from.display(),
            executor::MSB_VERSION
        ),
    }
    let dest = executor::branvil_home().join("msb");
    let lib_src = from
        .parent()
        .and_then(Path::parent)
        .map(|p| p.join("lib"))
        .context("cannot find the lib directory next to msb")?;
    std::fs::create_dir_all(dest.join("bin"))?;
    std::fs::create_dir_all(dest.join("lib"))?;
    let bin = dest
        .join("bin")
        .join(from.file_name().context("msb file name")?);
    std::fs::copy(from, &bin)?;
    let mut libs = 0;
    for e in std::fs::read_dir(&lib_src)
        .with_context(|| format!("reading {}", lib_src.display()))?
        .flatten()
    {
        if e.file_name().to_string_lossy().starts_with("libkrunfw") && e.path().is_file() {
            std::fs::copy(e.path(), dest.join("lib").join(e.file_name()))?;
            libs += 1;
        }
    }
    if libs == 0 {
        bail!("no libkrunfw found in {}", lib_src.display());
    }
    println!("installed {} and {libs} libkrunfw file(s)", bin.display());
    Ok(())
}

/// Runs the runner until the process is stopped.
pub async fn run(settings: Settings) -> anyhow::Result<()> {
    let api = settings.api()?;
    let me = api
        .me()
        .await
        .map_err(|e| anyhow::anyhow!("cannot reach Branvil at {}: {e}", settings.url))?;
    if me["kind"] != "runner" {
        bail!(
            "the token belongs to a {} member; the runner needs a runner member's token (`branvil runner register`)",
            me["kind"]
        );
    }
    let name = me["name"].as_str().unwrap_or("runner").to_string();
    std::fs::create_dir_all(&settings.work_dir)?;
    let lock = std::fs::File::create(settings.work_dir.join("runner.lock"))?;
    if lock.try_lock().is_err() {
        bail!(
            "another runner uses {}; give each runner its own --work-dir",
            settings.work_dir.display()
        );
    }
    let executor = settings.microsandbox(&name);
    let runner = Runner {
        api,
        work_dir: settings.work_dir.clone(),
        limits: settings.executor.clone(),
    };
    tracing::info!(
        "runner {name} serving {} (work directory {})",
        settings.url,
        settings.work_dir.display()
    );
    runner.serve(&executor).await
}

pub struct Runner {
    pub api: Api,
    pub work_dir: PathBuf,
    pub limits: ExecutorConfig,
}

impl Runner {
    /// Sweeps leftovers, then claims and runs attempts forever.
    pub async fn serve<E: Executor>(&self, executor: &E) -> anyhow::Result<()> {
        executor.sweep().await;
        let _ = std::fs::remove_dir_all(self.work_dir.join("jobs"));
        let mut backoff = 1;
        loop {
            match self.once(executor).await {
                Ok(true) => backoff = 1,
                Ok(false) => {
                    backoff = 1;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(ApiError::Unauthorized) => bail!("{}", ApiError::Unauthorized),
                Err(e) => {
                    tracing::warn!("{e}");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(30);
                }
            }
        }
    }

    /// Claims and runs at most one attempt. Returns whether there was one.
    pub async fn once<E: Executor>(&self, executor: &E) -> Result<bool, ApiError> {
        let health = executor.health().await;
        let Some(claim) = self.api.claim(executor.name(), &health).await? else {
            return Ok(false);
        };
        tracing::info!(
            "attempt {} ({} #{} of {} {})",
            claim.attempt_id,
            claim.job.name,
            claim.attempt,
            claim.repo,
            claim.sha
        );
        let result = self.attempt(executor, &health, &claim).await;
        let _ = std::fs::remove_dir_all(self.job_dir(&claim));
        match result {
            Ok(()) => Ok(true),
            Err(ApiError::Lost) => {
                tracing::warn!(
                    "attempt {}: the lease was lost; result dropped",
                    claim.attempt_id
                );
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    fn job_dir(&self, c: &Claim) -> PathBuf {
        self.work_dir.join("jobs").join(&c.attempt_id)
    }

    /// The job with this runner's limits applied (and notes on what changed).
    fn bounded(&self, job: &JobDef, notes: &mut Vec<String>) -> JobDef {
        let mut j = job.clone();
        let l = &self.limits;
        if j.cpus > l.max_cpus {
            notes.push(format!(
                "cpus {} exceeds the runner limit {}; using {}",
                j.cpus, l.max_cpus, l.max_cpus
            ));
            j.cpus = l.max_cpus;
        }
        if j.memory_mib > l.max_memory_mib {
            notes.push(format!(
                "memory_mib {} exceeds the runner limit {}; using {}",
                j.memory_mib, l.max_memory_mib, l.max_memory_mib
            ));
            j.memory_mib = l.max_memory_mib;
        }
        if j.timeout_secs > l.max_timeout_secs {
            notes.push(format!(
                "timeout_secs {} exceeds the runner limit {}; using {}",
                j.timeout_secs, l.max_timeout_secs, l.max_timeout_secs
            ));
            j.timeout_secs = l.max_timeout_secs;
        }
        j
    }

    async fn attempt<E: Executor>(
        &self,
        executor: &E,
        health: &Health,
        c: &Claim,
    ) -> Result<(), ApiError> {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let lost = Arc::new(AtomicBool::new(false));
        let beat = tokio::spawn(heartbeats(
            Api::new(&self.api.url, &self.api.token),
            c.clone(),
            cancel_tx,
            lost.clone(),
        ));
        let (log_tx, log_rx) = mpsc::channel::<Vec<u8>>(256);
        let uploader = tokio::spawn(upload_log(
            Api::new(&self.api.url, &self.api.token),
            c.clone(),
            log_rx,
        ));
        let result = self
            .work(executor, health, c, log_tx, cancel_rx, &lost)
            .await;
        let _ = uploader.await;
        beat.abort();
        if lost.load(Ordering::SeqCst) {
            return Err(ApiError::Lost);
        }
        let (conclusion, details) = result;
        self.api.finish(c, conclusion, details).await
    }

    /// Everything between the claim and the finish. Returns the conclusion
    /// and its details.
    async fn work<E: Executor>(
        &self,
        executor: &E,
        health: &Health,
        c: &Claim,
        log: mpsc::Sender<Vec<u8>>,
        cancel: watch::Receiver<bool>,
        lost: &AtomicBool,
    ) -> (&'static str, Option<String>) {
        let say = |text: String| {
            let log = log.clone();
            async move {
                let _ = log.send(format!("[branvil] {text}\n").into_bytes()).await;
            }
        };
        if !health.ready {
            let why = health
                .detail
                .clone()
                .unwrap_or_else(|| "the executor is not usable".into());
            say(format!("executor {} unavailable: {why}", executor.name())).await;
            return (
                "startup_failure",
                Some(format!("The runner's executor is unavailable: {why}")),
            );
        }
        let mut notes = Vec::new();
        let job = self.bounded(&c.job, &mut notes);
        say(format!(
            "attempt {} of job {} on {} {} (executor {}, image {}, {} cpu, {} MiB, timeout {} s, network {})",
            c.attempt,
            job.name,
            c.repo,
            c.sha,
            executor.name(),
            job.image,
            job.cpus,
            job.memory_mib,
            job.timeout_secs,
            if c.network { "public only" } else { "off" }
        ))
        .await;
        for n in notes.drain(..) {
            say(n).await;
        }
        // Checked before the source is fetched; the lock that comes with the
        // check keeps a load or a pull from re-pointing the checked tag until
        // the VM has picked the image.
        let mut stopping = cancel.clone();
        let checked = tokio::select! {
            checked = executor.resolve_image(&job.image, &log) => checked.map_err(Some),
            Ok(_) = stopping.wait_for(|stop| *stop) => Err(None),
        };
        let (check, image_lock) = match checked {
            Ok(checked) => checked,
            Err(None) => {
                say("job ended: cancelled while waiting for the image cache".into()).await;
                return ("cancelled", Some("Stopped on request.".to_string()));
            }
            Err(Some(e)) => {
                return (
                    "startup_failure",
                    Some(format!("Checking the image failed: {e}")),
                );
            }
        };
        let run_image = match check {
            ImageCheck::Present(reference) => reference,
            ImageCheck::Missing => {
                let why = format!(
                    "The image {} is not in the runner's image cache (jobs never pull); pull or load it under Settings → CI for the runner on the Branvil machine, or run `branvil runner pull {}` on the runner's machine.",
                    job.image, job.image
                );
                say(why.clone()).await;
                return ("startup_failure", Some(why));
            }
            ImageCheck::Other(held) => {
                let why = format!(
                    "The runner's image cache holds the tag of {} as an image whose digest is {held}; load the pinned image into the runner's cache (Settings → CI for the runner on the Branvil machine).",
                    job.image
                );
                say(why.clone()).await;
                return ("startup_failure", Some(why));
            }
        };
        // The work directory as the VM will see it: canonical (the runner's
        // directory may sit behind system links such as macOS's /var), and
        // with no link anywhere below the runner's directory.
        let root = match std::fs::create_dir_all(&self.work_dir)
            .and_then(|_| std::fs::canonicalize(&self.work_dir))
        {
            Ok(r) => r,
            Err(e) => {
                return (
                    "startup_failure",
                    Some(format!("Cannot use the runner directory: {e}")),
                );
            }
        };
        let rel = format!("jobs/{}/work", c.attempt_id);
        let log_rel = format!("jobs/{}/log", c.attempt_id);
        let dir = root.join("jobs").join(&c.attempt_id);
        let work = root.join(&rel);
        let log_dir = root.join(&log_rel);
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = std::fs::create_dir_all(&work).and_then(|_| std::fs::create_dir(&log_dir)) {
            return (
                "startup_failure",
                Some(format!("Cannot create the work directory: {e}")),
            );
        }
        match self.api.source(c).await {
            Ok(tar) => {
                let w = work.clone();
                let unpacked =
                    tokio::task::spawn_blocking(move || workspace::unpack(&tar, &w)).await;
                if !matches!(unpacked, Ok(Ok(()))) {
                    return (
                        "startup_failure",
                        Some("Unpacking the source failed.".into()),
                    );
                }
            }
            Err(ApiError::Lost) => {
                lost.store(true, Ordering::SeqCst);
                return ("cancelled", None);
            }
            Err(e) => {
                return (
                    "startup_failure",
                    Some(format!("Fetching the source failed: {e}")),
                );
            }
        }
        // Re-check right before the VM gets the directories.
        if let Err(e) = workspace::no_links_below(&root, &rel)
            .and_then(|_| workspace::no_links_below(&root, &log_rel))
        {
            return (
                "startup_failure",
                Some(format!("Unsafe work directory: {e}")),
            );
        }
        let cache_root = self.work_dir.join("cache");
        let cache_key = job
            .cache
            .as_ref()
            .map(|cache| workspace::cache_key(&c.repo, &job, cache, &c.cache_scope, &work));
        if let (Some(cache), Some(key)) = (&job.cache, &cache_key) {
            let mut keys = vec![key.clone()];
            if c.cache_scope != "protected" {
                keys.push(workspace::cache_key(
                    &c.repo,
                    &job,
                    cache,
                    "protected",
                    &work,
                ));
            }
            match workspace::restore(&cache_root, &keys, cache, &work) {
                Ok(Some(k)) if &k == key => say("cache restored".into()).await,
                Ok(Some(_)) => {
                    say("cache restored from the protected branches' cache (read only)".into())
                        .await
                }
                Ok(None) => say("no cache entry yet".into()).await,
                Err(e) => say(format!("cache restore failed: {e}")).await,
            }
        }
        let env = vec![
            ("CI".to_string(), "true".to_string()),
            ("BRANVIL_CI".to_string(), "true".to_string()),
            ("BRANVIL_REPOSITORY".to_string(), c.repo.clone()),
            ("BRANVIL_SHA".to_string(), c.sha.clone()),
            ("BRANVIL_JOB".to_string(), job.name.clone()),
            ("BRANVIL_ATTEMPT_ID".to_string(), c.attempt_id.clone()),
            ("BRANVIL_ATTEMPT".to_string(), c.attempt.to_string()),
        ];
        let outcome = executor
            .run(
                JobRun {
                    attempt_id: &c.attempt_id,
                    job: &job,
                    image: &run_image,
                    image_lock,
                    work: &work,
                    log_dir: &log_dir,
                    network: c.network,
                    env,
                },
                log.clone(),
                cancel,
            )
            .await;
        if lost.load(Ordering::SeqCst) {
            return ("cancelled", None);
        }
        let (conclusion, details) = match &outcome {
            Outcome::Exited(0) => ("success", None),
            Outcome::Exited(code) => (
                "failure",
                Some(format!("A command exited with code {code}.")),
            ),
            Outcome::TimedOut => (
                "timed_out",
                Some(format!(
                    "The job ran longer than {} s and was stopped.",
                    job.timeout_secs
                )),
            ),
            Outcome::Cancelled => ("cancelled", Some("Stopped on request.".to_string())),
            Outcome::StartupFailure(m) => ("startup_failure", Some(m.clone())),
        };
        say(format!(
            "job ended: {conclusion}{}",
            details
                .as_ref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        ))
        .await;
        if matches!(outcome, Outcome::Exited(_) | Outcome::TimedOut) {
            let mut budget = self.limits.artifacts_max_mib << 20;
            for a in &job.artifacts {
                let mut notes = Vec::new();
                let packed = workspace::pack(&work, a, &mut budget, &mut notes);
                for n in notes {
                    say(n).await;
                }
                match packed {
                    Ok(Some(p)) => match self.api.artifact(c, &p.name, p.data).await {
                        Ok(()) => {
                            say(format!(
                                "artifact {} uploaded ({} file(s))",
                                p.name, p.files
                            ))
                            .await
                        }
                        Err(ApiError::Lost) => {
                            lost.store(true, Ordering::SeqCst);
                            return ("cancelled", None);
                        }
                        Err(e) => say(format!("artifact {}: upload failed: {e}", p.name)).await,
                    },
                    Ok(None) => {}
                    Err(e) => say(format!("artifact {}: {e}", a.name)).await,
                }
            }
        }
        if conclusion == "success"
            && let (Some(cache), Some(key)) = (&job.cache, &cache_key)
        {
            match workspace::save(
                &cache_root,
                key,
                cache,
                &work,
                self.limits.cache_max_mib << 20,
            ) {
                Ok(bytes) => {
                    say(format!(
                        "cache saved ({bytes} bytes, scope {})",
                        c.cache_scope
                    ))
                    .await
                }
                Err(e) => say(format!("cache save failed: {e}")).await,
            }
        }
        (conclusion, details)
    }
}

/// Renews the lease every third of its length. The service asking to stop,
/// or the lease turning out stale (or not renewable before it expires),
/// stops the job.
async fn heartbeats(api: Api, c: Claim, cancel: watch::Sender<bool>, lost: Arc<AtomicBool>) {
    let every = Duration::from_secs((c.lease_secs / 3).clamp(1, 15));
    let mut last_ok = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(every).await;
        match api.heartbeat(&c).await {
            Ok(stop) => {
                last_ok = tokio::time::Instant::now();
                if stop {
                    let _ = cancel.send(true);
                }
            }
            Err(ApiError::Lost) => {
                lost.store(true, Ordering::SeqCst);
                let _ = cancel.send(true);
                return;
            }
            Err(e) => {
                tracing::warn!("heartbeat of {}: {e}", c.attempt_id);
                if last_ok.elapsed() > Duration::from_secs(c.lease_secs) {
                    // The lease has expired by now: nobody will accept the result.
                    lost.store(true, Ordering::SeqCst);
                    let _ = cancel.send(true);
                    return;
                }
            }
        }
    }
}

/// Sends the log in batches (at most every 500 ms or 64 KiB) until every
/// sender is gone or the service says the log is full.
async fn upload_log(api: Api, c: Claim, mut rx: mpsc::Receiver<Vec<u8>>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut full = false;
    loop {
        let more = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
        let closed = matches!(more, Ok(None));
        if let Ok(Some(bytes)) = more {
            if !full {
                buf.extend_from_slice(&bytes);
            }
            if buf.len() < 64 << 10 {
                continue;
            }
        }
        while !buf.is_empty() && !full {
            let take = buf.len().min(512 << 10);
            let chunk: Vec<u8> = buf.drain(..take).collect();
            match api.log(&c, chunk).await {
                Ok(truncated) => full = truncated,
                Err(e) => {
                    tracing::warn!("log of {}: {e}", c.attempt_id);
                    full = true;
                }
            }
        }
        buf.clear();
        if closed {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    //! The runner loop against an in-process service, with a fake executor
    //! that interprets a few words (`echo`, `write`, `exit`, `wait`) and
    //! never starts a process.

    use super::*;
    use executor::ImageLock;
    use std::sync::atomic::AtomicBool;

    struct Fake {
        ready: AtomicBool,
        /// The image cache is held (by a pull or a load): checks wait.
        hold_images: AtomicBool,
        /// (the reference run, the job's pinned image) of every run.
        ran: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl Executor for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        async fn health(&self) -> Health {
            let ready = self.ready.load(Ordering::SeqCst);
            Health {
                ready,
                detail: (!ready).then(|| "fake executor switched off".to_string()),
            }
        }
        async fn resolve_image(
            &self,
            image: &str,
            _: &mpsc::Sender<Vec<u8>>,
        ) -> Result<(ImageCheck, ImageLock), String> {
            if self.hold_images.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok((
                ImageCheck::Present(format!("{image}#resolved")),
                ImageLock::default(),
            ))
        }
        async fn run(
            &self,
            job: JobRun<'_>,
            log: mpsc::Sender<Vec<u8>>,
            mut cancel: watch::Receiver<bool>,
        ) -> Outcome {
            self.ran
                .lock()
                .unwrap()
                .push((job.image.to_string(), job.job.image.clone()));
            for c in &job.job.commands {
                let words: Vec<&str> = c.split_whitespace().collect();
                match words.as_slice() {
                    ["echo", rest @ ..] => {
                        let _ = log.send(format!("{}\n", rest.join(" ")).into_bytes()).await;
                    }
                    ["write", path, content] => {
                        let p = job.work.join(path);
                        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                        std::fs::write(p, content).unwrap();
                    }
                    ["exit", code] => return Outcome::Exited(code.parse().unwrap()),
                    ["wait"] => loop {
                        if cancel.changed().await.is_err() || *cancel.borrow() {
                            return Outcome::Cancelled;
                        }
                    },
                    _ => return Outcome::Exited(127),
                }
            }
            Outcome::Exited(0)
        }
        async fn sweep(&self) {}
    }

    fn git(dir: &Path, args: &[&str]) {
        let home = dir.join("..").join("git-home");
        std::fs::create_dir_all(&home).unwrap();
        let global = home.join("gitconfig");
        std::fs::write(&global, "").unwrap();
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "credential.helper=", "-c", "core.askPass="])
            .args(args)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &global)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_runner_loop_reports_results_cancellation_and_an_unavailable_executor() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("branvil.toml"), "[ci]\nlease_secs = 3\n").unwrap();
        let config =
            crate::config::Config::load(None, Some(&data), Some("127.0.0.1:0"), None).unwrap();
        let layout = config.layout();
        layout.create().unwrap();
        let store = crate::store::Store::open(&layout.db(), 1 << 20, 2).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (owner, runner_token) = store
            .write_sync(|w| {
                use crate::domain::members::{NewMember, insert_member};
                use crate::model::MemberKind;
                for (name, kind) in [
                    ("alice", MemberKind::Owner),
                    ("runner1", MemberKind::Runner),
                ] {
                    insert_member(
                        w,
                        &NewMember {
                            name: name.into(),
                            display_name: None,
                            kind,
                        },
                    )?;
                }
                Ok((
                    crate::auth::issue_token(w, "alice", "t")?.1,
                    crate::auth::issue_token(w, "runner1", "t")?.1,
                ))
            })
            .unwrap();
        let key = layout.load_cursor_key().unwrap();
        let app = Arc::new(crate::app::App::new(
            config,
            url.clone(),
            store,
            key,
            url.clone(),
        ));
        tokio::spawn(axum::serve(listener, crate::http::router(app.clone())).into_future());
        let http = reqwest::Client::new();
        for (path, body) in [
            ("workspaces", serde_json::json!({"name": "acme"})),
            ("repos", serde_json::json!({"path": "acme/demo"})),
        ] {
            let resp = http
                .post(format!("{url}/api/v1/{path}"))
                .bearer_auth(&owner)
                .json(&body)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 201);
        }

        // A commit of main with a pipeline, pushed into the bare repository
        // directly (no hooks: the hooks directory is empty) and recorded.
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join(".branvil")).unwrap();
        std::fs::write(
            work.join(".branvil/ci.toml"),
            "[[jobs]]\nname = \"ok\"\nimage = \"alpine\"\ncommands = [\"echo hello from the job\", \"write out/r.txt report\"]\nartifacts = [{name = \"report\", paths = [\"out/\"]}]\n\n\
             [[jobs]]\nname = \"slow\"\nimage = \"alpine\"\ncommands = [\"wait\"]\n",
        )
        .unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["add", "."]);
        git(&work, &["commit", "-qm", "pipeline"]);
        let bare = layout.repo_dir("acme/demo");
        git(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
        let repo = app
            .store
            .read(|r| crate::domain::repo_by_path(r, "acme/demo"))
            .await
            .unwrap();
        crate::refs::record_refs(&app, &repo).await.unwrap();

        let runner = Runner {
            api: Api::new(&url, &runner_token),
            work_dir: dir.path().join("runner"),
            limits: ExecutorConfig::default(),
        };
        let fake = Fake {
            ready: AtomicBool::new(true),
            hold_images: AtomicBool::new(false),
            ran: Default::default(),
        };
        let job = |id: &str| {
            let (http, url, owner, id) = (http.clone(), url.clone(), owner.clone(), id.to_string());
            async move {
                http.get(format!("{url}/api/v1/repos/acme%2Fdemo/jobs/{id}"))
                    .bearer_auth(&owner)
                    .send()
                    .await
                    .unwrap()
                    .json::<serde_json::Value>()
                    .await
                    .unwrap()
            }
        };
        let sha = app.git.branch_tips(&bare).await.unwrap()["main"].clone();
        let checks = || {
            let (app, repo, sha) = (app.clone(), repo.clone(), sha.clone());
            async move { app.ci.head_checks(&repo, &sha).await.unwrap().checks }
        };

        // Job "ok": output streamed, artifact collected, success.
        assert!(runner.once(&fake).await.unwrap());
        let ok = checks().await.into_iter().find(|c| c.name == "ok").unwrap();
        assert_eq!(ok.conclusion.as_deref(), Some("success"), "{ok:?}");
        // The executor runs the resolved reference; the job (and with it the
        // cache key) keeps the pinned image.
        let (run, pinned) = fake.ran.lock().unwrap()[0].clone();
        assert_eq!(run, format!("{pinned}#resolved"));
        assert_eq!(ok.artifact_names, vec!["report".to_string()]);
        let id = ok.attempt_id.clone().unwrap();
        let (log, _) = app.ci.log_tail(&id, 1 << 20).await.unwrap();
        assert!(log.contains("hello from the job"), "{log}");
        assert!(
            log.contains("artifact report uploaded (1 file(s))"),
            "{log}"
        );
        assert!(
            !runner.work_dir.join("jobs").join(&id).exists(),
            "work directory removed"
        );

        // Job "slow": cancelled through the API while it runs, then again
        // while its check waits for the image cache.
        let cancel_slow = |runner: Runner, fake: Fake| {
            let (checks, http, url, owner) = (checks, http.clone(), url.clone(), owner.clone());
            async move {
                let running = tokio::spawn(async move {
                    let r = runner.once(&fake).await;
                    (runner, fake, r)
                });
                let mut slow_id = None;
                for _ in 0..50 {
                    if let Some(c) = checks()
                        .await
                        .into_iter()
                        .find(|c| c.name == "slow" && c.status == "in_progress")
                    {
                        slow_id = c.attempt_id;
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let slow_id = slow_id.expect("slow job running");
                let resp = http
                    .post(format!(
                        "{url}/api/v1/repos/acme%2Fdemo/jobs/{slow_id}/cancel"
                    ))
                    .bearer_auth(&owner)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(resp.status(), 200);
                let (runner, fake, r) = tokio::time::timeout(Duration::from_secs(20), running)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(r.unwrap());
                (runner, fake, slow_id)
            }
        };
        let (runner, fake, slow_id) = cancel_slow(runner, fake).await;
        let slow = job(&slow_id).await;
        assert_eq!(slow["conclusion"], "cancelled", "{slow}");
        fake.hold_images.store(true, Ordering::SeqCst);
        let resp = http
            .post(format!(
                "{url}/api/v1/repos/acme%2Fdemo/jobs/{slow_id}/retry"
            ))
            .bearer_auth(&owner)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let (runner, fake, waiting_id) = cancel_slow(runner, fake).await;
        let waiting = job(&waiting_id).await;
        assert_eq!(waiting["conclusion"], "cancelled", "{waiting}");
        assert_eq!(
            fake.ran.lock().unwrap().len(),
            2,
            "the waiting job never ran"
        );
        fake.hold_images.store(false, Ordering::SeqCst);

        // An unusable executor: the job concludes startup_failure with why.
        fake.ready.store(false, Ordering::SeqCst);
        let resp = http
            .post(format!("{url}/api/v1/repos/acme%2Fdemo/jobs/{id}/retry"))
            .bearer_auth(&owner)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert!(runner.once(&fake).await.unwrap());
        let ok = checks().await.into_iter().find(|c| c.name == "ok").unwrap();
        assert_eq!(ok.attempt, Some(2));
        assert_eq!(ok.conclusion.as_deref(), Some("startup_failure"));
        assert!(ok.details.as_deref().unwrap().contains("switched off"));
        let diag = app.ci.diagnostics().await;
        assert_eq!(diag["state"], "executor_unavailable", "{diag}");
        assert!(!runner.once(&fake).await.unwrap(), "nothing left");
    }
}
