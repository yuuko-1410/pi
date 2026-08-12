//! Provider attribution headers, port of `core/provider-attribution.ts`.

use pi_ai::types::Model;

use super::settings_manager::SettingsManager;
use super::telemetry::is_install_telemetry_enabled;

const OPENROUTER_HOST: &str = "openrouter.ai";
const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const CLOUDFLARE_API_HOST: &str = "api.cloudflare.com";
const CLOUDFLARE_AI_GATEWAY_HOST: &str = "gateway.ai.cloudflare.com";
const OPENCODE_HOST: &str = "opencode.ai";

fn matches_host(base_url: &str, expected_host: &str) -> bool {
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"));
    let Some(rest) = rest else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or(rest);
    host == expected_host
}

fn is_openrouter_model(model: &Model) -> bool {
    model.provider == "openrouter" || model.base_url.contains(OPENROUTER_HOST)
}

fn is_nvidia_nim_model(model: &Model) -> bool {
    model.provider == "nvidia" || matches_host(&model.base_url, NVIDIA_NIM_HOST)
}

fn is_cloudflare_model(model: &Model) -> bool {
    model.provider == "cloudflare-workers-ai"
        || model.provider == "cloudflare-ai-gateway"
        || matches_host(&model.base_url, CLOUDFLARE_API_HOST)
        || matches_host(&model.base_url, CLOUDFLARE_AI_GATEWAY_HOST)
}

fn get_default_attribution_headers(
    model: &Model,
    settings_manager: &SettingsManager,
) -> Option<Vec<(String, String)>> {
    if !is_install_telemetry_enabled(settings_manager, None) {
        return None;
    }
    if is_openrouter_model(model) {
        return Some(vec![
            ("HTTP-Referer".into(), "https://pi.dev".into()),
            ("X-OpenRouter-Title".into(), "pi".into()),
            ("X-OpenRouter-Categories".into(), "cli-agent".into()),
        ]);
    }
    if is_nvidia_nim_model(model) {
        return Some(vec![("X-BILLING-INVOKE-ORIGIN".into(), "Pi".into())]);
    }
    if is_cloudflare_model(model) {
        return Some(vec![("User-Agent".into(), "pi-coding-agent".into())]);
    }
    None
}

fn get_session_headers(model: &Model, session_id: Option<&str>) -> Option<Vec<(String, String)>> {
    let session_id = session_id?;
    if model.provider != "opencode"
        && model.provider != "opencode-go"
        && !matches_host(&model.base_url, OPENCODE_HOST)
    {
        return None;
    }
    Some(vec![
        ("x-opencode-session".into(), session_id.to_string()),
        ("x-opencode-client".into(), "pi".into()),
    ])
}

pub type ProviderHeaders = Vec<(String, Option<String>)>;

/// Merge attribution headers in order: session, default attribution, sources.
pub fn merge_provider_attribution_headers(
    model: &Model,
    settings_manager: &SettingsManager,
    session_id: Option<&str>,
    header_sources: &[Option<&ProviderHeaders>],
) -> Option<ProviderHeaders> {
    let mut merged: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push = |key: String, value: String| {
        if seen.insert(key.clone()) {
            merged.push((key, value));
        } else if let Some(entry) = merged.iter_mut().find(|(k, _)| *k == key) {
            entry.1 = value;
        }
    };

    if let Some(headers) = get_session_headers(model, session_id) {
        for (key, value) in headers {
            push(key, value);
        }
    }
    if let Some(headers) = get_default_attribution_headers(model, settings_manager) {
        for (key, value) in headers {
            push(key, value);
        }
    }
    for headers in header_sources {
        if let Some(headers) = headers {
            for (key, value) in headers.iter() {
                if let Some(value) = value {
                    push(key.clone(), value.clone());
                }
            }
        }
    }

    if merged.is_empty() {
        None
    } else {
        Some(merged.into_iter().map(|(key, value)| (key, Some(value))).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, base_url: &str) -> Model {
        let mut model = Model {
            id: "m".into(),
            name: "m".into(),
            api: "openai".into(),
            provider: provider.into(),
            base_url: base_url.into(),
            reasoning: false,
            thinking_level_map: None,
            input: Vec::new(),
            cost: pi_ai::types::ModelCost {
                rates: pi_ai::types::ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: 0.0,
            max_tokens: 0.0,
            sampling_params: None,
            headers: None,
            compat: None,
        };
        let _ = &mut model;
        model
    }

    fn unique_agent_dir(name: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-attrib-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn openrouter_attribution_when_telemetry_on() {
        let mut settings = super::super::settings_manager::SettingsManager::create("/tmp", &unique_agent_dir("on"), true);
        settings.set_enable_install_telemetry(true);
        let model = model("openrouter", "https://openrouter.ai/api/v1");
        let merged = merge_provider_attribution_headers(&model, &settings, None, &[]).unwrap();
        let keys: Vec<&str> = merged.iter().map(|(key, _)| key.as_str()).collect();
        assert!(keys.contains(&"HTTP-Referer"));
        assert!(keys.contains(&"X-OpenRouter-Title"));
        assert!(keys.contains(&"X-OpenRouter-Categories"));
    }

    #[test]
    fn no_headers_when_telemetry_off() {
        let mut settings = super::super::settings_manager::SettingsManager::create("/tmp", &unique_agent_dir("off"), true);
        settings.set_enable_install_telemetry(false);
        let model = model("openrouter", "https://openrouter.ai/api/v1");
        assert!(merge_provider_attribution_headers(&model, &settings, None, &[]).is_none());
    }

    #[test]
    fn opencode_session_header() {
        let mut settings = super::super::settings_manager::SettingsManager::create("/tmp", &unique_agent_dir("oc"), true);
        settings.set_enable_install_telemetry(true);
        let model = model("opencode", "https://opencode.ai");
        let merged = merge_provider_attribution_headers(&model, &settings, Some("sess-1"), &[]).unwrap();
        assert!(merged.iter().any(|(key, value)| key == "x-opencode-session" && value.as_deref() == Some("sess-1")));
    }
}
