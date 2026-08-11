//! Port of `packages/ai/src/utils/headers.ts` (the provider-headers part;
//! `headersToRecord` operates on the WHATWG `Headers` object of the HTTP
//! layer and is ported there).

use crate::types::ProviderHeaders;

/// Mirrors `providerHeadersToRecord`: drops suppressed (`None`) values and
/// returns `None` when nothing remains.
pub fn provider_headers_to_record(headers: Option<&ProviderHeaders>) -> Option<Vec<(String, String)>> {
    let headers = headers?;
    let result: Vec<(String, String)> = headers
        .iter()
        .filter_map(|(key, value)| value.clone().map(|value| (key.clone(), value)))
        .collect();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_suppressed_headers_and_empty_results() {
        let headers = vec![
            ("keep".to_string(), Some("v".to_string())),
            ("drop".to_string(), None),
        ];
        assert_eq!(
            provider_headers_to_record(Some(&headers)),
            Some(vec![("keep".to_string(), "v".to_string())])
        );
        assert_eq!(
            provider_headers_to_record(Some(&vec![("drop".to_string(), None)])),
            None
        );
        assert_eq!(provider_headers_to_record(None), None);
    }
}
