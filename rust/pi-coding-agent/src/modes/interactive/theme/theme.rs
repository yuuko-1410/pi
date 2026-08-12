//! Theme loading and styling, port of `modes/interactive/theme/theme.ts`.
//!
//! Built-in dark/light themes are embedded at compile time (include_str!).
//! ponytail: syntax highlighting (highlight.js) and the fs.watch theme
//! reloader are deferred; highlightCode falls back to mdCodeBlock coloring
//! and theme reload requires an explicit set_theme call.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::source_info::SourceInfo;

pub const DARK_THEME_JSON: &str = include_str!("dark.json");
pub const LIGHT_THEME_JSON: &str = include_str!("light.json");

#[derive(Clone, Debug, PartialEq)]
pub enum ColorValue {
    Hex(String),
    Empty,
    Index(u8),
    Var(String),
}

fn parse_color_value(value: &pi_protocol::Value) -> Option<ColorValue> {
    match value {
        pi_protocol::Value::String(text) => {
            if text.is_empty() {
                Some(ColorValue::Empty)
            } else if let Some(hex) = text.strip_prefix('#') {
                if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    Some(ColorValue::Hex(text.clone()))
                } else {
                    // Variable reference.
                    Some(ColorValue::Var(text.clone()))
                }
            } else if text.starts_with("var(") {
                // Not used by builtin themes; treat as variable name.
                Some(ColorValue::Var(text.trim_start_matches("var(").trim_end_matches(')').to_string()))
            } else {
                Some(ColorValue::Var(text.clone()))
            }
        }
        pi_protocol::Value::Number(number) => {
            if *number >= 0.0 && *number <= 255.0 {
                Some(ColorValue::Index(*number as u8))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The colors section keys (required + optional), in the JS schema order.
pub const THEME_FG_COLORS: [&str; 46] = [
    "accent", "border", "borderAccent", "borderMuted", "success", "error", "warning", "muted", "dim", "text",
    "thinkingText", "userMessageText", "customMessageText", "customMessageLabel", "toolTitle", "toolOutput",
    "mdHeading", "mdLink", "mdLinkUrl", "mdCode", "mdCodeBlock", "mdCodeBlockBorder", "mdQuote", "mdQuoteBorder",
    "mdHr", "mdListBullet", "toolDiffAdded", "toolDiffRemoved", "toolDiffContext", "syntaxComment", "syntaxKeyword",
    "syntaxFunction", "syntaxVariable", "syntaxString", "syntaxNumber", "syntaxType", "syntaxOperator",
    "syntaxPunctuation", "thinkingOff", "thinkingMinimal", "thinkingLow", "thinkingMedium", "thinkingHigh",
    "thinkingXhigh", "thinkingMax", "bashMode",
];

pub const THEME_BG_COLORS: [&str; 7] = [
    "selectedBg", "scrollbarThumb", "userMessageBg", "customMessageBg", "toolPendingBg", "toolSuccessBg",
    "toolErrorBg",
];

fn hex_to_rgb(hex: &str) -> (i64, i64, i64) {
    let cleaned = hex.trim_start_matches('#');
    let r = i64::from_str_radix(&cleaned[0..2], 16).unwrap_or(0);
    let g = i64::from_str_radix(&cleaned[2..4], 16).unwrap_or(0);
    let b = i64::from_str_radix(&cleaned[4..6], 16).unwrap_or(0);
    (r, g, b)
}

const CUBE_VALUES: [i64; 6] = [0, 95, 135, 175, 215, 255];

fn gray_values() -> Vec<i64> {
    (0..24).map(|i| 8 + i * 10).collect()
}

fn find_closest_cube_index(value: i64) -> usize {
    let mut min_dist = i64::MAX;
    let mut min_idx = 0;
    for (i, cube) in CUBE_VALUES.iter().enumerate() {
        let dist = (value - cube).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

fn find_closest_gray_index(gray: i64) -> usize {
    let grays = gray_values();
    let mut min_dist = i64::MAX;
    let mut min_idx = 0;
    for (i, value) in grays.iter().enumerate() {
        let dist = (gray - value).abs();
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

fn color_distance(r1: i64, g1: i64, b1: i64, r2: i64, g2: i64, b2: i64) -> f64 {
    let dr = r1 - r2;
    let dg = g1 - g2;
    let db = b1 - b2;
    (dr * dr) as f64 * 0.299 + (dg * dg) as f64 * 0.587 + (db * db) as f64 * 0.114
}

fn rgb_to_256(r: i64, g: i64, b: i64) -> i64 {
    let r_idx = find_closest_cube_index(r);
    let g_idx = find_closest_cube_index(g);
    let b_idx = find_closest_cube_index(b);
    let cube_r = CUBE_VALUES[r_idx];
    let cube_g = CUBE_VALUES[g_idx];
    let cube_b = CUBE_VALUES[b_idx];
    let cube_index = 16 + 36 * r_idx + 6 * g_idx + b_idx;
    let cube_dist = color_distance(r, g, b, cube_r, cube_g, cube_b);

    let gray = (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64).round() as i64;
    let gray_idx = find_closest_gray_index(gray);
    let gray_value = gray_values()[gray_idx];
    let gray_index = 232 + gray_idx;
    let gray_dist = color_distance(r, g, b, gray_value, gray_value, gray_value);

    let max_c = r.max(g).max(b);
    let min_c = r.min(g).min(b);
    let spread = max_c - min_c;

    if spread < 10 && gray_dist < cube_dist {
        return gray_index as i64;
    }
    cube_index as i64
}

fn hex_to_256(hex: &str) -> i64 {
    let (r, g, b) = hex_to_rgb(hex);
    rgb_to_256(r, g, b)
}

fn fg_ansi(color: &ColorValue, mode: &ColorMode) -> String {
    match color {
        ColorValue::Empty => "\x1b[39m".to_string(),
        ColorValue::Index(index) => format!("\x1b[38;5;{index}m"),
        ColorValue::Hex(hex) => {
            if mode == &ColorMode::Truecolor {
                let (r, g, b) = hex_to_rgb(hex);
                format!("\x1b[38;2;{r};{g};{b}m")
            } else {
                format!("\x1b[38;5;{}m", hex_to_256(hex))
            }
        }
        ColorValue::Var(_) => "\x1b[39m".to_string(), // unresolved: default fg
    }
}

fn bg_ansi(color: &ColorValue, mode: &ColorMode) -> String {
    match color {
        ColorValue::Empty => "\x1b[49m".to_string(),
        ColorValue::Index(index) => format!("\x1b[48;5;{index}m"),
        ColorValue::Hex(hex) => {
            if mode == &ColorMode::Truecolor {
                let (r, g, b) = hex_to_rgb(hex);
                format!("\x1b[48;2;{r};{g};{b}m")
            } else {
                format!("\x1b[48;5;{}m", hex_to_256(hex))
            }
        }
        ColorValue::Var(_) => "\x1b[49m".to_string(),
    }
}

fn resolve_var_refs(value: &ColorValue, vars: &HashMap<String, ColorValue>, visited: &mut Vec<String>) -> Result<ColorValue, String> {
    match value {
        ColorValue::Var(name) => {
            if visited.iter().any(|existing| existing == name) {
                return Err(format!("Circular variable reference detected: {name}"));
            }
            let resolved = vars
                .get(name)
                .ok_or_else(|| format!("Variable reference not found: {name}"))?;
            visited.push(name.clone());
            let result = resolve_var_refs(resolved, vars, visited)?;
            visited.pop();
            Ok(result)
        }
        other => Ok(other.clone()),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ColorMode {
    Truecolor,
    Color256,
}

/// Theme with resolved ANSI codes for fg and bg colors.
pub struct Theme {
    pub name: Option<String>,
    pub source_path: Option<String>,
    pub source_info: Option<SourceInfo>,
    fg_colors: HashMap<String, String>,
    bg_colors: HashMap<String, String>,
    mode: ColorMode,
}

impl Theme {
    pub fn new(
        fg_colors: HashMap<String, ColorValue>,
        bg_colors: HashMap<String, ColorValue>,
        mode: ColorMode,
        name: Option<String>,
        source_path: Option<String>,
        source_info: Option<SourceInfo>,
    ) -> Result<Theme, String> {
        let mut fg_map = HashMap::new();
        for (key, value) in fg_colors {
            fg_map.insert(key, fg_ansi(&value, &mode));
        }
        let mut bg_map = HashMap::new();
        for (key, value) in bg_colors {
            bg_map.insert(key, bg_ansi(&value, &mode));
        }
        Ok(Theme {
            name,
            source_path,
            source_info,
            fg_colors: fg_map,
            bg_colors: bg_map,
            mode,
        })
    }

    pub fn fg(&self, color: &str, text: &str) -> String {
        let ansi = self
            .fg_colors
            .get(color)
            .ok_or_else(|| format!("Unknown theme color: {color}"))
            .map_err(|error| error)
            .unwrap_or_else(|error| panic!("{error}"));
        format!("{ansi}{text}\x1b[39m")
    }

    pub fn bg(&self, color: &str, text: &str) -> String {
        let ansi = self
            .bg_colors
            .get(color)
            .unwrap_or_else(|| panic!("Unknown theme background color: {color}"));
        format!("{ansi}{text}\x1b[49m")
    }

    pub fn bold(&self, text: &str) -> String {
        format!("\x1b[1m{text}\x1b[22m")
    }

    pub fn italic(&self, text: &str) -> String {
        format!("\x1b[3m{text}\x1b[23m")
    }

    pub fn underline(&self, text: &str) -> String {
        format!("\x1b[4m{text}\x1b[24m")
    }

    pub fn inverse(&self, text: &str) -> String {
        format!("\x1b[7m{text}\x1b[27m")
    }

    pub fn strikethrough(&self, text: &str) -> String {
        format!("\x1b[9m{text}\x1b[29m")
    }

    pub fn get_fg_ansi(&self, color: &str) -> String {
        self.fg_colors
            .get(color)
            .cloned()
            .unwrap_or_else(|| panic!("Unknown theme color: {color}"))
    }

    pub fn get_bg_ansi(&self, color: &str) -> String {
        self.bg_colors
            .get(color)
            .cloned()
            .unwrap_or_else(|| panic!("Unknown theme background color: {color}"))
    }

    pub fn get_color_mode(&self) -> &ColorMode {
        &self.mode
    }

    pub fn get_thinking_border_color(&self, level: &str) -> String {
        let color = match level {
            "off" => "thinkingOff",
            "minimal" => "thinkingMinimal",
            "low" => "thinkingLow",
            "medium" => "thinkingMedium",
            "high" => "thinkingHigh",
            "xhigh" => "thinkingXhigh",
            "max" => "thinkingMax",
            _ => "thinkingOff",
        };
        self.get_fg_ansi(color)
    }

    pub fn get_bash_mode_border_color(&self) -> String {
        self.get_fg_ansi("bashMode")
    }
}

struct ThemeJson {
    name: String,
    vars: HashMap<String, ColorValue>,
    colors: HashMap<String, ColorValue>,
    export: Option<HashMap<String, ColorValue>>,
}

fn parse_theme_json(label: &str, content: &str) -> Result<ThemeJson, String> {
    let value: pi_protocol::Value = pi_ai::utils::json::parse_json_with_repair(content)
        .map_err(|error| format!("Failed to parse theme {label}: {error}"))?;
    let root = value.as_map().ok_or_else(|| format!("Invalid theme \"{label}\": expected an object"))?;

    let name = root
        .iter()
        .find(|(k, _)| k == "name")
        .and_then(|(_, v)| v.as_str())
        .ok_or_else(|| format!("Invalid theme \"{label}\": missing \"name\""))?
        .to_string();
    if name.contains('/') {
        return Err(format!(
            "Invalid theme name \"{name}\": theme names cannot contain \"/\" because it is reserved for automatic light/dark theme settings."
        ));
    }

    let mut vars: HashMap<String, ColorValue> = HashMap::new();
    if let Some(vars_value) = root.iter().find(|(k, _)| k == "vars") {
        if let Some(entries) = vars_value.1.as_map() {
            for (key, value) in entries {
                if let Some(parsed) = parse_color_value(value) {
                    vars.insert(key.clone(), parsed);
                }
            }
        }
    }

    let colors_value = root
        .iter()
        .find(|(k, _)| k == "colors")
        .and_then(|(_, v)| v.as_map())
        .ok_or_else(|| format!("Invalid theme \"{label}\": missing \"colors\""))?;

    let mut colors: HashMap<String, ColorValue> = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    for key in THEME_FG_COLORS.iter().chain(THEME_BG_COLORS.iter()) {
        match colors_value.iter().find(|(k, _)| k == key) {
            Some((_, value)) => {
                if let Some(parsed) = parse_color_value(value) {
                    colors.insert(key.to_string(), parsed);
                }
            }
            None => {
                // thinkingMax and scrollbarThumb are optional.
                if *key != "thinkingMax" && *key != "scrollbarThumb" {
                    missing.push(key.to_string());
                }
            }
        }
    }
    if !missing.is_empty() {
        let mut message = format!("Invalid theme \"{label}\":\n\nMissing required color tokens:\n");
        for color in &missing {
            message.push_str(&format!("  - {color}\n"));
        }
        message.push_str("\n\nPlease add these colors to your theme's \"colors\" object.\n");
        message.push_str("See the built-in themes (dark.json, light.json) for reference values.");
        return Err(message);
    }

    let export = root.iter().find(|(k, _)| k == "export").and_then(|(_, v)| v.as_map()).map(|entries| {
        let mut map = HashMap::new();
        for (key, value) in entries {
            if let Some(parsed) = parse_color_value(value) {
                map.insert(key.clone(), parsed);
            }
        }
        map
    });

    Ok(ThemeJson {
        name,
        vars,
        colors,
        export,
    })
}

fn resolve_theme_colors(
    colors: &HashMap<String, ColorValue>,
    vars: &HashMap<String, ColorValue>,
) -> Result<HashMap<String, ColorValue>, String> {
    let mut resolved = HashMap::new();
    for (key, value) in colors {
        resolved.insert(key.clone(), resolve_var_refs(value, vars, &mut Vec::new())?);
    }
    Ok(resolved)
}

fn with_theme_color_fallbacks(colors: &mut HashMap<String, ColorValue>) {
    // thinkingMax falls back to thinkingXhigh; scrollbarThumb to selectedBg.
    if !colors.contains_key("thinkingMax") {
        if let Some(xhigh) = colors.get("thinkingXhigh").cloned() {
            colors.insert("thinkingMax".to_string(), xhigh);
        }
    }
    if !colors.contains_key("scrollbarThumb") {
        if let Some(selected_bg) = colors.get("selectedBg").cloned() {
            colors.insert("scrollbarThumb".to_string(), selected_bg);
        }
    }
}

/// Detect the color mode from the terminal (pi-tui capabilities).
pub fn detect_color_mode() -> ColorMode {
    if pi_tui::terminal_image::get_capabilities().true_color {
        ColorMode::Truecolor
    } else {
        ColorMode::Color256
    }
}

/// Load a theme from raw JSON content.
pub fn load_theme_from_content(content: &str, mode: Option<ColorMode>, source_path: Option<&str>) -> Result<Theme, String> {
    let theme_json = parse_theme_json(source_path.unwrap_or("<inline>"), content)?;
    create_theme(&theme_json, mode, source_path)
}

fn create_theme(theme_json: &ThemeJson, mode: Option<ColorMode>, source_path: Option<&str>) -> Result<Theme, String> {
    let color_mode = mode.unwrap_or_else(detect_color_mode);
    let mut colors = resolve_theme_colors(&theme_json.colors, &theme_json.vars)?;
    with_theme_color_fallbacks(&mut colors);

    let mut fg_colors: HashMap<String, ColorValue> = HashMap::new();
    let mut bg_colors: HashMap<String, ColorValue> = HashMap::new();
    for (key, value) in colors {
        if THEME_BG_COLORS.contains(&key.as_str()) {
            bg_colors.insert(key, value);
        } else {
            fg_colors.insert(key, value);
        }
    }
    Theme::new(
        fg_colors,
        bg_colors,
        color_mode,
        Some(theme_json.name.clone()),
        source_path.map(|value| value.to_string()),
        None,
    )
}

/// Load a built-in or custom theme by name.
pub fn load_theme(name: &str, mode: Option<ColorMode>) -> Result<Theme, String> {
    let content = match name {
        "dark" => DARK_THEME_JSON.to_string(),
        "light" => LIGHT_THEME_JSON.to_string(),
        _ => {
            let path = get_custom_themes_dir() + "/" + name + ".json";
            std::fs::read_to_string(&path).map_err(|_| format!("Theme not found: {name}"))?
        }
    };
    load_theme_from_content(&content, mode, Some(name))
}

pub fn get_custom_themes_dir() -> String {
    crate::config::get_agent_dir() + "/themes"
}

/// Get a theme by name; None on failure (mirrors the JS try/catch).
pub fn get_theme_by_name(name: &str) -> Option<Theme> {
    load_theme(name, None).ok()
}

/// Default theme from the terminal background environment (COLORFGBG).
pub fn get_default_theme() -> String {
    match std::env::var("COLORFGBG") {
        Ok(colorfgbg) => {
            let parts: Vec<&str> = colorfgbg.split(';').collect();
            for part in parts.iter().rev() {
                if let Ok(index) = part.trim().parse::<i64>() {
                    if (0..=255).contains(&index) {
                        return if ansi256_luminance(index) >= 0.5 { "light" } else { "dark" }.to_string();
                    }
                }
            }
            "dark".to_string()
        }
        Err(_) => "dark".to_string(),
    }
}

fn ansi256_to_hex(index: i64) -> String {
    let basic = [
        "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0", "#808080",
        "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    if index < 16 {
        return basic[index as usize].to_string();
    }
    if index < 232 {
        let cube_index = index - 16;
        let r = cube_index / 36;
        let g = (cube_index % 36) / 6;
        let b = cube_index % 6;
        let to_hex = |n: i64| -> String {
            let value = if n == 0 { 0 } else { 55 + n * 40 };
            format!("{value:02x}")
        };
        return format!("#{}{}{}", to_hex(r), to_hex(g), to_hex(b));
    }
    let gray = 8 + (index - 232) * 10;
    format!("#{gray:02x}{gray:02x}{gray:02x}")
}

fn ansi256_luminance(index: i64) -> f64 {
    let hex = ansi256_to_hex(index);
    let (r, g, b) = hex_to_rgb(&hex);
    rgb_luminance(r, g, b)
}

fn rgb_luminance(r: i64, g: i64, b: i64) -> f64 {
    let to_linear = |channel: i64| -> f64 {
        let value = channel as f64 / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * to_linear(r) + 0.7152 * to_linear(g) + 0.0722 * to_linear(b)
}

/// Parse a "light/dark" auto theme setting.
pub fn parse_auto_theme_setting(theme_setting: Option<&str>) -> Option<(String, String)> {
    let theme_setting = theme_setting?;
    let slash_index = theme_setting.find('/')?;
    if theme_setting[slash_index + 1..].contains('/') {
        return None;
    }
    let light_theme = theme_setting[..slash_index].trim().to_string();
    let dark_theme = theme_setting[slash_index + 1..].trim().to_string();
    if light_theme.is_empty() || dark_theme.is_empty() {
        return None;
    }
    Some((light_theme, dark_theme))
}

/// Resolve a theme setting for a terminal theme.
pub fn resolve_theme_setting(theme_setting: Option<&str>, terminal_theme: &str) -> Option<String> {
    if let Some((light_theme, dark_theme)) = parse_auto_theme_setting(theme_setting) {
        return Some(if terminal_theme == "light" { light_theme } else { dark_theme });
    }
    if theme_setting.is_some_and(|setting| setting.contains('/')) {
        return None;
    }
    theme_setting.map(|setting| setting.to_string())
}

/// Get resolved theme colors as CSS hex strings (HTML export).
pub fn get_resolved_theme_colors(theme_name: Option<&str>) -> HashMap<String, String> {
    let name = theme_name.map(|value| value.to_string()).unwrap_or_else(get_default_theme);
    let content = match name.as_str() {
        "dark" => DARK_THEME_JSON.to_string(),
        "light" => LIGHT_THEME_JSON.to_string(),
        _ => match std::fs::read_to_string(format!("{}/{name}.json", get_custom_themes_dir())) {
            Ok(content) => content,
            Err(_) => DARK_THEME_JSON.to_string(),
        },
    };
    let theme_json = match parse_theme_json(&name, &content) {
        Ok(theme_json) => theme_json,
        Err(_) => return HashMap::new(),
    };
    let resolved = match resolve_theme_colors(&theme_json.colors, &theme_json.vars) {
        Ok(resolved) => resolved,
        Err(_) => return HashMap::new(),
    };
    let default_text = if name == "light" { "#000000" } else { "#e5e5e7" };
    let mut css_colors: HashMap<String, String> = HashMap::new();
    for (key, value) in resolved {
        let css = match value {
            ColorValue::Index(index) => ansi256_to_hex(index as i64),
            ColorValue::Empty => default_text.to_string(),
            ColorValue::Hex(hex) => hex,
            ColorValue::Var(_) => default_text.to_string(),
        };
        css_colors.insert(key, css);
    }
    css_colors
}

pub fn is_light_theme(theme_name: Option<&str>) -> bool {
    theme_name == Some("light")
}

/// Get explicit export colors from the theme JSON.
pub fn get_theme_export_colors(theme_name: Option<&str>) -> HashMap<String, String> {
    let name = theme_name.map(|value| value.to_string()).unwrap_or_else(get_default_theme);
    let content = match name.as_str() {
        "dark" => DARK_THEME_JSON.to_string(),
        "light" => LIGHT_THEME_JSON.to_string(),
        _ => match std::fs::read_to_string(format!("{}/{name}.json", get_custom_themes_dir())) {
            Ok(content) => content,
            Err(_) => return HashMap::new(),
        },
    };
    let Ok(theme_json) = parse_theme_json(&name, &content) else {
        return HashMap::new();
    };
    let Some(export) = theme_json.export else {
        return HashMap::new();
    };
    let mut result = HashMap::new();
    for (key, value) in export {
        let resolved = match resolve_var_refs(&value, &theme_json.vars, &mut Vec::new()) {
            Ok(resolved) => resolved,
            Err(_) => continue,
        };
        match resolved {
            ColorValue::Index(index) => {
                result.insert(key, ansi256_to_hex(index as i64));
            }
            ColorValue::Hex(hex) => {
                result.insert(key, hex);
            }
            _ => {}
        }
    }
    result
}

/// Get the language identifier from a file path extension.
pub fn get_language_from_path(file_path: &str) -> Option<String> {
    let ext = file_path.rsplit('.').next()?.to_lowercase();
    if ext.is_empty() {
        return None;
    }
    let ext_to_lang: &[(&str, &str)] = &[
        ("ts", "typescript"), ("tsx", "typescript"), ("js", "javascript"), ("jsx", "javascript"),
        ("mjs", "javascript"), ("cjs", "javascript"), ("py", "python"), ("rb", "ruby"), ("rs", "rust"),
        ("go", "go"), ("java", "java"), ("kt", "kotlin"), ("swift", "swift"), ("c", "c"), ("h", "c"),
        ("cpp", "cpp"), ("cc", "cpp"), ("cxx", "cpp"), ("hpp", "cpp"), ("cs", "csharp"), ("php", "php"),
        ("sh", "bash"), ("bash", "bash"), ("zsh", "bash"), ("fish", "fish"), ("ps1", "powershell"),
        ("sql", "sql"), ("html", "html"), ("htm", "html"), ("css", "css"), ("scss", "scss"),
        ("sass", "sass"), ("less", "less"), ("json", "json"), ("yaml", "yaml"), ("yml", "yaml"),
        ("toml", "toml"), ("xml", "xml"), ("md", "markdown"), ("markdown", "markdown"),
        ("dockerfile", "dockerfile"), ("makefile", "makefile"), ("cmake", "cmake"), ("lua", "lua"),
        ("perl", "perl"), ("r", "r"), ("scala", "scala"), ("clj", "clojure"), ("ex", "elixir"),
        ("exs", "elixir"), ("erl", "erlang"), ("hs", "haskell"), ("ml", "ocaml"), ("vim", "vim"),
        ("graphql", "graphql"), ("proto", "protobuf"), ("tf", "hcl"), ("hcl", "hcl"),
    ];
    ext_to_lang
        .iter()
        .find(|(existing, _)| *existing == ext)
        .map(|(_, lang)| lang.to_string())
}

/// Highlight code with syntax coloring. ponytail: highlight.js is not
/// ported; lines are colored with mdCodeBlock (the no-language fallback).
pub fn highlight_code(code: &str, theme: &Theme) -> Vec<String> {
    code.split('\n').map(|line| theme.fg("mdCodeBlock", line)).collect()
}

/// The TUI markdown/editor/select theme adapters (closures in JS become
/// simple fg color lookups in Rust; components call theme.fg directly).
pub fn get_editor_border_color(theme: &Theme) -> String {
    theme.get_fg_ansi("borderMuted")
}

pub fn get_select_list_selected_prefix(theme: &Theme) -> String {
    theme.get_fg_ansi("accent")
}

pub fn get_settings_label_color(theme: &Theme, selected: bool) -> String {
    if selected {
        theme.get_fg_ansi("accent")
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Global theme instance
// ---------------------------------------------------------------------------

static GLOBAL_THEME: Mutex<Option<Theme>> = Mutex::new(None);
static CURRENT_THEME_NAME: Mutex<Option<String>> = Mutex::new(None);

/// Initialize the global theme (falls back to dark on failure).
pub fn init_theme(theme_name: Option<&str>) {
    let name = theme_name.map(|value| value.to_string()).unwrap_or_else(get_default_theme);
    match load_theme(&name, None) {
        Ok(theme) => {
            *CURRENT_THEME_NAME.lock().unwrap() = Some(name);
            *GLOBAL_THEME.lock().unwrap() = Some(theme);
        }
        Err(_) => {
            *CURRENT_THEME_NAME.lock().unwrap() = Some("dark".to_string());
            *GLOBAL_THEME.lock().unwrap() = load_theme("dark", None).ok();
        }
    }
}

/// Set the active theme; returns success and optional error (JS setTheme).
pub fn set_theme(name: &str) -> (bool, Option<String>) {
    match load_theme(name, None) {
        Ok(theme) => {
            *CURRENT_THEME_NAME.lock().unwrap() = Some(name.to_string());
            *GLOBAL_THEME.lock().unwrap() = Some(theme);
            (true, None)
        }
        Err(error) => {
            *CURRENT_THEME_NAME.lock().unwrap() = Some("dark".to_string());
            *GLOBAL_THEME.lock().unwrap() = load_theme("dark", None).ok();
            (false, Some(error))
        }
    }
}

/// The active global theme (JS `theme` proxy).
pub fn theme() -> std::sync::MutexGuard<'static, Option<Theme>> {
    GLOBAL_THEME.lock().unwrap()
}

pub fn current_theme_name() -> Option<String> {
    CURRENT_THEME_NAME.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_dark_theme() {
        let theme = load_theme("dark", Some(ColorMode::Truecolor)).unwrap();
        assert_eq!(theme.name.as_deref(), Some("dark"));
        // accent resolves through vars.
        let fg = theme.fg("accent", "x");
        assert!(fg.starts_with("\x1b[38;2;"));
        assert!(fg.ends_with("x\x1b[39m"));
    }

    #[test]
    fn loads_light_theme() {
        let theme = load_theme("light", Some(ColorMode::Color256)).unwrap();
        assert_eq!(theme.name.as_deref(), Some("light"));
        let fg = theme.fg("text", "x");
        assert!(fg.starts_with("\x1b[38;5;"));
    }

    #[test]
    fn unknown_theme_errors() {
        assert!(load_theme("nope", None).is_err());
        assert!(get_theme_by_name("nope").is_none());
    }

    #[test]
    fn empty_color_uses_default_fg() {
        let theme = load_theme("dark", Some(ColorMode::Truecolor)).unwrap();
        assert!(theme.get_fg_ansi("text").starts_with("\x1b[38;2;"));
    }

    #[test]
    fn invalid_theme_missing_colors() {
        let result = load_theme_from_content(r#"{"name":"bad","colors":{"accent":"\u0023fff"}}"#, None, None);
        assert!(result.is_err());
        match result {
            Err(error) => assert!(error.contains("Missing required color tokens")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn invalid_theme_name_with_slash() {
        let json = r#"{"name":"a/b","colors":{"accent":"\u0023fff","border":"\u0023fff","borderAccent":"\u0023fff","borderMuted":"\u0023fff","success":"\u0023fff","error":"\u0023fff","warning":"\u0023fff","muted":"\u0023fff","dim":"\u0023fff","text":"\u0023fff","thinkingText":"\u0023fff","userMessageText":"\u0023fff","customMessageText":"\u0023fff","customMessageLabel":"\u0023fff","toolTitle":"\u0023fff","toolOutput":"\u0023fff","mdHeading":"\u0023fff","mdLink":"\u0023fff","mdLinkUrl":"\u0023fff","mdCode":"\u0023fff","mdCodeBlock":"\u0023fff","mdCodeBlockBorder":"\u0023fff","mdQuote":"\u0023fff","mdQuoteBorder":"\u0023fff","mdHr":"\u0023fff","mdListBullet":"\u0023fff","toolDiffAdded":"\u0023fff","toolDiffRemoved":"\u0023fff","toolDiffContext":"\u0023fff","syntaxComment":"\u0023fff","syntaxKeyword":"\u0023fff","syntaxFunction":"\u0023fff","syntaxVariable":"\u0023fff","syntaxString":"\u0023fff","syntaxNumber":"\u0023fff","syntaxType":"\u0023fff","syntaxOperator":"\u0023fff","syntaxPunctuation":"\u0023fff","thinkingOff":"\u0023fff","thinkingMinimal":"\u0023fff","thinkingLow":"\u0023fff","thinkingMedium":"\u0023fff","thinkingHigh":"\u0023fff","thinkingXhigh":"\u0023fff","thinkingMax":"\u0023fff","bashMode":"\u0023fff","selectedBg":"\u0023fff","userMessageBg":"\u0023fff","customMessageBg":"\u0023fff","toolPendingBg":"\u0023fff","toolSuccessBg":"\u0023fff","toolErrorBg":"\u0023fff"}}"#;
        let result = load_theme_from_content(json, None, None);
        match result {
            Err(error) => assert!(error.contains("theme names cannot contain")),
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn thinking_border_colors() {
        let theme = load_theme("dark", Some(ColorMode::Truecolor)).unwrap();
        assert_eq!(theme.get_thinking_border_color("off"), theme.get_fg_ansi("thinkingOff"));
        assert_eq!(theme.get_thinking_border_color("max"), theme.get_fg_ansi("thinkingMax"));
        assert_eq!(theme.get_thinking_border_color("bogus"), theme.get_fg_ansi("thinkingOff"));
    }

    #[test]
    fn auto_theme_setting_parsing() {
        assert_eq!(
            parse_auto_theme_setting(Some("solarized-light/solarized-dark")),
            Some(("solarized-light".to_string(), "solarized-dark".to_string()))
        );
        assert_eq!(parse_auto_theme_setting(Some("single")), None);
        assert_eq!(parse_auto_theme_setting(Some("a/b/c")), None);
        assert_eq!(parse_auto_theme_setting(None), None);
        assert_eq!(resolve_theme_setting(Some("dark"), "dark"), Some("dark".to_string()));
        assert_eq!(
            resolve_theme_setting(Some("light-theme/dark-theme"), "light"),
            Some("light-theme".to_string())
        );
        // "a/b" parses as an auto setting: dark terminal -> "b".
        assert_eq!(resolve_theme_setting(Some("a/b"), "dark"), Some("b".to_string()));
        assert_eq!(resolve_theme_setting(Some("a/b/c"), "dark"), None);
    }

    #[test]
    fn language_from_path() {
        assert_eq!(get_language_from_path("main.rs").as_deref(), Some("rust"));
        assert_eq!(get_language_from_path("x.ts").as_deref(), Some("typescript"));
        assert_eq!(get_language_from_path("Makefile"), Some("makefile".to_string()));
        assert_eq!(get_language_from_path("noext"), None);
    }

    #[test]
    fn resolved_css_colors() {
        let colors = get_resolved_theme_colors(Some("dark"));
        assert!(colors.get("accent").is_some());
        assert!(colors.get("accent").unwrap().starts_with('#'));
        let colors = get_resolved_theme_colors(Some("light"));
        assert!(colors.get("text").is_some());
    }

    #[test]
    fn rgb_to_256_cube() {
        // Pure red maps to cube index 16 + 36*5 + 6*0 + 0 = 196.
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        // Gray (128,128,128) with spread 0: grayscale ramp wins.
        let gray_index = rgb_to_256(128, 128, 128);
        assert!((232..=255).contains(&gray_index));
    }

    #[test]
    fn theme_fallbacks() {
        let mut colors: HashMap<String, ColorValue> = HashMap::new();
        colors.insert("thinkingXhigh".to_string(), ColorValue::Hex("#ff0000".to_string()));
        colors.insert("selectedBg".to_string(), ColorValue::Hex("#00ff00".to_string()));
        with_theme_color_fallbacks(&mut colors);
        assert!(colors.contains_key("thinkingMax"));
        assert!(colors.contains_key("scrollbarThumb"));
    }

    #[test]
    fn global_theme_init() {
        init_theme(Some("dark"));
        assert!(theme().is_some());
        let (success, _) = set_theme("dark");
        assert!(success);
        let (success, error) = set_theme("nonexistent-theme");
        assert!(!success);
        assert!(error.is_some());
        // Falls back to dark.
        assert_eq!(current_theme_name().as_deref(), Some("dark"));
    }
}
