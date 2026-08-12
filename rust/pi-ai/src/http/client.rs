//! HTTP client for provider requests (ureq-based, synchronous).
//!
//! Mirrors the provider SDK behaviors pi relies on: JSON POST bodies,
//! streaming SSE responses, timeout/retry knobs, and error normalization
//! (status + truncated body). WebSocket transports are not yet ported.

use std::io::Read;
use std::time::Duration;

use pi_protocol::Value;

use crate::utils::error_body::{truncate_error_text, MAX_PROVIDER_ERROR_BODY_CHARS};
use crate::utils::provider_retry::ProviderError;

#[derive(Clone, Debug)]
pub struct HttpClient {
    agent: ureq::Agent,
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    pub fn new() -> Self {
        let agent = ureq::Agent::new();
        Self { agent }
    }

    fn request(&self, method: &str, url: &str) -> ureq::Request {
        let request = match method {
            "POST" => self.agent.post(url),
            "GET" => self.agent.get(url),
            "DELETE" => self.agent.delete(url),
            _ => panic!("unsupported method {method}"),
        };
        request
    }

    fn apply_headers(request: ureq::Request, headers: &[(String, String)]) -> ureq::Request {
        let mut request = request;
        for (name, value) in headers {
            request = request.set(name, value);
        }
        request
    }

    /// Sends a GET request and returns the streaming response.
    pub fn get(
        &self,
        url: &str,
        headers: &[(String, String)],
        timeout_ms: Option<u64>,
    ) -> Result<HttpResponse, ProviderError> {
        let mut request = self.request("GET", url);
        request = Self::apply_headers(request, headers);
        if let Some(timeout_ms) = timeout_ms {
            request = request.timeout(Duration::from_millis(timeout_ms));
        }
        let response = request.call().map_err(to_provider_error)?;
        let status = response.status();
        let headers = response.headers_names()
            .into_iter()
            .filter_map(|name| {
                let value = response.header(&name)?;
                Some((name.to_string(), value.to_string()))
            })
            .collect();
        let reader: Box<dyn Read + Send> = Box::new(response.into_reader());
        Ok(HttpResponse {
            status,
            headers,
            reader,
        })
    }

    /// Sends a JSON body and returns the streaming response (body reader for
    /// SSE consumption). Non-2xx responses raise a `ProviderError` with the
    /// truncated body.
    pub fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &Value,
        timeout_ms: Option<u64>,
    ) -> Result<HttpResponse, ProviderError> {
        let body_text = crate::utils::json::json_stringify(body);
        let mut request = self.request("POST", url);
        request = Self::apply_headers(request, headers);
        request = request.set("Content-Type", "application/json");
        if let Some(timeout_ms) = timeout_ms {
            request = request.timeout(Duration::from_millis(timeout_ms));
        }

        let response = request
            .send_string(&body_text)
            .map_err(|error| to_provider_error(error))?;
        let status = response.status();
        let response_headers = response_headers(&response);
        if !(200..300).contains(&status) {
            let body = read_limited(response.into_reader());
            return Err(ProviderError::new(Some(status), response_headers, body));
        }
        Ok(HttpResponse {
            status,
            headers: response_headers,
            reader: Box::new(response.into_reader()),
        })
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub reader: Box<dyn Read + Send>,
}

fn response_headers(response: &ureq::Response) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for name in response.headers_names() {
        if let Some(value) = response.header(&name) {
            headers.push((name, value.to_string()));
        }
    }
    headers
}

fn read_limited(mut reader: impl Read) -> String {
    let mut buffer = Vec::new();
    reader
        .by_ref()
        .take(MAX_PROVIDER_ERROR_BODY_CHARS as u64 + 64)
        .read_to_end(&mut buffer)
        .ok();
    let text = String::from_utf8_lossy(&buffer).to_string();
    truncate_error_text(&text, MAX_PROVIDER_ERROR_BODY_CHARS)
}

fn to_provider_error(error: ureq::Error) -> ProviderError {
    match error {
        ureq::Error::Status(status, response) => {
            let headers = response_headers(&response);
            let body = read_limited(response.into_reader());
            ProviderError::new(Some(status), headers, body)
        }
        ureq::Error::Transport(transport) => {
            let message = transport.to_string();
            ProviderError::new(None, Vec::new(), message)
        }
    }
}

/// Convenience: reads an SSE stream from an HTTP response body, invoking the
/// callback per parsed event.
pub fn read_sse_stream(
    reader: impl Read,
    mut on_event: impl FnMut(&crate::http::sse::SseEvent),
) {
    let mut parser = crate::http::sse::SseParser::new();
    let mut reader = reader;
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                for event in parser.push(&buffer[..n]) {
                    on_event(&event);
                }
            }
            Err(_) => break,
        }
    }
    parser.end();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sse_from_a_byte_source() {
        let wire = b"event: response.created\ndata: {\"id\":\"1\"}\n\nevent: x\ndata: y\n\n";
        let mut events = Vec::new();
        read_sse_stream(&wire[..], |event| events.push(event.clone()));
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("response.created"));
        assert_eq!(events[1].data, "y");
    }
}
