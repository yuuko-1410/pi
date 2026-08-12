//! Armin easter egg, port of `components/armin.ts`.
//!
//! ponytail: the JS version animates via setInterval with several reveal
//! effects. The Rust version renders the final grid immediately (no
//! animation loop); the effect is deterministic.

use pi_tui::tui::Component;

use crate::modes::interactive::theme::theme::theme;

const WIDTH: usize = 31;
const HEIGHT: usize = 36;
const BITS: [u8; 144] = [
    0xff, 0xff, 0xff, 0x7f, 0xff, 0xf0, 0xff, 0x7f, 0xff, 0xed, 0xff, 0x7f, 0xff, 0xdb, 0xff, 0x7f, 0xff, 0xb7, 0xff,
    0x7f, 0xff, 0x77, 0xfe, 0x7f, 0x3f, 0xf8, 0xfe, 0x7f, 0xdf, 0xff, 0xfe, 0x7f, 0xdf, 0x3f, 0xfc, 0x7f, 0x9f, 0xc3,
    0xfb, 0x7f, 0x6f, 0xfc, 0xf4, 0x7f, 0xf7, 0x0f, 0xf7, 0x7f, 0xf7, 0xff, 0xf7, 0x7f, 0xf7, 0xff, 0xe3, 0x7f, 0xf7,
    0x07, 0xe8, 0x7f, 0xef, 0xf8, 0x67, 0x70, 0x0f, 0xff, 0xbb, 0x6f, 0xf1, 0x00, 0xd0, 0x5b, 0xfd, 0x3f, 0xec, 0x53,
    0xc1, 0xff, 0xef, 0x57, 0x9f, 0xfd, 0xee, 0x5f, 0x9f, 0xfc, 0xae, 0x5f, 0x1f, 0x78, 0xac, 0x5f, 0x3f, 0x00, 0x50,
    0x6c, 0x7f, 0x00, 0xdc, 0x77, 0xff, 0xc0, 0x3f, 0x78, 0xff, 0x01, 0xf8, 0x7f, 0xff, 0x03, 0x9c, 0x78, 0xff, 0x07,
    0x8c, 0x7c, 0xff, 0x0f, 0xce, 0x78, 0xff, 0xff, 0xcf, 0x7f, 0xff, 0xff, 0xcf, 0x78, 0xff, 0xff, 0xdf, 0x78, 0xff,
    0xff, 0xdf, 0x7d, 0xff, 0xff, 0x3f, 0x7e, 0xff, 0xff, 0xff, 0x7f,
];

const BYTES_PER_ROW: usize = (WIDTH + 7) / 8;
const DISPLAY_HEIGHT: usize = HEIGHT / 2;

/// Pixel at (x, y): true = foreground, false = background.
fn get_pixel(x: usize, y: usize) -> bool {
    if y >= HEIGHT {
        return false;
    }
    let byte_index = y * BYTES_PER_ROW + x / 8;
    let bit_index = x % 8;
    ((BITS[byte_index] >> bit_index) & 1) == 0
}

/// Character for a cell (2 vertical pixels packed).
fn get_char(x: usize, row: usize) -> char {
    let upper = get_pixel(x, row * 2);
    let lower = get_pixel(x, row * 2 + 1);
    if upper && lower {
        '█'
    } else if upper {
        '▀'
    } else if lower {
        '▄'
    } else {
        ' '
    }
}

fn build_final_grid() -> Vec<String> {
    (0..DISPLAY_HEIGHT)
        .map(|row| {
            (0..WIDTH).map(|x| get_char(x, row)).collect::<String>()
        })
        .collect()
}

/// Armin easter egg component (static render).
pub struct ArminComponent {
    final_grid: Vec<String>,
}

impl ArminComponent {
    pub fn new() -> Self {
        Self {
            final_grid: build_final_grid(),
        }
    }
}

impl Component for ArminComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let padding = 1usize;
        let available_width = width.saturating_sub(padding);
        let mut lines: Vec<String> = Vec::new();
        for row in &self.final_grid {
            let clipped: String = row.chars().take(available_width).collect();
            let styled = theme()
                .as_ref()
                .map(|t| t.fg("accent", &clipped))
                .unwrap_or(clipped.clone());
            let pad_right = width.saturating_sub(padding + visible_len(&clipped));
            lines.push(format!(" {styled}{}", " ".repeat(pad_right)));
        }
        let message = "ARMIN SAYS HI";
        let msg_pad_right = width.saturating_sub(padding + message.len());
        let styled_message = theme()
            .as_ref()
            .map(|t| t.fg("accent", message))
            .unwrap_or_else(|| message.to_string());
        lines.push(format!(" {styled_message}{}", " ".repeat(msg_pad_right)));
        lines
    }
}

fn visible_len(text: &str) -> usize {
    pi_tui::utils::visible_width(text) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_final_grid() {
        let component = ArminComponent::new();
        let lines = component.render(40);
        assert_eq!(lines.len(), DISPLAY_HEIGHT + 1);
        assert!(lines[lines.len() - 1].contains("ARMIN SAYS HI"));
    }
}
