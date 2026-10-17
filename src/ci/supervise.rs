//! `[ci] runner = "auto"`: `branvil serve` starts one `branvil runner` child
//! (the same binary, a separate process) and restarts it when it exits. The
//! child authenticates as the runner member `branvil-runner` with a token
//! minted at each start (the previous automatic token is revoked), passed
//! through the environment, never the command line.

use std::time::Duration;

use crate::app::Shared;
use crate::domain::members::{NewMember, insert_member};
use crate::error::{AppError, AppResult};
use crate::model::{Member, MemberKind};
use crate::store::{self, Rd};

pub const AUTO_RUNNER: &str = "branvil-runner";

async fn mint_token(app: &Shared) -> AppResult<String> {
    app.store
        .write(|w| {
            match w.get::<Member>(store::MEMBERS, &Member::key(AUTO_RUNNER))? {
                Some(m) if m.kind != MemberKind::Runner => {
                    return Err(AppError::conflict(format!(
                        "The member {AUTO_RUNNER} exists and is not a runner; the automatic runner cannot use it."
                    )));
                }
                Some(_) => {}
                None => {
                    insert_member(
                        w,
                        &NewMember {
                            name: AUTO_RUNNER.into(),
                            display_name: Some("Branvil runner (automatic)".into()),
                            kind: MemberKind::Runner,
                        },
                    )?;
                }
            }
            Ok(crate::auth::replace_token(w, AUTO_RUNNER, crate::auth::AUTO_LABEL)?.1)
        })
        .await
}

/// Supervises the runner child for the lifetime of the service.
pub fn start(app: Shared, url: String) {
    tokio::spawn(async move {
        let mut backoff = 2;
        loop {
            let token = match mint_token(&app).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("CI: cannot start the automatic runner: {e}");
                    return;
                }
            };
            let exe = match std::env::current_exe() {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("CI: cannot find the branvil binary: {e}");
                    return;
                }
            };
            let mut cmd = tokio::process::Command::new(exe);
            cmd.arg("runner")
                .arg("--url")
                .arg(&url)
                .arg("--data-dir")
                .arg(&app.config.data_dir)
                .env("BRANVIL_TOKEN", token)
                .env_remove("BRANVIL_FAILPOINT")
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
            let started = std::time::Instant::now();
            match cmd.spawn() {
                Ok(mut child) => {
                    tracing::info!("CI: started the automatic runner (pid {:?})", child.id());
                    let status = child.wait().await;
                    tracing::warn!("CI: the automatic runner exited ({status:?}); restarting");
                }
                Err(e) => tracing::error!("CI: cannot start the automatic runner: {e}"),
            }
            if started.elapsed() > Duration::from_secs(300) {
                backoff = 2;
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(120);
        }
    });
}
