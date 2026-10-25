//! The owner's webhook, usable without opening the database or the service.

use crate::config::Layout;

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("branvil/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("an HTTP client")
}

/// True only after delivery; an unset webhook is a successful no-op.
/// Neither errors nor response bodies may expose the credential in the URL.
pub async fn send(client: &reqwest::Client, layout: &Layout, text: &str) -> Result<bool, String> {
    let url = match std::fs::read_to_string(layout.webhook()) {
        Ok(url) => url,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("cannot read the webhook credential".into()),
    };
    let response = client
        .post(url.trim())
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .map_err(|_| "the webhook request failed".to_string())?;
    if !response.status().is_success() {
        return Err(format!("the webhook answered {}", response.status()));
    }
    Ok(true)
}
