//! Config value resolution, port of `core/resolve-config-value.ts`.
//!
//! Values may be shell commands (`!cmd`, cached), environment references
//! (`$VAR` / `${VAR}` with `$$`/`$!` escapes), or literals.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use crate::utils::shell::get_shell_config;

static COMMAND_RESULT_CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

const ENV_VAR_NAME_RE: &str = r"^[A-Za-z_][A-Za-z0-9_]*$";

enum TemplatePart {
    Literal(String),
    Env(String),
}

enum ConfigValueReference {
    Command(String),
    Template(Vec<TemplatePart>),
}

fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(previous)) = parts.last_mut() {
        previous.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_string()));
}

fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    let mut parts: Vec<TemplatePart> = Vec::new();
    let mut index = 0usize;

    while index < config.len() {
        let Some(dollar_index) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar_index = index + dollar_index;

        append_literal(&mut parts, &config[index..dollar_index]);
        let next_char = config[dollar_index + 1..].chars().next();

        if next_char == Some('$') || next_char == Some('!') {
            append_literal(&mut parts, &next_char.unwrap().to_string());
            index = dollar_index + 2;
            continue;
        }

        if next_char == Some('{') {
            let Some(relative_end) = config[dollar_index + 2..].find('}') else {
                append_literal(&mut parts, "$");
                index = dollar_index + 1;
                continue;
            };
            let end_index = dollar_index + 2 + relative_end;
            let name = &config[dollar_index + 2..end_index];
            if is_env_var_name(name) {
                parts.push(TemplatePart::Env(name.to_string()));
            } else {
                append_literal(&mut parts, &config[dollar_index..end_index + 1]);
            }
            index = end_index + 1;
            continue;
        }

        // $NAME (no braces): longest prefix of [A-Za-z_][A-Za-z0-9_]*.
        let rest = &config[dollar_index + 1..];
        let name_len = rest
            .char_indices()
            .take_while(|(i, c)| {
                if *i == 0 {
                    c.is_ascii_alphabetic() || *c == '_'
                } else {
                    c.is_ascii_alphanumeric() || *c == '_'
                }
            })
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0);
        if name_len > 0 {
            parts.push(TemplatePart::Env(rest[..name_len].to_string()));
            index = dollar_index + 1 + name_len;
            continue;
        }

        append_literal(&mut parts, "$");
        index = dollar_index + 1;
    }

    parts
}

fn is_env_var_name(name: &str) -> bool {
    // Simple regex-free check matching ENV_VAR_NAME_RE.
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_config_value_reference(config: &str) -> ConfigValueReference {
    if config.starts_with('!') {
        return ConfigValueReference::Command(config.to_string());
    }
    ConfigValueReference::Template(parse_config_value_template(config))
}

fn resolve_env_config_value(name: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    env.and_then(|env| env.get(name).cloned())
        .or_else(|| std::env::var(name).ok())
}

fn get_template_env_var_names(parts: &[TemplatePart]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in parts {
        if let TemplatePart::Env(name) = part {
            if !names.iter().any(|existing| existing == name) {
                names.push(name.clone());
            }
        }
    }
    names
}

fn resolve_template(parts: &[TemplatePart], env: Option<&HashMap<String, String>>) -> Option<String> {
    let mut resolved = String::new();
    for part in parts {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(value),
            TemplatePart::Env(name) => {
                let env_value = resolve_env_config_value(name, env)?;
                resolved.push_str(&env_value);
            }
        }
    }
    Some(resolved)
}

pub fn get_config_value_env_var_name(config: &str) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(_) => None,
        ConfigValueReference::Template(parts) => match parts.as_slice() {
            [TemplatePart::Env(name)] => Some(name.clone()),
            _ => None,
        },
    }
}

pub fn get_config_value_env_var_names(config: &str) -> Vec<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(_) => Vec::new(),
        ConfigValueReference::Template(parts) => get_template_env_var_names(&parts),
    }
}

pub fn get_missing_config_value_env_var_names(config: &str, env: Option<&HashMap<String, String>>) -> Vec<String> {
    get_config_value_env_var_names(config)
        .into_iter()
        .filter(|name| resolve_env_config_value(name, env).is_none())
        .collect()
}

pub fn is_command_config_value(config: &str) -> bool {
    matches!(parse_config_value_reference(config), ConfigValueReference::Command(_))
}

pub fn is_config_value_configured(config: &str, env: Option<&HashMap<String, String>>) -> bool {
    get_missing_config_value_env_var_names(config, env).is_empty()
}

/// Execute `!command` via the default shell (non-Windows path: execSync).
fn execute_with_default_shell(command: &str) -> Option<String> {
    let output = Command::new("sh").args(["-c", command]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Windows-style configured shell path (kept for parity; on Unix the default
/// shell path is used).
fn execute_with_configured_shell(command: &str) -> Option<String> {
    let Ok(config) = get_shell_config(None) else {
        return None;
    };
    let mut process = Command::new(&config.shell);
    let mut args = config.args.clone();
    args.push(command.to_string());
    process.args(&args);
    let output = process.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn execute_command_uncached(command_config: &str) -> Option<String> {
    let command = &command_config[1..];
    if cfg!(windows) {
        execute_with_configured_shell(command).or_else(|| execute_with_default_shell(command))
    } else {
        execute_with_default_shell(command)
    }
}

fn execute_command(command_config: &str) -> Option<String> {
    let mut cache = COMMAND_RESULT_CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some(cached) = cache.get(command_config) {
        return cached.clone();
    }
    let result = execute_command_uncached(command_config);
    cache.insert(command_config.to_string(), result.clone());
    result
}

/// Resolve a config value (API key, header value, etc.) to an actual value.
pub fn resolve_config_value(config: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => execute_command(&command),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

/// Resolve without the command-result cache (used by resolve-or-throw).
pub fn resolve_config_value_uncached(config: &str, env: Option<&HashMap<String, String>>) -> Option<String> {
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => execute_command_uncached(&command),
        ConfigValueReference::Template(parts) => resolve_template(&parts, env),
    }
}

pub fn resolve_config_value_or_throw(
    config: &str,
    description: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<String, String> {
    if let Some(value) = resolve_config_value_uncached(config, env) {
        return Ok(value);
    }
    match parse_config_value_reference(config) {
        ConfigValueReference::Command(command) => {
            Err(format!("Failed to resolve {description} from shell command: {}", &command[1..]))
        }
        ConfigValueReference::Template(parts) => {
            let missing = get_missing_config_value_env_var_names(config, env);
            match missing.len() {
                1 => Err(format!(
                    "Failed to resolve {description} from environment variable: {}",
                    missing[0]
                )),
                len if len > 1 => Err(format!(
                    "Failed to resolve {description} from environment variables: {}",
                    missing.join(", ")
                )),
                _ => Err(format!("Failed to resolve {description}")),
            }
        }
    }
    .map_err(|message| message)
}

/// Resolve all header values using the same resolution logic as API keys.
/// Values that resolve to empty are dropped (JS truthiness).
pub fn resolve_headers(
    headers: &[(String, String)],
    env: Option<&HashMap<String, String>>,
) -> Option<Vec<(String, String)>> {
    let mut resolved: Vec<(String, String)> = Vec::new();
    for (key, value) in headers {
        if let Some(resolved_value) = resolve_config_value(value, env) {
            if !resolved_value.is_empty() {
                resolved.push((key.clone(), resolved_value));
            }
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(resolved)
    }
}

pub fn resolve_headers_or_throw(
    headers: &[(String, String)],
    description: &str,
    env: Option<&HashMap<String, String>>,
) -> Result<Vec<(String, String)>, String> {
    if headers.is_empty() {
        return Ok(Vec::new());
    }
    let mut resolved: Vec<(String, String)> = Vec::new();
    for (key, value) in headers {
        resolved.push((
            key.clone(),
            resolve_config_value_or_throw(value, &format!("{description} header \"{key}\""), env)?,
        ));
    }
    Ok(resolved)
}

/// Clear the config value command cache. Exported for testing.
pub fn clear_config_value_cache() {
    if let Some(cache) = COMMAND_RESULT_CACHE.lock().unwrap().as_mut() {
        cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_pass_through() {
        assert_eq!(resolve_config_value("sk-abc", None).as_deref(), Some("sk-abc"));
        assert_eq!(resolve_config_value("", None).as_deref(), Some(""));
    }

    #[test]
    fn env_var_references() {
        let env: HashMap<String, String> = [("MY_KEY".to_string(), "secret".to_string())].into();
        assert_eq!(resolve_config_value("$MY_KEY", Some(&env)).as_deref(), Some("secret"));
        assert_eq!(resolve_config_value("${MY_KEY}", Some(&env)).as_deref(), Some("secret"));
        assert_eq!(resolve_config_value("pre-$MY_KEY-post", Some(&env)).as_deref(), Some("pre-secret-post"));
        // Missing env var -> unresolved.
        assert_eq!(resolve_config_value("$NOPE", Some(&env)), None);
        // Escapes.
        assert_eq!(resolve_config_value("$$MY_KEY", Some(&env)).as_deref(), Some("$MY_KEY"));
        assert_eq!(resolve_config_value("$!MY_KEY", Some(&env)).as_deref(), Some("!MY_KEY"));
        // Invalid name stays literal.
        assert_eq!(resolve_config_value("$1KEY", Some(&env)).as_deref(), Some("$1KEY"));
        // Unclosed brace stays literal.
        assert_eq!(resolve_config_value("${MY_KEY", Some(&env)).as_deref(), Some("${MY_KEY"));
    }

    #[test]
    fn env_var_name_helpers() {
        assert_eq!(get_config_value_env_var_name("$MY_KEY").as_deref(), Some("MY_KEY"));
        assert_eq!(get_config_value_env_var_name("$MY_KEY-x"), None);
        assert_eq!(get_config_value_env_var_name("pre-$MY_KEY"), None);
        assert_eq!(get_config_value_env_var_name("!cmd"), None);
        let names = get_config_value_env_var_names("$A-$B-$A");
        assert_eq!(names, vec!["A".to_string(), "B".to_string()]);
        let missing = get_missing_config_value_env_var_names("$A-$B", None);
        assert_eq!(missing.len(), 2);
    }

    #[test]
    fn command_values_execute() {
        let value = resolve_config_value("!echo hello", None);
        assert_eq!(value.as_deref(), Some("hello"));
        // Cached.
        assert_eq!(resolve_config_value("!echo hello", None).as_deref(), Some("hello"));
        clear_config_value_cache();
        // Non-zero exit -> None.
        assert_eq!(resolve_config_value("!exit 3", None), None);
        clear_config_value_cache();
    }

    #[test]
    fn is_command_and_configured() {
        assert!(is_command_config_value("!echo hi"));
        assert!(!is_command_config_value("$KEY"));
        assert!(!is_config_value_configured("$MISSING", None));
        let env: HashMap<String, String> = [("K".to_string(), "v".to_string())].into();
        assert!(is_config_value_configured("$K", Some(&env)));
    }

    #[test]
    fn resolve_headers_drops_empty() {
        let env: HashMap<String, String> = [("TOKEN".to_string(), "t".to_string())].into();
        let headers = vec![
            ("Authorization".to_string(), "$TOKEN".to_string()),
            ("X-Empty".to_string(), "$NOPE".to_string()),
        ];
        let resolved = resolve_headers(&headers, Some(&env)).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], ("Authorization".to_string(), "t".to_string()));
    }

    #[test]
    fn resolve_or_throw_messages() {
        assert!(resolve_config_value_or_throw("$MISSING", "test key", None).is_err());
        assert!(resolve_config_value_or_throw("$A-$B", "test key", None).unwrap_err().contains("$A, $B".replace("$A, $B", "A, B").as_str()));
        assert!(resolve_config_value_or_throw("!false", "test key", None).unwrap_err().contains("shell command"));
        assert_eq!(
            resolve_config_value_or_throw("plain", "test key", None).unwrap(),
            "plain"
        );
    }

    #[test]
    fn resolve_headers_or_throw_works() {
        let headers = vec![("A".to_string(), "$MISSING".to_string())];
        assert!(resolve_headers_or_throw(&headers, "test", None).is_err());
        assert_eq!(resolve_headers_or_throw(&[], "test", None).unwrap(), Vec::<(String, String)>::new());
    }
}
