//! HTML session export, port of `core/export-html/index.ts`.
//! The template assets are read from the JS package source at runtime
//! (packages/coding-agent/src/core/export-html/), mirroring the JS
//! getExportTemplateDir behavior for tsx runs.

use std::path::PathBuf;



use super::session_manager::SessionManager;
use crate::modes::interactive::theme::theme::{get_resolved_theme_colors, get_theme_export_colors};

/// Parse a color string to RGB. Supports hex (#RRGGBB) and rgb(r,g,b).
fn parse_color(color: &str) -> Option<(u8, u8, u8)> {
    if let Some(hex) = color.strip_prefix('#') {
        if hex.len() == 6 {
            if let Ok(value) = u32::from_str_radix(hex, 16) {
                return Some(((value >> 16) as u8, (value >> 8) as u8, value as u8));
            }
        }
        return None;
    }
    // rgb(r, g, b)
    let inner = color.trim();
    if let Some(body) = inner.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<&str> = body.split(',').map(|s| s.trim()).collect();
        if parts.len() == 3 {
            let parse = |s: &str| s.parse::<u8>().ok();
            if let (Some(r), Some(g), Some(b)) = (parse(parts[0]), parse(parts[1]), parse(parts[2])) {
                return Some((r, g, b));
            }
        }
    }
    None
}

/// Relative luminance (0-1, higher = lighter).
fn get_luminance(r: u8, g: u8, b: u8) -> f64 {
    let to_linear = |c: u8| {
        let s = f64::from(c) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * to_linear(r) + 0.7152 * to_linear(g) + 0.0722 * to_linear(b)
}

/// Adjust color brightness. Factor > 1 lightens, < 1 darkens.
fn adjust_brightness(color: &str, factor: f64) -> String {
    match parse_color(color) {
        Some((r, g, b)) => {
            let adjust = |c: u8| (f64::from(c) * factor).round().clamp(0.0, 255.0) as u8;
            format!("rgb({}, {}, {})", adjust(r), adjust(g), adjust(b))
        }
        None => color.to_string(),
    }
}

/// Derive export background colors from a base color (e.g. userMessageBg).
fn derive_export_colors(base_color: &str) -> (String, String, String) {
    let Some((r, g, b)) = parse_color(base_color) else {
        return (
            "rgb(24, 24, 30)".into(),
            "rgb(30, 30, 36)".into(),
            "rgb(60, 55, 40)".into(),
        );
    };
    let is_light = get_luminance(r, g, b) > 0.5;
    if is_light {
        (
            adjust_brightness(base_color, 0.96),
            base_color.to_string(),
            format!(
                "rgb({}, {}, {})",
                (u16::from(r) + 10).min(255),
                (u16::from(g) + 5).min(255),
                u16::from(b).saturating_sub(20)
            ),
        )
    } else {
        (
            adjust_brightness(base_color, 0.7),
            adjust_brightness(base_color, 0.85),
            format!(
                "rgb({}, {}, {})",
                (u16::from(r) + 20).min(255),
                (u16::from(g) + 15).min(255),
                b
            ),
        )
    }
}

/// Generate CSS custom property declarations from theme colors.
fn generate_theme_vars(theme_name: Option<&str>) -> String {
    let colors = get_resolved_theme_colors(theme_name);
    let mut lines: Vec<String> = Vec::new();
    let mut keys: Vec<&String> = colors.keys().collect();
    keys.sort();
    for key in keys {
        lines.push(format!("--{key}: {};", colors[key]));
    }
    let theme_export = get_theme_export_colors(theme_name);
    let user_message_bg = colors
        .get("userMessageBg")
        .cloned()
        .unwrap_or_else(|| "#343541".into());
    let derived = derive_export_colors(&user_message_bg);
    lines.push(format!(
        "--exportPageBg: {};",
        theme_export.get("pageBg").unwrap_or(&derived.0)
    ));
    lines.push(format!(
        "--exportCardBg: {};",
        theme_export.get("cardBg").unwrap_or(&derived.1)
    ));
    lines.push(format!(
        "--exportInfoBg: {};",
        theme_export.get("infoBg").unwrap_or(&derived.2)
    ));
    lines.join("\n      ")
}

pub struct ExportOptions {
    pub output_path: Option<String>,
    pub theme_name: Option<String>,
}

/// Read a template asset from the JS source tree (repo-relative fallback).
fn read_asset(name: &str) -> String {
    let candidates = [
        // repo layout: packages/coding-agent/src/core/export-html/<name>
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("packages")
            .join("coding-agent")
            .join("src")
            .join("core")
            .join("export-html")
            .join(name),
    ];
    for candidate in &candidates {
        if candidate.exists() {
            return std::fs::read_to_string(candidate)
                .unwrap_or_else(|error| panic!("failed to read export template {name}: {error}"));
        }
    }
    panic!("export template asset not found: {name}");
}

struct SessionData {
    header: pi_protocol::Value,
    entries: Vec<pi_protocol::Value>,
    leaf_id: Option<String>,
    system_prompt: Option<String>,
    tools: Vec<pi_protocol::Value>,
}

/// Core HTML generation shared by both export functions.
fn generate_html(session_data: &SessionData, theme_name: Option<&str>) -> String {
    let template = read_asset("template.html");
    let template_css = read_asset("template.css");
    let template_js = read_asset("template.js");
    let marked_js = read_asset("vendor/marked.min.js");
    let hljs_js = read_asset("vendor/highlight.min.js");

    let theme_vars = generate_theme_vars(theme_name);
    let colors = get_resolved_theme_colors(theme_name);
    let theme_export = get_theme_export_colors(theme_name);
    let user_message_bg = colors.get("userMessageBg").cloned().unwrap_or_else(|| "#343541".into());
    let derived = derive_export_colors(&user_message_bg);
    let body_bg = theme_export.get("pageBg").unwrap_or(&derived.0);
    let container_bg = theme_export.get("cardBg").unwrap_or(&derived.1);
    let info_bg = theme_export.get("infoBg").unwrap_or(&derived.2);

    // Base64 encode session data to avoid escaping issues.
    let payload = pi_protocol::Value::Map(vec![
        ("header".into(), session_data.header.clone()),
        ("entries".into(), pi_protocol::Value::Array(session_data.entries.clone())),
        ("leafId".into(), session_data.leaf_id.clone().map_or(pi_protocol::Value::Null, pi_protocol::Value::String)),
        (
            "systemPrompt".into(),
            session_data.system_prompt.clone().map_or(pi_protocol::Value::Null, pi_protocol::Value::String),
        ),
        ("tools".into(), pi_protocol::Value::Array(session_data.tools.clone())),
    ]);
    use base64::Engine as _;
    let session_data_base64 = base64::engine::general_purpose::STANDARD
        .encode(pi_ai::utils::json::json_stringify(&payload));

    let css = template_css
        .replace("{{THEME_VARS}}", &theme_vars)
        .replace("{{BODY_BG}}", body_bg)
        .replace("{{CONTAINER_BG}}", container_bg)
        .replace("{{INFO_BG}}", info_bg);

    template
        .replace("{{CSS}}", &css)
        .replace("{{JS}}", &template_js)
        .replace("{{SESSION_DATA}}", &session_data_base64)
        .replace("{{MARKED_JS}}", &marked_js)
        .replace("{{HIGHLIGHT_JS}}", &hljs_js)
}

fn header_to_json(header: &super::session_types::SessionHeader) -> pi_protocol::Value {
    let mut fields = vec![
        ("id".into(), pi_protocol::Value::String(header.id.clone())),
        ("timestamp".into(), pi_protocol::Value::String(header.timestamp.clone())),
        ("cwd".into(), pi_protocol::Value::String(header.cwd.clone())),
    ];
    if let Some(version) = header.version {
        fields.push(("version".into(), pi_protocol::Value::Number(version as f64)));
    }
    if let Some(parent_session) = &header.parent_session {
        fields.push(("parentSession".into(), pi_protocol::Value::String(parent_session.clone())));
    }
    pi_protocol::Value::Map(fields)
}

fn entries_to_json(entries: &[super::session_types::SessionEntry]) -> Vec<pi_protocol::Value> {
    entries.iter().map(super::session_types::entry_to_json).collect()
}

/// Export the current session to HTML. Mirrors exportSessionToHtml.
pub fn export_session_to_html(
    sm: &SessionManager,
    system_prompt: Option<&str>,
    options: &ExportOptions,
) -> Result<String, String> {
    let session_file = sm.get_session_file().map(|s| s.to_string());
    let Some(session_file) = session_file else {
        return Err("Cannot export in-memory session to HTML".into());
    };
    if !std::path::Path::new(&session_file).exists() {
        return Err("Nothing to export yet - start a conversation first".into());
    }

    let entries = sm.get_entries();
    let header = sm.get_header().map(|header| header_to_json(&header)).unwrap_or(pi_protocol::Value::Null);

    let session_data = SessionData {
        header,
        entries: entries_to_json(&entries),
        leaf_id: sm.get_leaf_id(),
        system_prompt: system_prompt.map(|s| s.to_string()),
        tools: Vec::new(),
    };
    let html = generate_html(&session_data, options.theme_name.as_deref());

    let output_path = options
        .output_path
        .clone()
        .unwrap_or_else(|| {
            let basename = std::path::Path::new(&session_file)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "session".into());
            format!("{}-session-{basename}.html", crate::config::APP_NAME)
        });
    std::fs::write(&output_path, html).map_err(|error| error.to_string())?;
    Ok(output_path)
}

/// Export a session file to HTML (standalone). Mirrors exportFromFile.
pub fn export_from_file(input_path: &str, options: &ExportOptions) -> Result<String, String> {
    let resolved = super::session_paths::resolve_path(input_path, None);
    if !std::path::Path::new(&resolved).exists() {
        return Err(format!("File not found: {resolved}"));
    }
    let sm = SessionManager::open(&resolved, None, None);
    let header = sm.get_header().map(|header| header_to_json(&header));
    let session_data = SessionData {
        header: header.unwrap_or(pi_protocol::Value::Null),
        entries: entries_to_json(&sm.get_entries()),
        leaf_id: sm.get_leaf_id(),
        system_prompt: None,
        tools: Vec::new(),
    };
    let html = generate_html(&session_data, options.theme_name.as_deref());
    let output_path = options.output_path.clone().unwrap_or_else(|| {
        let basename = std::path::Path::new(&resolved)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "session".into());
        format!("{}-session-{basename}.html", crate::config::APP_NAME)
    });
    std::fs::write(&output_path, html).map_err(|error| error.to_string())?;
    Ok(output_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(parse_color("#1a2b3c"), Some((0x1a, 0x2b, 0x3c)));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some((1, 2, 3)));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#12"), None);
    }

    #[test]
    fn derive_colors_light_and_dark() {
        // Dark base: page darker than card.
        let (page, card, _) = derive_export_colors("#343541");
        let (pr, _, _) = parse_color(&page).unwrap();
        let (cr, _, _) = parse_color(&card).unwrap();
        assert!(pr < cr);
        // Light base: page slightly dimmer than card.
        let (page2, card2, _) = derive_export_colors("#f0f0f0");
        let (pr2, _, _) = parse_color(&page2).unwrap();
        let (cr2, _, _) = parse_color(&card2).unwrap();
        assert!(pr2 < cr2);
        assert!(cr2 >= 240);
    }

    #[test]
    fn generate_html_embeds_data() {
        let data = SessionData {
            header: pi_protocol::Value::Null,
            entries: vec![pi_protocol::Value::String("x".into())],
            leaf_id: Some("abc".into()),
            system_prompt: None,
            tools: Vec::new(),
        };
        let html = generate_html(&data, None);
        assert!(html.contains(r#"id="session-data""#));
        assert!(html.contains("--userMessageBg"));
        assert!(html.contains("--exportPageBg"));
        // Base64 payload is embedded (JS/TS runs of the same template get the
        // same placeholder replacement).
        assert!(html.contains("eyJ") || html.contains("e30"));
    }
}
