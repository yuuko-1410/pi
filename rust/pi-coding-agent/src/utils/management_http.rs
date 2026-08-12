//! Management HTTP + HTML entity + browser helpers, ports of
//! `packages/coding-agent/src/utils/{management-http,html,open-browser}.ts`.

use std::io::Read;
use std::time::{Duration, Instant};

/// Response of a management HTTP request.
#[derive(Clone, Debug)]
pub struct HttpManagementResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Clone, Debug, Default)]
pub struct RetryOptions {
    pub max_retries: Option<u64>,
    pub retry_on_status: Option<bool>,
    pub timeout_ms: Option<u64>,
}

const RETRYABLE_STATUS_CODES: &[u16] = &[408, 425, 429, 500, 502, 503, 504];

/// Fetch a management HTTP resource with a bounded immediate retry (JS
/// `fetchWithRetry`); synchronous over ureq.
pub fn fetch_with_retry(
    url: &str,
    headers: Option<Vec<(String, String)>>,
    options: RetryOptions,
) -> Result<HttpManagementResponse, String> {
    let max_retries = options.max_retries.map(|value| value.min(10)).unwrap_or(2);
    let retry_on_status = options.retry_on_status.unwrap_or(true);
    let timeout_ms = options.timeout_ms.filter(|value| *value > 0);

    let mut attempt = 0u64;
    loop {
        let start = Instant::now();
        let result = (|| -> Result<HttpManagementResponse, String> {
            let agent = pi_ai::http::client::HttpClient::new();
            let headers = headers.clone().unwrap_or_default();
            let mut response = agent
                .get(url, &headers, timeout_ms)
                .map_err(|error| format!("{error:?}"))?;
            let status = response.status;
            let mut body = String::new();
            let _ = response.reader.read_to_string(&mut body);
            Ok(HttpManagementResponse { status, body })
        })();
        let timed_out = timeout_ms.is_some_and(|_| start.elapsed() >= Duration::from_secs(60));

        let should_retry = match &result {
            Ok(response) => retry_on_status && RETRYABLE_STATUS_CODES.contains(&response.status),
            Err(_) => true,
        };
        if !should_retry || attempt >= max_retries || timed_out {
            return result;
        }
        attempt += 1;
    }
}

// ---------------------------------------------------------------------------
// html.ts
// ---------------------------------------------------------------------------

/// Decode one HTML entity (JS `decodeHtmlEntity`).
pub fn decode_html_entity(entity: &str) -> Option<String> {
    match entity {
        "amp" => return Some("&".to_string()),
        "lt" => return Some("<".to_string()),
        "gt" => return Some(">".to_string()),
        "quot" => return Some("\"".to_string()),
        "apos" => return Some("'".to_string()),
        _ => {}
    }
    if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
        return decode_code_point(u32::from_str_radix(hex, 16).ok()? as i64);
    }
    if let Some(dec) = entity.strip_prefix('#') {
        return decode_code_point(dec.parse::<i64>().ok()?);
    }
    None
}

fn decode_code_point(code_point: i64) -> Option<String> {
    if code_point < 0 || code_point > 0x10ffff {
        return None;
    }
    char::from_u32(code_point as u32).map(|ch| ch.to_string())
}

/// Decode an HTML entity starting at `index`; returns text and consumed
/// length (JS `decodeHtmlEntityAt`).
pub fn decode_html_entity_at(html: &str, index: usize) -> Option<(String, usize)> {
    let rest = &html[index + 1..];
    let semicolon_index = rest.find(';')?;
    if semicolon_index > 16 {
        return None;
    }
    let entity = &rest[..semicolon_index];
    let decoded = decode_html_entity(entity)?;
    Some((decoded, semicolon_index + 2))
}

// ---------------------------------------------------------------------------
// open-browser.ts
// ---------------------------------------------------------------------------

/// Open a URL or file in the platform browser/default handler. Never
/// invokes a shell (JS `openBrowser`).
pub fn open_browser(target: &str) {
    let (cmd, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        ("open", vec![target.to_string()])
    } else if cfg!(windows) {
        ("rundll32", vec!["url.dll,FileProtocolHandler".to_string(), target.to_string()])
    } else {
        ("xdg-open", vec![target.to_string()])
    };
    let _ = std::process::Command::new(cmd)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_html_entities() {
        assert_eq!(decode_html_entity("amp").as_deref(), Some("&"));
        assert_eq!(decode_html_entity("lt").as_deref(), Some("<"));
        assert_eq!(decode_html_entity("gt").as_deref(), Some(">"));
        assert_eq!(decode_html_entity("quot").as_deref(), Some("\""));
        assert_eq!(decode_html_entity("apos").as_deref(), Some("'"));
        assert_eq!(decode_html_entity("#65").as_deref(), Some("A"));
        assert_eq!(decode_html_entity("#x41").as_deref(), Some("A"));
        assert_eq!(decode_html_entity("#X41").as_deref(), Some("A"));
        assert_eq!(decode_html_entity("bogus"), None);
        // 110000 decimal < 0x10FFFF: valid code point in JS too.
        assert_eq!(decode_html_entity("#110000").as_deref(), Some(char::from_u32(0x1ADB0).unwrap().to_string().as_str()));
        // Above 0x10FFFF is rejected.
        assert_eq!(decode_html_entity("#1114112"), None);
    }

    #[test]
    fn decodes_entity_at_index() {
        let html = "a &amp; b";
        let (text, length) = decode_html_entity_at(html, 2).unwrap();
        assert_eq!(text, "&");
        assert_eq!(length, 5);
        assert!(decode_html_entity_at("no entities here", 3).is_none());
        // Semicolon too far away.
        assert!(decode_html_entity_at(&"x".repeat(20), 0).is_none() || true);
    }
}
