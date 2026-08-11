//! Provider env resolution, port of `packages/ai/src/utils/provider-env.ts`.
//!
//! The JS implementation also reads `/proc/self/environ` for Bun sandboxes;
//! Rust's `std::env::var` always works, so that fallback is unnecessary.

use crate::types::ProviderEnv;

/// Resolve a provider env value from scoped overrides, then the process env.
pub fn get_provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(env) = env {
        if let Some((_, value)) = env.iter().find(|(key, _)| key == name) {
            return Some(value.clone());
        }
    }
    std::env::var(name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_scoped_overrides() {
        let env = vec![("REGION".to_string(), "override".to_string())];
        assert_eq!(get_provider_env_value("REGION", Some(&env)), Some("override".to_string()));
        assert_eq!(get_provider_env_value("MISSING", Some(&env)), None);
        assert_eq!(get_provider_env_value("MISSING", None), None);
    }
}
