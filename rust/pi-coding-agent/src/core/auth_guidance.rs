//! Auth guidance messages, port of `core/auth-guidance.ts`.

const UNKNOWN_PROVIDER: &str = "unknown";

/// docs path under the package dir (JS getDocsPath).
pub fn get_docs_path() -> String {
    crate::config::get_package_dir() + "/docs"
}

pub fn get_provider_login_help() -> String {
    [
        "Use /login to log into a provider via OAuth or API key. See:".to_string(),
        format!("  {}", crate::core::session_paths::join(&get_docs_path(), "providers.md")),
        format!("  {}", crate::core::session_paths::join(&get_docs_path(), "models.md")),
    ]
    .join("\n")
}

pub fn format_no_models_available_message() -> String {
    format!("No models available. {}", get_provider_login_help())
}

pub fn format_no_model_selected_message() -> String {
    format!("No model selected.\n\n{}\n\nThen use /model to select a model.", get_provider_login_help())
}

pub fn format_no_api_key_found_message(provider: &str) -> String {
    let provider_display = if provider == UNKNOWN_PROVIDER {
        "the selected model"
    } else {
        provider
    };
    format!("No API key found for {provider_display}.\n\n{}", get_provider_login_help())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_mention_login() {
        assert!(format_no_models_available_message().contains("Use /login"));
        assert!(format_no_model_selected_message().contains("/model"));
        assert!(format_no_api_key_found_message("openai").contains("No API key found for openai"));
        assert!(format_no_api_key_found_message("unknown").contains("the selected model"));
    }
}
