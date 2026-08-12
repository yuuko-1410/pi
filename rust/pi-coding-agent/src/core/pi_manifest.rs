//! `pi` manifest reading from package.json, port of `core/pi-manifest.ts`.

use std::fs;

use pi_ai::utils::json::parse_json_with_repair;
use pi_protocol::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PiManifest {
    pub extensions: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub prompts: Option<Vec<String>>,
    pub themes: Option<Vec<String>>,
}

const RESOURCE_FIELDS: [&str; 4] = ["extensions", "skills", "prompts", "themes"];

fn is_object(value: &Value) -> bool {
    matches!(value, Value::Map(_))
}

/// Read the `pi` manifest from a package.json path; null on parse errors or
/// when no `pi` field exists (mirrors the JS try/catch).
pub fn read_pi_manifest(package_json_path: &str) -> Option<PiManifest> {
    let content = fs::read_to_string(package_json_path).ok()?;
    let pkg: Value = parse_json_with_repair(&content).ok()?;
    if !is_object(&pkg) {
        return None;
    }
    let pi = match pkg.as_map()?.iter().find(|(k, _)| k == "pi") {
        Some((_, value)) => value,
        None => return None,
    };
    if !matches!(pi, Value::Map(_)) {
        return None;
    }

    let mut manifest = PiManifest::default();
    let fields = pi.as_map()?;
    for field in RESOURCE_FIELDS {
        let entries = fields.iter().find(|(k, _)| k == field).map(|(_, v)| v);
        match entries {
            Some(Value::Array(items)) if items.iter().all(|item| matches!(item, Value::String(_))) => {
                let list = items
                    .iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect();
                match field {
                    "extensions" => manifest.extensions = Some(list),
                    "skills" => manifest.skills = Some(list),
                    "prompts" => manifest.prompts = Some(list),
                    _ => manifest.themes = Some(list),
                }
            }
            _ => {}
        }
    }
    Some(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn write_temp(content: &str) -> String {
        let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("pi-manifest-{}-{counter}.json", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn reads_resource_fields() {
        let path = write_temp(r#"{"name":"x","pi":{"extensions":["a","b"],"themes":["t"]}}"#);
        let manifest = read_pi_manifest(&path).unwrap();
        assert_eq!(manifest.extensions.as_deref(), Some(&["a".to_string(), "b".to_string()][..]));
        assert_eq!(manifest.skills, None);
        assert_eq!(manifest.themes.as_deref(), Some(&["t".to_string()][..]));
    }

    #[test]
    fn missing_or_invalid_pi_field() {
        let path = write_temp(r#"{"name":"x"}"#);
        assert_eq!(read_pi_manifest(&path), None);
        let path = write_temp(r#"{"pi":"not-object"}"#);
        assert_eq!(read_pi_manifest(&path), None);
        let path = write_temp("not json");
        assert_eq!(read_pi_manifest(&path), None);
    }

    #[test]
    fn non_string_entries_ignored() {
        let path = write_temp(r#"{"pi":{"extensions":["a",3]}}"#);
        let manifest = read_pi_manifest(&path).unwrap();
        assert_eq!(manifest.extensions, None);
    }
}
