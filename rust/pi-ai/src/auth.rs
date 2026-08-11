//! Provider auth, port of the API-key resolution parts of
//! `packages/ai/src/auth/*`. The full OAuth flows (browser redirect
//! servers) are deferred; the api-key/env resolution used at request time is
//! fully ported.

use crate::types::ProviderEnv;

/// Resolved credentials for a request, mirroring the JS `ResolvedAuth`
/// variants used by provider request paths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedAuth {
    pub api_key: Option<String>,
    pub headers: Vec<(String, String)>,
    pub env: Option<ProviderEnv>,
}

/// An api-key auth configuration: the secret prompt label plus the ordered
/// environment variables consulted at resolve time.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiKeyAuth {
    pub name: String,
    /// Env vars in priority order (e.g. ANTHROPIC_AUTH_TOKEN,
    /// ANTHROPIC_OAUTH_TOKEN, ANTHROPIC_API_KEY_ENV).
    pub env_vars: Vec<String>,
}

/// Provider auth configuration. OAuth is represented by its name so the
/// provider factory shapes stay identical; resolving OAuth credentials is
/// deferred to the auth layer port.
#[derive(Clone, Debug, PartialEq)]
pub enum ProviderAuth {
    ApiKey(ApiKeyAuth),
    OAuth { name: String },
    ApiKeyAndOAuth { api_key: ApiKeyAuth, oauth_name: String },
    None,
}

/// Resolves an api-key auth from explicit credentials or the environment.
/// Mirrors the JS `resolve` functions: stored credential wins, then the env
/// vars in order; unknown env vars fall back to a bare api-key check.
pub fn resolve_api_key_auth(
    auth: &ApiKeyAuth,
    credential_key: Option<&str>,
    credential_env: Option<&ProviderEnv>,
    lookup_env: &dyn Fn(&str) -> Option<String>,
) -> Option<ResolvedAuth> {
    if let Some(key) = credential_key {
        if !key.is_empty() {
            return Some(ResolvedAuth {
                api_key: Some(key.to_string()),
                env: credential_env.cloned(),
                ..ResolvedAuth::default()
            });
        }
    }
    for env_var in &auth.env_vars {
        if let Some(value) = lookup_env(env_var) {
            if !value.is_empty() {
                // ANTHROPIC_AUTH_TOKEN-style vars are bearer tokens sent as
                // Authorization headers by the provider adapters; the api
                // key path handles the rest.
                return Some(ResolvedAuth {
                    api_key: Some(value),
                    env: None,
                    ..ResolvedAuth::default()
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_wins_then_env_vars_in_order() {
        let auth = ApiKeyAuth {
            name: "Test key".to_string(),
            env_vars: vec!["TEST_TOKEN".to_string(), "TEST_KEY".to_string()],
        };
        let env = |name: &str| -> Option<String> {
            match name {
                "TEST_TOKEN" => None,
                "TEST_KEY" => Some("env-key".to_string()),
                _ => None,
            }
        };
        let resolved = resolve_api_key_auth(&auth, None, None, &env).unwrap();
        assert_eq!(resolved.api_key.as_deref(), Some("env-key"));

        let resolved = resolve_api_key_auth(&auth, Some("stored"), None, &env).unwrap();
        assert_eq!(resolved.api_key.as_deref(), Some("stored"));

        let empty_env = |_: &str| None;
        assert!(resolve_api_key_auth(&auth, None, None, &empty_env).is_none());
    }
}
