//! Model resolution, scoping, and initial selection, port of
//! `core/model-resolver.ts`. minimatch is a hand-written glob subset
//! (`*`, `?`, `[...]`, nocase); chalk is plain ANSI SGR.

use pi_ai::models::models_are_equal;
use pi_ai::types::Model;

use super::defaults::DEFAULT_THINKING_LEVEL;

pub const VALID_THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub fn is_valid_thinking_level(level: &str) -> bool {
    VALID_THINKING_LEVELS.contains(&level)
}

/// Default model IDs for each known provider.
pub const DEFAULT_MODEL_PER_PROVIDER: [(&str, &str); 40] = [
    ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
    ("ant-ling", "Ring-2.6-1T"),
    ("anthropic", "claude-opus-4-8"),
    ("openai", "gpt-5.5"),
    ("azure-openai-responses", "gpt-5.4"),
    ("openai-codex", "gpt-5.5"),
    ("radius", "auto"),
    ("nvidia", "nvidia/nemotron-3-super-120b-a12b"),
    ("deepseek", "deepseek-v4-pro"),
    ("google", "gemini-3.1-pro-preview"),
    ("google-vertex", "gemini-3.1-pro-preview"),
    ("github-copilot", "gpt-5.4"),
    ("openrouter", "moonshotai/kimi-k2.6"),
    ("vercel-ai-gateway", "zai/glm-5.1"),
    ("xai", "grok-4.5"),
    ("groq", "openai/gpt-oss-120b"),
    ("cerebras", "zai-glm-4.7"),
    ("zai", "glm-5.1"),
    ("zai-coding-cn", "glm-5.1"),
    ("mistral", "devstral-medium-latest"),
    ("minimax", "MiniMax-M2.7"),
    ("minimax-cn", "MiniMax-M2.7"),
    ("moonshotai", "kimi-k2.6"),
    ("moonshotai-cn", "kimi-k2.6"),
    ("huggingface", "moonshotai/Kimi-K2.6"),
    ("fireworks", "accounts/fireworks/models/kimi-k2p6"),
    ("together", "moonshotai/Kimi-K2.6"),
    ("baseten", "zai-org/GLM-5.2"),
    ("opencode", "kimi-k2.6"),
    ("opencode-go", "kimi-k2.6"),
    ("kimi-coding", "kimi-for-coding"),
    ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
    ("cloudflare-ai-gateway", "workers-ai/@cf/moonshotai/kimi-k2.6"),
    ("qwen-token-plan", "qwen3.7-max"),
    ("qwen-token-plan-cn", "qwen3.7-max"),
    ("qwen-token-plan-individual", "qwen3.8-max"),
    ("xiaomi", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-cn", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-ams", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-sgp", "mimo-v2.5-pro"),
];

pub fn default_model_per_provider(provider: &str) -> Option<&'static str> {
    DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, model)| *model)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScopedModel {
    pub model: Model,
    /// Thinking level if explicitly specified in the pattern.
    pub thinking_level: Option<String>,
}

/// Check if a model ID looks like an alias (no -YYYYMMDD date suffix).
fn is_alias(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }
    // Dated versions end with -YYYYMMDD.
    let Some(index) = id.rfind('-') else {
        return true;
    };
    let suffix = &id[index + 1..];
    !(suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()))
}

/// Find an exact model reference match (bare id or provider/modelId).
pub fn find_exact_model_reference_match(model_reference: &str, available_models: &[Model]) -> Option<Model> {
    let trimmed_reference = model_reference.trim();
    if trimmed_reference.is_empty() {
        return None;
    }
    let normalized_reference = trimmed_reference.to_lowercase();

    let canonical_matches: Vec<&Model> = available_models
        .iter()
        .filter(|model| format!("{}/{}", model.provider, model.id).to_lowercase() == normalized_reference)
        .collect();
    if canonical_matches.len() == 1 {
        return Some(canonical_matches[0].clone());
    }
    if canonical_matches.len() > 1 {
        return None;
    }

    if let Some(slash_index) = trimmed_reference.find('/') {
        let provider = trimmed_reference[..slash_index].trim();
        let model_id = trimmed_reference[slash_index + 1..].trim();
        if !provider.is_empty() && !model_id.is_empty() {
            let provider_matches: Vec<&Model> = available_models
                .iter()
                .filter(|model| {
                    model.provider.to_lowercase() == provider.to_lowercase()
                        && model.id.to_lowercase() == model_id.to_lowercase()
                })
                .collect();
            if provider_matches.len() == 1 {
                return Some(provider_matches[0].clone());
            }
            if provider_matches.len() > 1 {
                return None;
            }
        }
    }

    let id_matches: Vec<&Model> = available_models
        .iter()
        .filter(|model| model.id.to_lowercase() == normalized_reference)
        .collect();
    if id_matches.len() == 1 {
        Some(id_matches[0].clone())
    } else {
        None
    }
}

/// Try to match a pattern to a model: exact first, then partial id/name with
/// alias preference over dated versions.
fn try_match_model(model_pattern: &str, available_models: &[Model]) -> Option<Model> {
    if let Some(exact_match) = find_exact_model_reference_match(model_pattern, available_models) {
        return Some(exact_match);
    }

    let pattern_lower = model_pattern.to_lowercase();
    let matches: Vec<&Model> = available_models
        .iter()
        .filter(|model| {
            model.id.to_lowercase().contains(&pattern_lower)
                || model.name.to_lowercase().contains(&pattern_lower)
        })
        .collect();

    if matches.is_empty() {
        return None;
    }

    let mut aliases: Vec<&Model> = matches.iter().filter(|model| is_alias(&model.id)).cloned().collect();
    if !aliases.is_empty() {
        aliases.sort_by(|a, b| b.id.cmp(&a.id));
        return Some(aliases[0].clone());
    }

    let mut dated_versions: Vec<&Model> = matches.into_iter().filter(|model| !is_alias(&model.id)).collect();
    dated_versions.sort_by(|a, b| b.id.cmp(&a.id));
    dated_versions.first().map(|model| (*model).clone())
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<String>,
    pub warning: Option<String>,
}

fn build_fallback_model(provider: &str, model_id: &str, available_models: &[Model]) -> Option<Model> {
    let provider_models: Vec<&Model> = available_models.iter().filter(|model| model.provider == provider).collect();
    if provider_models.is_empty() {
        return None;
    }
    let default_id = default_model_per_provider(provider);
    let base_model = match default_id {
        Some(default_id) => provider_models
            .iter()
            .find(|model| model.id == default_id)
            .map(|model| *model)
            .unwrap_or(provider_models[0]),
        None => provider_models[0],
    };

    let mut model = base_model.clone();
    model.id = model_id.to_string();
    model.name = model_id.to_string();
    Some(model)
}

/// Parse a pattern to extract model and thinking level. Handles models with
/// colons in their IDs (e.g. OpenRouter's :exacto suffix).
pub fn parse_model_pattern(
    pattern: &str,
    available_models: &[Model],
    allow_invalid_thinking_level_fallback: bool,
) -> ParsedModelResult {
    // Try exact match first.
    if let Some(exact_match) = try_match_model(pattern, available_models) {
        return ParsedModelResult {
            model: Some(exact_match),
            thinking_level: None,
            warning: None,
        };
    }

    // Try splitting on the last colon.
    let Some(last_colon_index) = pattern.rfind(':') else {
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    };

    let prefix = &pattern[..last_colon_index];
    let suffix = &pattern[last_colon_index + 1..];

    if is_valid_thinking_level(suffix) {
        let result = parse_model_pattern(prefix, available_models, allow_invalid_thinking_level_fallback);
        if result.model.is_some() {
            return ParsedModelResult {
                thinking_level: if result.warning.is_some() { None } else { Some(suffix.to_string()) },
                ..result
            };
        }
        return result;
    }

    if !allow_invalid_thinking_level_fallback {
        // Strict mode (CLI --model parsing): treat as part of the model id.
        return ParsedModelResult {
            model: None,
            thinking_level: None,
            warning: None,
        };
    }

    // Scope mode: recurse on prefix and warn.
    let result = parse_model_pattern(prefix, available_models, allow_invalid_thinking_level_fallback);
    if result.model.is_some() {
        return ParsedModelResult {
            model: result.model,
            thinking_level: None,
            warning: Some(format!(
                "Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."
            )),
        };
    }
    result
}

/// Simple minimatch subset: `*`, `?`, `[...]`, nocase. Compiles a glob to a
/// matching predicate against full strings (slashes are ordinary chars).
/// ponytail: no brace expansion, extglobs, or character classes with ranges
/// beyond plain sets; add when a pattern in the wild needs them.
fn glob_match(glob: &str, text: &str, nocase: bool) -> bool {
    let haystack = if nocase { text.to_lowercase() } else { text.to_string() };
    let needle = if nocase { glob.to_lowercase() } else { glob.to_string() };
    let glob: Vec<char> = needle.chars().collect();
    let text: Vec<char> = haystack.chars().collect();
    glob_match_chars(&glob, &text)
}

fn glob_match_chars(glob: &[char], text: &[char]) -> bool {
    // Iterative backtracking: star matches greedily with fallback positions.
    let mut g = 0usize;
    let mut t = 0usize;
    let mut star_g = None;
    let mut star_t = 0usize;

    while t < text.len() {
        if g < glob.len() && (glob[g] == '?' || glob[g] == text[t]) {
            g += 1;
            t += 1;
        } else if g < glob.len() && glob[g] == '*' {
            star_g = Some(g);
            g += 1;
            star_t = t;
        } else if g < glob.len() && glob[g] == '[' {
            // Character class.
            let mut j = g + 1;
            let negate = j < glob.len() && glob[j] == '!';
            if negate {
                j += 1;
            }
            let mut matched = false;
            let mut closed = false;
            while j < glob.len() {
                if glob[j] == ']' {
                    closed = true;
                    break;
                }
                if j + 2 < glob.len() && glob[j + 1] == '-' && glob[j + 2] != ']' {
                    if glob[j] <= text[t] && text[t] <= glob[j + 2] {
                        matched = true;
                    }
                    j += 3;
                } else {
                    if glob[j] == text[t] {
                        matched = true;
                    }
                    j += 1;
                }
            }
            if !closed {
                // Unterminated class is a literal '['.
                if glob[g] == text[t] {
                    g += 1;
                    t += 1;
                } else {
                    return false;
                }
            } else if matched == negate {
                return false;
            } else {
                g = j + 1;
                t += 1;
            }
        } else if let Some(star) = star_g {
            g = star + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while g < glob.len() && glob[g] == '*' {
        g += 1;
    }
    g == glob.len()
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelScopeDiagnostic {
    pub kind: &'static str, // "warning"
    pub code: &'static str, // "no-match" | "invalid-thinking-level"
    pub message: String,
    pub pattern: String,
}

#[derive(Clone, Debug, Default)]
pub struct ResolveModelScopeResult {
    pub scoped_models: Vec<ScopedModel>,
    pub diagnostics: Vec<ModelScopeDiagnostic>,
}

pub fn resolve_model_scope_from_models(patterns: &[String], models: &[Model]) -> ResolveModelScopeResult {
    let available_models = models.to_vec();
    let mut scoped_models: Vec<ScopedModel> = Vec::new();
    let mut diagnostics: Vec<ModelScopeDiagnostic> = Vec::new();

    for pattern in patterns {
        // Glob patterns.
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            let colon_idx = pattern.rfind(':');
            let mut glob_pattern = pattern.clone();
            let mut thinking_level: Option<String> = None;

            if let Some(colon_idx) = colon_idx {
                let suffix = &pattern[colon_idx + 1..];
                if is_valid_thinking_level(suffix) {
                    thinking_level = Some(suffix.to_string());
                    glob_pattern = pattern[..colon_idx].to_string();
                }
            }

            if let Some(exact_match) = find_exact_model_reference_match(&glob_pattern, &available_models) {
                if !scoped_models
                    .iter()
                    .any(|sm| models_are_equal(Some(&sm.model), Some(&exact_match)))
                {
                    scoped_models.push(ScopedModel {
                        model: exact_match,
                        thinking_level: thinking_level.clone(),
                    });
                }
                continue;
            }

            let matching_models: Vec<&Model> = available_models
                .iter()
                .filter(|model| {
                    let full_id = format!("{}/{}", model.provider, model.id);
                    glob_match(&glob_pattern, &full_id, true) || glob_match(&glob_pattern, &model.id, true)
                })
                .collect();

            if matching_models.is_empty() {
                diagnostics.push(ModelScopeDiagnostic {
                    kind: "warning",
                    code: "no-match",
                    message: format!("No models match pattern \"{pattern}\""),
                    pattern: pattern.clone(),
                });
                continue;
            }

            for model in matching_models {
                if !scoped_models
                    .iter()
                    .any(|sm| models_are_equal(Some(&sm.model), Some(model)))
                {
                    scoped_models.push(ScopedModel {
                        model: model.clone(),
                        thinking_level: thinking_level.clone(),
                    });
                }
            }
            continue;
        }

        let result = parse_model_pattern(pattern, &available_models, true);

        if let Some(warning) = &result.warning {
            diagnostics.push(ModelScopeDiagnostic {
                kind: "warning",
                code: "invalid-thinking-level",
                message: warning.clone(),
                pattern: pattern.clone(),
            });
        }

        let Some(model) = result.model else {
            diagnostics.push(ModelScopeDiagnostic {
                kind: "warning",
                code: "no-match",
                message: format!("No models match pattern \"{pattern}\""),
                pattern: pattern.clone(),
            });
            continue;
        };

        if !scoped_models.iter().any(|sm| models_are_equal(Some(&sm.model), Some(&model))) {
            scoped_models.push(ScopedModel {
                model,
                thinking_level: result.thinking_level,
            });
        }
    }

    ResolveModelScopeResult {
        scoped_models,
        diagnostics,
    }
}

/// Model runtime surface used by the resolver (implemented by ModelRuntime).
pub trait ModelRuntimeLike {
    fn get_models(&self) -> Vec<Model>;
    fn get_available_snapshot(&self) -> Vec<Model>;
    fn get_model(&self, provider: &str, model_id: &str) -> Option<Model>;
    fn has_configured_auth(&self, provider: &str) -> bool;
}

#[derive(Clone, Debug)]
pub struct ResolveCliModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

/// Resolve a single model from CLI flags.
pub fn resolve_cli_model(
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
    model_runtime: &dyn ModelRuntimeLike,
) -> ResolveCliModelResult {
    let Some(cli_model) = cli_model else {
        return ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: None,
        };
    };

    // Use all models, not just authenticated ones (allows --api-key first setup).
    let available_models = model_runtime.get_models();
    if available_models.is_empty() {
        return ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: Some("No models available. Check your installation or add models to models.json.".to_string()),
        };
    }

    // Build a canonical provider lookup (case-insensitive).
    let mut provider_map: Vec<(String, String)> = Vec::new();
    for model in &available_models {
        let key = model.provider.to_lowercase();
        if !provider_map.iter().any(|(existing, _)| *existing == key) {
            provider_map.push((key, model.provider.clone()));
        }
    }
    let canonical_provider = |name: &str| -> Option<String> {
        provider_map
            .iter()
            .find(|(existing, _)| *existing == name.to_lowercase())
            .map(|(_, canonical)| canonical.clone())
    };

    let mut provider = cli_provider.and_then(canonical_provider);
    if cli_provider.is_some() && provider.is_none() {
        return ResolveCliModelResult {
            model: None,
            thinking_level: None,
            warning: None,
            error: Some(format!(
                "Unknown provider \"{}\". Use --list-models to see available providers/models.",
                cli_provider.unwrap()
            )),
        };
    }

    // Interpret "provider/model" when the prefix matches a known provider.
    let mut pattern = cli_model.to_string();
    let mut inferred_provider = false;
    if provider.is_none() {
        if let Some(slash_index) = cli_model.find('/') {
            let maybe_provider = &cli_model[..slash_index];
            if let Some(canonical) = canonical_provider(maybe_provider) {
                provider = Some(canonical);
                pattern = cli_model[slash_index + 1..].to_string();
                inferred_provider = true;
            }
        }
    }

    // Exact matches without provider inference.
    if provider.is_none() {
        let lower = cli_model.to_lowercase();
        let exact_matches: Vec<&Model> = available_models
            .iter()
            .filter(|model| {
                model.id.to_lowercase() == lower || format!("{}/{}", model.provider, model.id).to_lowercase() == lower
            })
            .collect();
        if exact_matches.len() == 1 {
            return ResolveCliModelResult {
                model: Some(exact_matches[0].clone()),
                warning: None,
                thinking_level: None,
                error: None,
            };
        }
        if exact_matches.len() > 1 {
            let authenticated: Vec<&Model> = exact_matches
                .iter()
                .filter(|model| model_runtime.has_configured_auth(&model.provider))
                .cloned()
                .collect();
            if authenticated.len() == 1 {
                return ResolveCliModelResult {
                    model: Some(authenticated[0].clone()),
                    warning: None,
                    thinking_level: None,
                    error: None,
                };
            }
            let mut matches: Vec<String> = exact_matches
                .iter()
                .map(|model| format!("{}/{}", model.provider, model.id))
                .collect();
            matches.sort();
            let auth_hint = if authenticated.is_empty() {
                "No matching provider is authenticated.".to_string()
            } else {
                "More than one matching provider is authenticated.".to_string()
            };
            return ResolveCliModelResult {
                model: None,
                warning: None,
                thinking_level: None,
                error: Some(format!(
                    "Model \"{cli_model}\" is ambiguous across providers: {}. {auth_hint} Use --provider or provider/model.",
                    matches.join(", ")
                )),
            };
        }
    }

    if cli_provider.is_some() && provider.is_some() {
        // Both provided: tolerate a provider/ prefix in --model.
        let prefix = format!("{}/", provider.as_deref().unwrap());
        if cli_model.to_lowercase().starts_with(&prefix.to_lowercase()) {
            pattern = cli_model[prefix.len()..].to_string();
        }
    }

    let candidates: Vec<Model> = match &provider {
        Some(provider) => available_models.iter().filter(|model| model.provider == *provider).cloned().collect(),
        None => available_models.clone(),
    };
    let result = parse_model_pattern(&pattern, &candidates, false);

    if let Some(model) = &result.model {
        // Prefer an authenticated raw model-id match when inference matched an
        // unauthenticated provider/model pair.
        if inferred_provider {
            let raw_exact_matches: Vec<&Model> = available_models
                .iter()
                .filter(|m| m.id.to_lowercase() == cli_model.to_lowercase() && !models_are_equal(Some(m), Some(model)))
                .collect();
            if !raw_exact_matches.is_empty() && !model_runtime.has_configured_auth(&model.provider) {
                let authenticated_raw: Vec<&Model> = raw_exact_matches
                    .iter()
                    .filter(|m| model_runtime.has_configured_auth(&m.provider))
                    .cloned()
                    .collect();
                if authenticated_raw.len() == 1 {
                    return ResolveCliModelResult {
                        model: Some(authenticated_raw[0].clone()),
                        thinking_level: None,
                        warning: None,
                        error: None,
                    };
                }
            }
        }
        return ResolveCliModelResult {
            model: Some(model.clone()),
            thinking_level: result.thinking_level,
            warning: result.warning,
            error: None,
        };
    }

    // Fall back to matching the full input as a raw model id.
    if inferred_provider {
        let lower = cli_model.to_lowercase();
        if let Some(exact) = available_models.iter().find(|model| {
            model.id.to_lowercase() == lower || format!("{}/{}", model.provider, model.id).to_lowercase() == lower
        }) {
            return ResolveCliModelResult {
                model: Some(exact.clone()),
                warning: None,
                thinking_level: None,
                error: None,
            };
        }
        let fallback = parse_model_pattern(cli_model, &available_models, false);
        if fallback.model.is_some() {
            return ResolveCliModelResult {
                model: fallback.model,
                thinking_level: fallback.thinking_level,
                warning: fallback.warning,
                error: None,
            };
        }
    }

    if let Some(provider) = &provider {
        // Parse a thinking level suffix before building the fallback model.
        let mut fallback_pattern = pattern.clone();
        let mut fallback_thinking: Option<String> = None;
        if let Some(last_colon) = pattern.rfind(':') {
            let suffix = &pattern[last_colon + 1..];
            if is_valid_thinking_level(suffix) {
                fallback_pattern = pattern[..last_colon].to_string();
                fallback_thinking = Some(suffix.to_string());
            }
        }

        if let Some(fallback_model) = build_fallback_model(provider, &fallback_pattern, &available_models) {
            let requested_thinking = cli_thinking_level(None, &fallback_thinking);
            let mut model = fallback_model;
            if requested_thinking.is_some() && requested_thinking.as_deref() != Some("off") {
                model.reasoning = true;
            }
            let fallback_warning = match &result.warning {
                Some(warning) => format!(
                    "{warning} Model \"{fallback_pattern}\" not found for provider \"{provider}\". Using custom model id."
                ),
                None => format!(
                    "Model \"{fallback_pattern}\" not found for provider \"{provider}\". Using custom model id."
                ),
            };
            return ResolveCliModelResult {
                model: Some(model),
                thinking_level: fallback_thinking,
                warning: Some(fallback_warning),
                error: None,
            };
        }
    }

    let display = match &provider {
        Some(provider) => format!("{provider}/{pattern}"),
        None => cli_model.to_string(),
    };
    ResolveCliModelResult {
        model: None,
        thinking_level: None,
        warning: result.warning,
        error: Some(format!("Model \"{display}\" not found. Use --list-models to see available models.")),
    }
}

/// cliThinking support (defaults to the parsed fallback).
fn cli_thinking_level(cli_thinking: Option<&str>, fallback: &Option<String>) -> Option<String> {
    cli_thinking.map(|value| value.to_string()).or_else(|| fallback.clone())
}

#[derive(Clone, Debug)]
pub struct InitialModelResult {
    pub model: Option<Model>,
    pub thinking_level: String,
    pub fallback_message: Option<String>,
}

/// Find the initial model by priority: CLI args, scoped models, saved
/// default, first available with auth.
pub fn find_initial_model(
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
    scoped_models: &[ScopedModel],
    is_continuing: bool,
    default_provider: Option<&str>,
    default_model_id: Option<&str>,
    default_thinking_level: Option<&str>,
    model_runtime: &dyn ModelRuntimeLike,
) -> InitialModelResult {
    // 1. CLI args take priority.
    if cli_provider.is_some() && cli_model.is_some() {
        let resolved = resolve_cli_model(cli_provider, cli_model, model_runtime);
        if let Some(error) = &resolved.error {
            eprintln!("\u{1b}[31m{error}\u{1b}[0m");
            std::process::exit(1);
        }
        if resolved.model.is_some() {
            return InitialModelResult {
                model: resolved.model,
                thinking_level: DEFAULT_THINKING_LEVEL.to_string(),
                fallback_message: None,
            };
        }
    }

    // 2. First scoped model (skip when continuing/resuming).
    if !scoped_models.is_empty() && !is_continuing {
        return InitialModelResult {
            model: Some(scoped_models[0].model.clone()),
            thinking_level: scoped_models[0]
                .thinking_level
                .clone()
                .or_else(|| default_thinking_level.map(|value| value.to_string()))
                .unwrap_or_else(|| DEFAULT_THINKING_LEVEL.to_string()),
            fallback_message: None,
        };
    }

    // 3. Saved default from settings if auth is configured.
    if let (Some(default_provider), Some(default_model_id)) = (default_provider, default_model_id) {
        if let Some(found) = model_runtime.get_model(default_provider, default_model_id) {
            if model_runtime.has_configured_auth(&found.provider) {
                let thinking_level = default_thinking_level
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| DEFAULT_THINKING_LEVEL.to_string());
                return InitialModelResult {
                    model: Some(found),
                    thinking_level,
                    fallback_message: None,
                };
            }
        }
    }

    // 4. First available model with a valid API key.
    let available_models = model_runtime.get_available_snapshot();
    if !available_models.is_empty() {
        for (provider, default_id) in DEFAULT_MODEL_PER_PROVIDER {
            if let Some(match_model) = available_models
                .iter()
                .find(|model| model.provider == provider && model.id == default_id)
            {
                return InitialModelResult {
                    model: Some(match_model.clone()),
                    thinking_level: DEFAULT_THINKING_LEVEL.to_string(),
                    fallback_message: None,
                };
            }
        }
        return InitialModelResult {
            model: Some(available_models[0].clone()),
            thinking_level: DEFAULT_THINKING_LEVEL.to_string(),
            fallback_message: None,
        };
    }

    // 5. No model found.
    InitialModelResult {
        model: None,
        thinking_level: DEFAULT_THINKING_LEVEL.to_string(),
        fallback_message: None,
    }
}

/// Restore a model from the session, with fallback to available models.
pub fn restore_model_from_session(
    saved_provider: &str,
    saved_model_id: &str,
    current_model: Option<&Model>,
    should_print_messages: bool,
    model_runtime: &dyn ModelRuntimeLike,
) -> (Option<Model>, Option<String>) {
    let restored_model = model_runtime.get_model(saved_provider, saved_model_id);
    let has_configured_auth = restored_model
        .as_ref()
        .is_some_and(|model| model_runtime.has_configured_auth(&model.provider));

    if restored_model.is_some() && has_configured_auth {
        if should_print_messages {
            println!("\u{1b}[2mRestored model: {saved_provider}/{saved_model_id}\u{1b}[0m");
        }
        return (restored_model, None);
    }

    let reason = if restored_model.is_none() {
        "model no longer exists"
    } else {
        "no auth configured"
    };

    if should_print_messages {
        eprintln!(
            "\u{1b}[33mWarning: Could not restore model {saved_provider}/{saved_model_id} ({reason}).\u{1b}[0m"
        );
    }

    if let Some(current_model) = current_model {
        if should_print_messages {
            println!(
                "\u{1b}[2mFalling back to: {}/{}\u{1b}[0m",
                current_model.provider, current_model.id
            );
        }
        return (
            Some(current_model.clone()),
            Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                current_model.provider, current_model.id
            )),
        );
    }

    let available_models = model_runtime.get_available_snapshot();
    if !available_models.is_empty() {
        let mut fallback_model: Option<Model> = None;
        for (provider, default_id) in DEFAULT_MODEL_PER_PROVIDER {
            if let Some(match_model) = available_models
                .iter()
                .find(|model| model.provider == provider && model.id == default_id)
            {
                fallback_model = Some(match_model.clone());
                break;
            }
        }
        if fallback_model.is_none() {
            fallback_model = Some(available_models[0].clone());
        }
        let fallback_model = fallback_model.unwrap();
        if should_print_messages {
            println!(
                "\u{1b}[2mFalling back to: {}/{}\u{1b}[0m",
                fallback_model.provider, fallback_model.id
            );
        }
        return (
            Some(fallback_model.clone()),
            Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                fallback_model.provider, fallback_model.id
            )),
        );
    }

    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider: &str, id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: id.to_string(),
            api: "openai".into(),
            provider: provider.to_string(),
            base_url: "https://example.com".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec!["text".into()],
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
        }
    }

    fn models() -> Vec<Model> {
        vec![
            model("anthropic", "claude-sonnet-4-5"),
            model("anthropic", "claude-sonnet-4-5-20250929"),
            model("openai", "gpt-5"),
            model("openrouter", "openai/gpt-4o:extended"),
        ]
    }

    #[test]
    fn exact_match_bare_and_canonical() {
        let available = models();
        assert_eq!(
            find_exact_model_reference_match("gpt-5", &available).unwrap().provider,
            "openai"
        );
        assert_eq!(
            find_exact_model_reference_match("anthropic/claude-sonnet-4-5", &available)
                .unwrap()
                .id,
            "claude-sonnet-4-5"
        );
        // Ambiguous across providers -> None.
        assert_eq!(find_exact_model_reference_match("", &available), None);
    }

    #[test]
    fn partial_match_prefers_alias() {
        let available = models();
        let matched = try_match_model("claude-sonnet", &available).unwrap();
        assert_eq!(matched.id, "claude-sonnet-4-5"); // alias beats dated
    }

    #[test]
    fn parse_pattern_thinking_level() {
        let available = models();
        let result = parse_model_pattern("claude-sonnet:high", &available, true);
        assert_eq!(result.model.unwrap().provider, "anthropic");
        assert_eq!(result.thinking_level.as_deref(), Some("high"));
        assert_eq!(result.warning, None);

        // Invalid thinking level warns and falls back.
        let result = parse_model_pattern("claude-sonnet:bogus", &available, true);
        assert_eq!(result.model.unwrap().id, "claude-sonnet-4-5");
        assert!(result.warning.as_deref().unwrap().contains("Invalid thinking level"));

        // Strict mode: no fallback.
        let result = parse_model_pattern("claude-sonnet:bogus", &available, false);
        assert!(result.model.is_none());
    }

    #[test]
    fn colon_in_model_id() {
        let available = models();
        let result = parse_model_pattern("openai/gpt-4o:extended", &available, false);
        assert_eq!(result.model.unwrap().provider, "openrouter");
    }

    #[test]
    fn glob_scoping() {
        let available = models();
        let result = resolve_model_scope_from_models(&["anthropic/*".to_string()], &available);
        assert_eq!(result.scoped_models.len(), 2);
        assert!(result.diagnostics.is_empty());

        let result = resolve_model_scope_from_models(&["*sonnet*".to_string()], &available);
        assert_eq!(result.scoped_models.len(), 2);

        let result = resolve_model_scope_from_models(&["no-match-*".to_string()], &available);
        assert!(result.scoped_models.is_empty());
        assert_eq!(result.diagnostics[0].code, "no-match");
    }

    #[test]
    fn glob_matcher() {
        assert!(glob_match("anthropic/*", "anthropic/claude", true));
        assert!(!glob_match("anthropic/*", "openai/gpt", true));
        assert!(glob_match("?pt", "gpt", true));
        assert!(glob_match("[og]pt", "gpt", true));
        assert!(!glob_match("[og]pt", "xpt", true));
        assert!(glob_match("a*b", "axxb", true));
    }

    #[test]
    fn is_alias_detection() {
        assert!(is_alias("claude-sonnet-4-5"));
        assert!(!is_alias("claude-sonnet-4-5-20250929"));
        assert!(is_alias("gpt-5-latest"));
    }

    #[test]
    fn cli_model_resolution() {
        struct Runtime(Vec<Model>);
        impl ModelRuntimeLike for Runtime {
            fn get_models(&self) -> Vec<Model> {
                self.0.clone()
            }
            fn get_available_snapshot(&self) -> Vec<Model> {
                self.0.clone()
            }
            fn get_model(&self, provider: &str, model_id: &str) -> Option<Model> {
                self.0
                    .iter()
                    .find(|model| model.provider == provider && model.id == model_id)
                    .cloned()
            }
            fn has_configured_auth(&self, provider: &str) -> bool {
                provider == "anthropic"
            }
        }
        let runtime = Runtime(models());

        // provider/model inference.
        let result = resolve_cli_model(Some("anthropic"), Some("claude-sonnet"), &runtime);
        assert_eq!(result.model.unwrap().id, "claude-sonnet-4-5");
        assert!(result.error.is_none());

        // Bare ambiguous exact id across providers: anthropic is authenticated.
        let result = resolve_cli_model(None, Some("openai/gpt-4o:extended"), &runtime);
        assert_eq!(result.model.unwrap().provider, "openrouter");

        // Unknown provider errors.
        let result = resolve_cli_model(Some("nope"), Some("x"), &runtime);
        assert!(result.error.as_deref().unwrap().contains("Unknown provider"));

        // Fallback custom model id for a known provider.
        let result = resolve_cli_model(Some("anthropic"), Some("custom-model"), &runtime);
        assert_eq!(result.model.unwrap().id, "custom-model");
        assert!(result.warning.as_deref().unwrap().contains("Using custom model id"));

        // No model at all.
        let result = resolve_cli_model(None, None, &runtime);
        assert!(result.model.is_none());
        assert!(result.error.is_none());
    }

    #[test]
    fn default_model_table_covers_known_providers() {
        assert_eq!(default_model_per_provider("anthropic"), Some("claude-opus-4-8"));
        assert_eq!(default_model_per_provider("openai"), Some("gpt-5.5"));
        assert_eq!(default_model_per_provider("nope"), None);
    }
}
