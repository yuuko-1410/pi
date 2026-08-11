//! Provider-request retry with server-requested delays, port of
//! `packages/ai/src/utils/provider-retry.ts`.

use super::abort::{abortable_sleep, CancellationToken};

const DEFAULT_MAX_RETRY_DELAY_MS: f64 = 60_000.0;

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderError {
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    pub message: String,
}

impl ProviderError {
    pub fn new(status: Option<u16>, headers: Vec<(String, String)>, message: impl Into<String>) -> Self {
        Self {
            status,
            headers,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderError {}

/// Header lookup mirroring `Headers.get` (case-insensitive).
fn header_get<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Mirrors the pinned OpenAI/Anthropic SDK retry policy.
fn is_retryable_provider_error(error: &ProviderError) -> bool {
    if let Some(should_retry) = header_get(&error.headers, "x-should-retry") {
        if should_retry == "true" {
            return true;
        }
        if should_retry == "false" {
            return false;
        }
    }

    match error.status {
        None => true,
        Some(status) => {
            status == 408 || status == 409 || status == 429 || status >= 500
        }
    }
}

fn validate_server_retry_delay_ms(
    delay_ms: f64,
    max_retry_delay_ms: Option<f64>,
    provider_error_message: &str,
) -> Result<f64, String> {
    let max_delay_ms = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max_delay_ms > 0.0 && delay_ms > max_delay_ms {
        return Err(format!(
            "Server requested {}s retry delay (max: {}s). {provider_error_message}",
            (delay_ms / 1000.0).ceil(),
            (max_delay_ms / 1000.0).ceil()
        ));
    }
    Ok(delay_ms)
}

/// Parses an IMF-fixdate HTTP date (`"Wed, 21 Oct 2015 07:28:00 GMT"`) as
/// milliseconds since the Unix epoch; `None` when unparseable.
fn parse_http_date(value: &str) -> Option<f64> {
    // "Wed, 21 Oct 2015 07:28:00 GMT"
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 6 || !parts[5].eq_ignore_ascii_case("gmt") {
        return None;
    }
    let day: u32 = parts[1].parse().ok()?;
    let month = match parts[2].to_ascii_lowercase().as_str() {
        "jan" => 1u32,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    let year: i64 = parts[3].parse().ok()?;
    let time: Vec<&str> = parts[4].split(':').collect();
    if time.len() != 3 {
        return None;
    }
    let hour: u32 = time[0].parse().ok()?;
    let minute: u32 = time[1].parse().ok()?;
    let second: u32 = time[2].parse().ok()?;

    // Days from civil algorithm (Howard Hinnant).
    fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) as i64 + 2) / 5 + d as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }

    let days = days_from_civil(year, month, day);
    let seconds = days * 86400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    Some(seconds as f64 * 1000.0)
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as f64
}

/// Entropy for jitter; falls back to a fixed value when unavailable.
fn random_fraction() -> f64 {
    use std::io::Read;
    let mut bytes = [0u8; 8];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if ok {
        let value = u64::from_le_bytes(bytes) as f64 / (u64::MAX as f64);
        value
    } else {
        0.5
    }
}

fn get_retry_delay_ms(
    error: &ProviderError,
    retry_index: u64,
    max_retry_delay_ms: Option<f64>,
) -> Result<f64, String> {
    if let Some(retry_after_ms) = header_get(&error.headers, "retry-after-ms") {
        if let Ok(value) = retry_after_ms.parse::<f64>() {
            return validate_server_retry_delay_ms(value, max_retry_delay_ms, &error.message);
        }
    }

    if let Some(retry_after) = header_get(&error.headers, "retry-after") {
        let delay_ms = match retry_after.parse::<f64>() {
            Ok(seconds) => seconds * 1000.0,
            Err(_) => match parse_http_date(retry_after) {
                Some(date_ms) => date_ms - now_ms(),
                None => f64::NAN,
            },
        };
        return validate_server_retry_delay_ms(delay_ms, max_retry_delay_ms, &error.message);
    }

    let exponential_delay = (0.5 * 2f64.powi(retry_index as i32)).min(8.0) * 1000.0;
    Ok(exponential_delay * (1.0 - random_fraction() * 0.25))
}

#[derive(Clone, Debug, Default)]
pub struct ProviderRetryOptions {
    pub max_retries: Option<u64>,
    pub max_retry_delay_ms: Option<f64>,
    pub token: Option<CancellationToken>,
}

/// Reproduce the retry behavior used by the OpenAI and Anthropic SDKs with
/// interruptible backoff. Provider-requested delays above `maxRetryDelayMs`
/// fail immediately (60 seconds by default); set it to zero to disable the
/// limit.
pub fn retry_provider_request<T, F>(
    mut request: F,
    options: ProviderRetryOptions,
) -> Result<T, ProviderRetryFailure>
where
    F: FnMut() -> Result<T, ProviderError>,
{
    let max_retries = options.max_retries.unwrap_or(0);
    let mut retries_remaining = max_retries;

    loop {
        match request() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if let Some(token) = &options.token {
                    if token.is_aborted() {
                        return Err(ProviderRetryFailure::Aborted);
                    }
                }
                if retries_remaining <= 0 || !is_retryable_provider_error(&error) {
                    return Err(ProviderRetryFailure::Error(error));
                }

                let retry_index = max_retries - retries_remaining;
                retries_remaining -= 1;
                match get_retry_delay_ms(&error, retry_index, options.max_retry_delay_ms) {
                    Ok(delay_ms) => {
                        if abortable_sleep(delay_ms as u64, options.token.as_ref()).is_err() {
                            return Err(ProviderRetryFailure::Aborted);
                        }
                    }
                    Err(message) => {
                        return Err(ProviderRetryFailure::Error(ProviderError::new(
                            error.status,
                            error.headers.clone(),
                            message,
                        )));
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub enum ProviderRetryFailure {
    Error(ProviderError),
    Aborted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_retryable_provider_errors() {
        let headers = |pairs: Vec<(&str, &str)>| -> Vec<(String, String)> {
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let base = || ProviderError::new(Some(429), vec![], "boom");

        assert!(is_retryable_provider_error(&base()));
        assert!(is_retryable_provider_error(&ProviderError::new(Some(500), vec![], "boom")));
        assert!(is_retryable_provider_error(&ProviderError::new(Some(503), vec![], "boom")));
        assert!(!is_retryable_provider_error(&ProviderError::new(Some(400), vec![], "boom")));
        assert!(!is_retryable_provider_error(&ProviderError::new(Some(403), vec![], "boom")));
        assert!(is_retryable_provider_error(&ProviderError::new(None, vec![], "boom")));
        // x-should-retry overrides the status.
        assert!(is_retryable_provider_error(&ProviderError::new(
            Some(400),
            headers(vec![("x-should-retry", "true")]),
            "boom"
        )));
        assert!(!is_retryable_provider_error(&ProviderError::new(
            Some(503),
            headers(vec![("x-should-retry", "false")]),
            "boom"
        )));
    }

    #[test]
    fn parses_http_dates() {
        // 2015-10-21T07:28:00Z = 1445426880000 ms
        let parsed = parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT");
        assert_eq!(parsed, Some(1445412480000.0));
        assert_eq!(parse_http_date("not a date"), None);
    }

    #[test]
    fn validates_server_retry_delays() {
        assert!(validate_server_retry_delay_ms(30_000.0, Some(60_000.0), "msg").is_ok());
        let error = validate_server_retry_delay_ms(120_000.0, Some(60_000.0), "provider said no").unwrap_err();
        assert!(error.contains("120s retry delay"), "{error}");
        assert!(error.contains("provider said no"), "{error}");
        // Zero disables the cap.
        assert!(validate_server_retry_delay_ms(999_999.0, Some(0.0), "msg").is_ok());
    }

    #[test]
    fn retries_transient_errors() {
        let mut calls = 0;
        let result = retry_provider_request(
            || {
                calls += 1;
                if calls < 3 {
                    Err(ProviderError::new(Some(429), vec![("retry-after-ms".to_string(), "1".to_string())], "slow down"))
                } else {
                    Ok(42)
                }
            },
            ProviderRetryOptions {
                max_retries: Some(3),
                ..Default::default()
            },
        );
        assert!(matches!(result, Ok(42)));
        assert_eq!(calls, 3);
    }

    #[test]
    fn non_retryable_errors_fail_fast() {
        let mut calls = 0;
        let result: Result<(), _> = retry_provider_request(
            || {
                calls += 1;
                Err(ProviderError::new(Some(400), vec![], "bad request"))
            },
            ProviderRetryOptions {
                max_retries: Some(3),
                ..Default::default()
            },
        );
        assert!(matches!(result, Err(ProviderRetryFailure::Error(_))));
        assert_eq!(calls, 1);
    }
}
