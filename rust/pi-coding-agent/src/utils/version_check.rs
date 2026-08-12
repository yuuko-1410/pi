//! Version checking, port of
//! `packages/coding-agent/src/utils/version-check.ts`.
//!
//! `semver` is replaced by a built-in comparison over split numeric
//! components (semver valid()/compare() subset; pre-release identifiers
//! compare lexically, documented).

use crate::utils::basics::get_pi_user_agent;

const LATEST_VERSION_URL: &str = "https://pi.dev/api/latest-version";
const DEFAULT_VERSION_CHECK_TIMEOUT_MS: u64 = 10_000;

#[derive(Clone, Debug, PartialEq)]
pub struct LatestPiRelease {
    pub version: String,
    pub package_name: Option<String>,
    pub note: Option<String>,
}

/// Format a version check error with cause codes (JS
/// `formatVersionCheckError`); synchronous analog includes the message.
pub fn format_version_check_error(message: &str) -> String {
    message.to_string()
}

#[derive(Clone, Debug, PartialEq)]
struct Semver {
    major: i64,
    minor: i64,
    patch: i64,
    pre: Option<String>,
}

fn valid(version: &str) -> Option<Semver> {
    let version = version.trim();
    let (core, pre) = match version.find('-') {
        Some(index) => (&version[..index], Some(version[index + 1..].to_string())),
        None => (version, None),
    };
    let core = core.strip_prefix('v').unwrap_or(core);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() < 3 || parts.len() > 4 {
        return None;
    }
    let mut parsed = Vec::new();
    for part in &parts {
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        parsed.push(part.parse::<i64>().ok()?);
    }
    let patch = if parts.len() == 4 {
        parsed[3]
    } else {
        parsed[2]
    };
    let minor = if parts.len() == 4 { parsed[2] } else { parsed[1] };
    Some(Semver {
        major: parsed[0],
        minor,
        patch,
        pre,
    })
}

fn compare_semver(left: &Semver, right: &Semver) -> i64 {
    if left.major != right.major {
        return left.major - right.major;
    }
    if left.minor != right.minor {
        return left.minor - right.minor;
    }
    if left.patch != right.patch {
        return left.patch - right.patch;
    }
    match (&left.pre, &right.pre) {
        (None, None) => 0,
        (Some(_), None) => -1,
        (None, Some(_)) => 1,
        (Some(left), Some(right)) => left.cmp(right) as i64,
    }
}

/// Compare package versions; None when either is invalid (JS
/// `comparePackageVersions`).
pub fn compare_package_versions(left_version: &str, right_version: &str) -> Option<i64> {
    let left = valid(left_version)?;
    let right = valid(right_version)?;
    Some(compare_semver(&left, &right))
}

/// True when the candidate is newer (JS `isNewerPackageVersion`).
pub fn is_newer_package_version(candidate_version: &str, current_version: &str) -> bool {
    match compare_package_versions(candidate_version, current_version) {
        Some(comparison) => comparison > 0,
        None => candidate_version.trim() != current_version.trim(),
    }
}

/// Fetch the latest pi release from pi.dev (JS `getLatestPiRelease`).
pub fn get_latest_pi_release(current_version: &str) -> Result<Option<LatestPiRelease>, String> {
    if std::env::var("PI_OFFLINE").is_ok() {
        return Ok(None);
    }
    let response = crate::utils::management_http::fetch_with_retry(
        LATEST_VERSION_URL,
        Some(vec![
            ("User-Agent".to_string(), get_pi_user_agent(current_version)),
            ("accept".to_string(), "application/json".to_string()),
        ]),
        crate::utils::management_http::RetryOptions {
            max_retries: Some(0),
            retry_on_status: Some(true),
            timeout_ms: Some(DEFAULT_VERSION_CHECK_TIMEOUT_MS),
        },
    )?;
    if !(200..300).contains(&response.status) {
        return Ok(None);
    }
    let value: pi_protocol::cbor::Value =
        pi_ai::utils::json::parse_json_with_repair(&response.body).map_err(|error| format!("{error}"))?;
    let entries: Vec<(String, pi_protocol::cbor::Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let version = entries
        .iter()
        .find(|(key, _)| key == "version")
        .and_then(|(_, value)| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let Some(version) = version else {
        return Ok(None);
    };
    let package_name = entries
        .iter()
        .find(|(key, _)| key == "packageName")
        .and_then(|(_, value)| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let note = entries
        .iter()
        .find(|(key, _)| key == "note")
        .and_then(|(_, value)| value.as_str())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(Some(LatestPiRelease {
        version,
        package_name,
        note,
    }))
}

/// Fetch the latest pi version (JS `getLatestPiVersion`).
pub fn get_latest_pi_version(current_version: &str) -> Result<Option<String>, String> {
    Ok(get_latest_pi_release(current_version)?.map(|release| release.version))
}

/// Check for a newer pi version (JS `checkForNewPiVersion`); returns None
/// on any failure.
pub fn check_for_new_pi_version(current_version: &str) -> Option<LatestPiRelease> {
    if std::env::var("PI_SKIP_VERSION_CHECK").is_ok() {
        return None;
    }
    match get_latest_pi_release(current_version) {
        Ok(Some(release)) if is_newer_package_version(&release.version, current_version) => Some(release),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semver() {
        assert_eq!(compare_package_versions("1.2.3", "1.2.4"), Some(-1));
        assert_eq!(compare_package_versions("2.0.0", "1.9.9"), Some(1));
        assert_eq!(compare_package_versions("1.2.3", "1.2.3"), Some(0));
        assert_eq!(compare_package_versions("1.2.3", "not-a-version"), None);
        assert_eq!(compare_package_versions("1.2.3-beta", "1.2.3"), Some(-1));
        assert_eq!(compare_package_versions("v1.2.3", "1.2.3"), Some(0));
    }

    #[test]
    fn newer_version_detection() {
        assert!(is_newer_package_version("1.3.0", "1.2.0"));
        assert!(!is_newer_package_version("1.2.0", "1.3.0"));
        // Invalid versions compare by string inequality.
        assert!(is_newer_package_version("2.0", "1.9"));
    }

    #[test]
    fn offline_mode_skips_fetch() {
        std::env::set_var("PI_OFFLINE", "1");
        assert_eq!(get_latest_pi_release("1.0.0").unwrap(), None);
        std::env::remove_var("PI_OFFLINE");
    }
}
