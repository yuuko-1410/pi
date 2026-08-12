//! Terminal colors, port of `packages/tui/src/terminal-colors.ts`.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub type TerminalColorScheme = &'static str; // "dark" | "light"

fn hex_to_rgb(hex: &str) -> RgbColor {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    let r = u8::from_str_radix(&normalized[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&normalized[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&normalized[4..6], 16).unwrap_or(0);
    RgbColor { r, g, b }
}

fn parse_osc_hex_channel(channel: &str) -> Option<u8> {
    if channel.is_empty() || !channel.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let max = 16usize.pow(channel.len() as u32) - 1;
    if max <= 0 {
        return None;
    }
    let value = u64::from_str_radix(channel, 16).ok()?;
    Some(((value as f64 / max as f64) * 255.0).round() as u8)
}

const OSC11_BACKGROUND_COLOR_RESPONSE_PREFIX: &str = "\x1b]11;";

/// True when data is an OSC 11 background color response
/// (`ESC ] 11 ; ... BEL` or `ESC ] 11 ; ... ESC \`).
pub fn is_osc11_background_color_response(data: &str) -> bool {
    if !data.starts_with(OSC11_BACKGROUND_COLOR_RESPONSE_PREFIX) {
        return false;
    }
    let rest = &data[OSC11_BACKGROUND_COLOR_RESPONSE_PREFIX.len()..];
    rest.ends_with('\x07') || rest.ends_with("\x1b\\")
}

pub fn parse_osc11_background_color(data: &str) -> Option<RgbColor> {
    if !is_osc11_background_color_response(data) {
        return None;
    }
    let value_start = OSC11_BACKGROUND_COLOR_RESPONSE_PREFIX.len();
    let value_end = data
        .find('\x07')
        .or_else(|| data.find("\x1b\\"))
        .unwrap_or(data.len());
    let value = data[value_start..value_end].trim();

    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hex_to_rgb(hex));
        }
        if hex.len() == 12 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let r = parse_osc_hex_channel(&hex[0..4]);
            let g = parse_osc_hex_channel(&hex[4..8]);
            let b = parse_osc_hex_channel(&hex[8..12]);
            return match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Some(RgbColor { r, g, b }),
                _ => None,
            };
        }
        return None;
    }

    let rgb_value = value
        .strip_prefix("rgba:")
        .or_else(|| value.strip_prefix("rgb:"))
        .unwrap_or(value);
    let mut channels = rgb_value.split('/');
    let red = channels.next();
    let green = channels.next();
    let blue = channels.next();
    match (red, green, blue) {
        (Some(red), Some(green), Some(blue)) => {
            let r = parse_osc_hex_channel(red);
            let g = parse_osc_hex_channel(green);
            let b = parse_osc_hex_channel(blue);
            match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Some(RgbColor { r, g, b }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parse a terminal color scheme report (`ESC [ ? 997 ; 1 n` repeated):
/// `1` = dark, `2` = light.
pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    // Pattern: ^(?:\x1b\[\?997;(1|2)n)+$
    if data.is_empty() {
        return None;
    }
    let mut rest = data;
    let mut first: Option<char> = None;
    while !rest.is_empty() {
        let prefix = "\x1b[?997;";
        let Some(tail) = rest.strip_prefix(prefix) else {
            return None;
        };
        let Some(value) = tail.chars().next() else {
            return None;
        };
        if value != '1' && value != '2' {
            return None;
        }
        let Some(tail) = tail.strip_prefix(value).and_then(|t| t.strip_prefix('n')) else {
            return None;
        };
        if first.is_none() {
            first = Some(value);
        }
        rest = tail;
    }
    match first {
        Some('2') => Some("light"),
        Some(_) => Some("dark"),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_osc11_responses() {
        assert!(is_osc11_background_color_response("\x1b]11;rgb:1a/1b/1c\x07"));
        assert!(is_osc11_background_color_response("\x1b]11;#112233\x1b\\"));
        assert!(!is_osc11_background_color_response("hello"));
        assert!(!is_osc11_background_color_response("\x1b]10;rgb:1/2/3\x07"));
    }

    #[test]
    fn parses_hex_background() {
        let color = parse_osc11_background_color("\x1b]11;#336699\x07").unwrap();
        assert_eq!(color, RgbColor { r: 0x33, g: 0x66, b: 0x99 });
    }

    #[test]
    fn parses_hex16_background() {
        let color = parse_osc11_background_color("\x1b]11;#333366669999\x07").unwrap();
        assert_eq!(color, RgbColor { r: 0x33, g: 0x66, b: 0x99 });
    }

    #[test]
    fn parses_rgb_background() {
        let color = parse_osc11_background_color("\x1b]11;rgb:1a/2b/3c\x07").unwrap();
        assert_eq!(color, RgbColor { r: 0x1a, g: 0x2b, b: 0x3c });
    }

    #[test]
    fn rejects_invalid_background() {
        assert_eq!(parse_osc11_background_color("nope"), None);
        assert_eq!(parse_osc11_background_color("\x1b]11;rgb:1a/2b\x07"), None);
        assert_eq!(parse_osc11_background_color("\x1b]11;#zzzzzz\x07"), None);
    }

    #[test]
    fn parses_color_scheme_report() {
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;1n"), Some("dark"));
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;2n"), Some("light"));
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;1n\x1b[?997;1n"), Some("dark"));
        assert_eq!(parse_terminal_color_scheme_report("junk"), None);
        assert_eq!(parse_terminal_color_scheme_report(""), None);
    }
}
