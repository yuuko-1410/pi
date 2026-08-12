//! HTTP dispatcher configuration, port of .
//!
//! JS swaps the undici global dispatcher for timeout/proxy control; the Rust
//! HTTP layer (ureq) reads HTTP_PROXY/HTTPS_PROXY from the environment by
//! default and per-request timeouts are applied at call sites. This module
//! keeps the shared constants and the proxy-env application.

pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: u64 = 300_000;

pub const HTTP_IDLE_TIMEOUT_CHOICES: [(&str, u64); 5] = [
    ("30 sec", 30_000),
    ("1 min", 60_000),
    ("2 min", 120_000),
    ("5 min", 300_000),
    ("disabled", 0),
];

/// Parse a timeout value (number or "disabled"/empty string).
pub fn parse_http_idle_timeout_ms(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("disabled") {
        return Some(0);
    }
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<f64>() {
        Ok(number) if number.is_finite() && number >= 0.0 => Some(number.floor() as u64),
        _ => None,
    }
}

pub fn format_http_idle_timeout_ms(timeout_ms: u64) -> String {
    for (label, choice) in HTTP_IDLE_TIMEOUT_CHOICES {
        if choice == timeout_ms {
            return label.to_string();
        }
    }
    format!("{} sec", timeout_ms / 1000)
}

/// Apply HTTP_PROXY/HTTPS_PROXY env defaults, mirroring the JS ??= behavior.
pub fn apply_http_proxy_settings(http_proxy: Option<&str>) {
    let proxy = http_proxy.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    if let Some(proxy) = proxy {
        let mut env = std::env::var_os("HTTP_PROXY");
        if env.is_none() {
            std::env::set_var("HTTP_PROXY", &proxy);
        }
        env = std::env::var_os("HTTPS_PROXY");
        if env.is_none() {
            std::env::set_var("HTTPS_PROXY", &proxy);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_parsing() {
        assert_eq!(parse_http_idle_timeout_ms("disabled"), Some(0));
        assert_eq!(parse_http_idle_timeout_ms("300000"), Some(300000));
        assert_eq!(parse_http_idle_timeout_ms("300000.9"), Some(300000));
        assert_eq!(parse_http_idle_timeout_ms("  "), None);
        assert_eq!(parse_http_idle_timeout_ms("-5"), None);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_http_idle_timeout_ms(30_000), "30 sec");
        assert_eq!(format_http_idle_timeout_ms(0), "disabled");
        assert_eq!(format_http_idle_timeout_ms(90_000), "90 sec");
    }
}
