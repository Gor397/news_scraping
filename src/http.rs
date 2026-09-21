//! Thin HTTP layer: one client, retries with backoff, charset-aware decoding.

use anyhow::{anyhow, Result};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use std::time::Duration;

pub struct Fetched {
    /// URL after redirects - used as the base for resolving relative links.
    pub url: String,
    pub status: u16,
    pub body: String,
}

pub fn build_client(user_agent: &str, timeout: Duration) -> Result<Client> {
    Client::builder()
        .user_agent(user_agent)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        // Keep many keep-alive connections open so concurrent fetches reuse
        // them instead of re-running the TCP+TLS handshake per request.
        .pool_max_idle_per_host(32)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_nodelay(true)
        // Let the HTTP/2 flow-control window grow so a slow page does not
        // stall other requests multiplexed on the same connection.
        .http2_adaptive_window(true)
        .http2_initial_stream_window_size(512 * 1024)
        .cookie_store(true)
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()
        .map_err(|e| anyhow!("cannot build HTTP client: {e}"))
}

pub async fn fetch(client: &Client, url: &str, retries: usize) -> Result<Fetched> {
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        let result = client
            .get(url)
            .header(
                ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(ACCEPT_LANGUAGE, "en,ru;q=0.8,fa;q=0.7,*;q=0.5")
            .send()
            .await;

        match result {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let content_type = resp
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let final_url = resp.url().to_string();
                    let bytes = resp
                        .bytes()
                        .await
                        .map_err(|e| anyhow!("body read failed: {e}"))?;
                    return Ok(Fetched {
                        url: final_url,
                        status: status.as_u16(),
                        body: decode_body(&bytes, &content_type),
                    });
                }

                let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                if retryable && attempt <= retries {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                return Err(anyhow!("HTTP {}", status.as_u16()));
            }
            Err(e) => {
                if attempt <= retries {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                return Err(anyhow!("{}", classify(&e)));
            }
        }
    }
}

fn backoff(attempt: usize) -> Duration {
    // 700ms, 1.4s, 2.8s, capped.
    let ms = 700u64.saturating_mul(1u64 << (attempt.min(5) as u32 - 1));
    Duration::from_millis(ms.min(8_000))
}

/// Pace requests so roughly one *starts* every `interval`, without the
/// cumulative drift of the old `sleep(i * delay)` stagger.
#[derive(Debug)]
pub struct Pacer {
    interval: Duration,
    next_slot: tokio::time::Instant,
}

impl Pacer {
    pub fn new(delay_ms: u64) -> Self {
        Self {
            interval: Duration::from_millis(delay_ms),
            next_slot: tokio::time::Instant::now(),
        }
    }

    /// Reserve the next start slot. The caller sleeps until it; several
    /// requests can hold slots at once, so the waits overlap downloads.
    pub fn reserve(&mut self) -> tokio::time::Instant {
        let slot = self.next_slot;
        let now = tokio::time::Instant::now();
        if self.next_slot < now {
            self.next_slot = now;
        }
        self.next_slot += self.interval;
        slot
    }
}

/// Short, groupable error labels so the summary can count them.
fn classify(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timeout".to_string()
    } else if e.is_connect() {
        "connection failed".to_string()
    } else if e.is_redirect() {
        "too many redirects".to_string()
    } else if e.is_decode() {
        "decode error".to_string()
    } else {
        let s = e.to_string();
        let s = s.split(':').next().unwrap_or("request error").trim();
        format!("request error ({s})")
    }
}

/// Decide the charset from the Content-Type header, then from a `<meta>` tag,
/// then assume UTF-8. Several of these sites are windows-1251 with the charset
/// declared only in the document.
fn decode_body(bytes: &[u8], content_type: &str) -> String {
    let label = charset_from_content_type(content_type).or_else(|| charset_from_meta(bytes));

    let encoding = label
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);

    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

fn charset_from_content_type(ct: &str) -> Option<String> {
    for part in ct.split(';') {
        let part = part.trim().to_ascii_lowercase();
        if let Some(rest) = part.strip_prefix("charset=") {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn charset_from_meta(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    let idx = head.find("charset")?;
    let tail = &head[idx + "charset".len()..];
    let tail = tail.trim_start().strip_prefix('=')?.trim_start();
    let value: String = tail
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}
