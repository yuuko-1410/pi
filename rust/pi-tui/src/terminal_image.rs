//! Terminal image support (Kitty graphics protocol), port of
//! `packages/tui/src/terminal-image.ts`.

use std::collections::HashMap;
use std::sync::Mutex;

pub type ImageProtocol = Option<&'static str>; // "kitty" | "iterm2" | null

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellDimensions {
    pub width_px: f64,
    pub height_px: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageDimensions {
    pub width_px: f64,
    pub height_px: f64,
}

pub const KITTY_PREFIX: &str = "\x1b_G";
pub const ITERM2_PREFIX: &str = "\x1b]1337;File=";

pub fn is_image_line(line: &str) -> bool {
    line.starts_with(KITTY_PREFIX)
        || line.starts_with(ITERM2_PREFIX)
        || line.contains(KITTY_PREFIX)
        || line.contains(ITERM2_PREFIX)
}

static CELL_DIMENSIONS: Mutex<CellDimensions> = Mutex::new(CellDimensions {
    width_px: 9.0,
    height_px: 18.0,
});

pub fn get_cell_dimensions() -> CellDimensions {
    *CELL_DIMENSIONS.lock().unwrap()
}

pub fn set_cell_dimensions(dims: CellDimensions) {
    *CELL_DIMENSIONS.lock().unwrap() = dims;
}

static IMAGE_ID_COUNTER: Mutex<u64> = Mutex::new(0);

/// Allocate an image id (JS uses a random id; a monotonic counter is
/// equivalent for collision avoidance within one process).
pub fn allocate_image_id() -> u64 {
    let mut counter = IMAGE_ID_COUNTER.lock().unwrap();
    *counter += 1;
    *counter
}

pub fn encode_kitty(
    base64_data: &str,
    columns: Option<f64>,
    rows: Option<f64>,
    image_id: Option<u64>,
    move_cursor: Option<bool>,
) -> String {
    const CHUNK_SIZE: usize = 4096;

    let mut params: Vec<String> = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];
    if move_cursor == Some(false) {
        params.push("C=1".to_string());
    }
    if let Some(columns) = columns {
        params.push(format!("c={columns}"));
    }
    if let Some(rows) = rows {
        params.push(format!("r={rows}"));
    }
    if let Some(image_id) = image_id {
        params.push(format!("i={image_id}"));
    }

    if base64_data.len() <= CHUNK_SIZE {
        return format!("\x1b_G{};{base64_data}\x1b\\", params.join(","));
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut offset = 0;
    let mut is_first = true;
    while offset < base64_data.len() {
        let end = (offset + CHUNK_SIZE).min(base64_data.len());
        let chunk = &base64_data[offset..end];
        let is_last = end >= base64_data.len();
        if is_first {
            chunks.push(format!("\x1b_G{},m=1;{chunk}\x1b\\", params.join(",")));
            is_first = false;
        } else if is_last {
            chunks.push(format!("\x1b_Gm=0;{chunk}\x1b\\"));
        } else {
            chunks.push(format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }
        offset = end;
    }
    chunks.join("")
}

pub fn delete_kitty_image(image_id: u64) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

pub fn delete_all_kitty_placements() -> String {
    "\x1b_Ga=d,d=a,q=2\x1b\\".to_string()
}

/// Encode an iTerm2 inline image.
pub fn encode_iterm2(
    base64_data: &str,
    width: Option<String>,
    height: Option<String>,
    name: Option<&str>,
    preserve_aspect_ratio: Option<bool>,
    inline: Option<bool>,
) -> String {
    let decoded_size = base64_len_to_bytes(base64_data);
    let mut params: Vec<String> = vec![
        format!("inline={}", if inline != Some(false) { 1 } else { 0 }),
        format!("size={decoded_size}"),
    ];
    if let Some(width) = width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = name {
        params.push(format!("name={}", base64_encode_utf8(name)));
    }
    if preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".to_string());
    }
    format!("\x1b]1337;File={}:{base64_data}\x07", params.join(";"))
}

/// Approximate base64 string length to decoded bytes.
fn base64_len_to_bytes(base64_data: &str) -> usize {
    let len = base64_data.len();
    let padding = base64_data.bytes().rev().take_while(|byte| *byte == b'=').count();
    len / 4 * 3 - padding
}

fn base64_encode_utf8(text: &str) -> String {
    // ponytail: ASCII base64 alphabet via a small encoder (no external deps).
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        result.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    result
}

#[derive(Clone, Debug, PartialEq)]
pub struct KittyImageMetadata {
    pub image_id: u64,
    pub columns: f64,
    pub rows: f64,
    pub width_px: f64,
    pub height_px: f64,
}

static KITTY_IMAGE_METADATA: Mutex<Option<HashMap<u64, KittyImageMetadata>>> = Mutex::new(None);
static KITTY_TRANSMISSION_GENERATION: Mutex<u64> = Mutex::new(0);

pub fn register_kitty_image_metadata(metadata: KittyImageMetadata) {
    let mut generation = KITTY_TRANSMISSION_GENERATION.lock().unwrap();
    *generation += 1;
    let mut map = KITTY_IMAGE_METADATA.lock().unwrap();
    let map = map.get_or_insert_with(HashMap::new);
    map.remove(&metadata.image_id);
    map.insert(metadata.image_id, metadata);
    if map.len() > 1000 {
        if let Some(oldest) = map.keys().next().copied() {
            map.remove(&oldest);
        }
    }
}

fn kitty_controls(line: &str) -> Option<String> {
    let start = line.find(KITTY_PREFIX)? + KITTY_PREFIX.len();
    let end = line[start..].find(';')? + start;
    Some(line[start..end].to_string())
}

fn parse_image_id(controls: &str) -> Option<u64> {
    controls.split(',').find_map(|control| {
        control
            .strip_prefix("i=")
            .and_then(|value| value.parse::<u64>().ok())
    })
}

pub fn get_kitty_image_metadata(line: &str) -> Option<KittyImageMetadata> {
    let controls = kitty_controls(line)?;
    let image_id = parse_image_id(&controls)?;
    KITTY_IMAGE_METADATA
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|map| map.get(&image_id))
        .cloned()
}

/// Crop a Kitty image line to a row range.
pub fn crop_kitty_image_line(line: &str, hidden_rows: f64, visible_rows: f64) -> String {
    let Some(metadata) = get_kitty_image_metadata(line) else {
        return line.to_string();
    };
    let Some(match_start) = line.find(KITTY_PREFIX) else {
        return line.to_string();
    };
    let controls_start = match_start + KITTY_PREFIX.len();
    let Some(semicolon) = line[controls_start..].find(';') else {
        return line.to_string();
    };
    let controls_end = controls_start + semicolon;
    let controls = &line[controls_start..controls_end];

    if hidden_rows < 0.0 || hidden_rows >= metadata.rows || visible_rows <= 0.0 {
        return line.to_string();
    }
    let cropped_rows = visible_rows.min(metadata.rows - hidden_rows);
    if hidden_rows == 0.0 && cropped_rows == metadata.rows {
        return line.to_string();
    }
    let source_y = ((metadata.height_px * hidden_rows) / metadata.rows).floor();
    let source_end = ((metadata.height_px * (hidden_rows + cropped_rows)) / metadata.rows).ceil();
    let source_height = (metadata.height_px.min(source_end) - source_y).max(1.0);

    let filtered: Vec<&str> = controls
        .split(',')
        .filter(|control| !control.starts_with('y') && !control.starts_with('h') && !control.starts_with('r'))
        .collect();
    let new_controls = format!(
        "{},{},y={source_y},h={source_height},r={cropped_rows}",
        filtered.join(","),
        "".to_string()
    );
    let rest = &line[controls_end..];
    format!("{KITTY_PREFIX}{new_controls}{rest}")
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageCellSize {
    pub columns: f64,
    pub rows: f64,
}

/// Calculate the cell size of an image given pixel dimensions.
pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: f64,
    max_height_cells: Option<f64>,
    cell_dimensions: CellDimensions,
) -> ImageCellSize {
    let max_width = max_width_cells.max(1.0).floor();
    let cell_width = cell_dimensions.width_px.max(1.0);
    let cell_height = cell_dimensions.height_px.max(1.0);
    let natural_width = image_dimensions.width_px / cell_width;
    let natural_height = image_dimensions.height_px / cell_height;
    let scale = if natural_width <= max_width {
        1.0
    } else {
        max_width / natural_width
    };
    let columns = (natural_width * scale).round().max(1.0);
    let rows = (natural_height * scale).round().max(1.0);
    match max_height_cells {
        Some(max_height_cells) if rows > max_height_cells.max(1.0) => {
            let height_scale = max_height_cells.max(1.0) / rows;
            ImageCellSize {
                columns: (columns * height_scale).round().max(1.0),
                rows: max_height_cells.max(1.0),
            }
        }
        _ => ImageCellSize { columns, rows },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_image_lines() {
        assert!(is_image_line("\x1b_Ga=T;data\x1b\\"));
        assert!(is_image_line("\x1b]1337;File=inline=1;:data\x07"));
        assert!(is_image_line("prefix \x1b_Ga=T;data\x1b\\"));
        assert!(!is_image_line("plain text"));
    }

    #[test]
    fn encodes_kitty_single_chunk() {
        let encoded = encode_kitty("abc", Some(2.0), Some(3.0), Some(7), Some(true));
        assert!(encoded.starts_with("\x1b_Ga=T,f=100,q=2,c=2,r=3,i=7;abc\x1b\\"));
    }

    #[test]
    fn encodes_kitty_chunked() {
        let data = "x".repeat(10000);
        let encoded = encode_kitty(&data, None, None, None, None);
        assert!(encoded.contains("m=1;"));
        assert!(encoded.contains("\x1b_Gm=0;"));
        assert!(encoded.ends_with("\x1b\\"));
    }

    #[test]
    fn delete_sequences() {
        assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
        assert_eq!(delete_all_kitty_images(), "\x1b_Ga=d,d=A,q=2\x1b\\");
    }

    #[test]
    fn registers_and_reads_metadata() {
        register_kitty_image_metadata(KittyImageMetadata {
            image_id: 5,
            columns: 2.0,
            rows: 3.0,
            width_px: 180.0,
            height_px: 54.0,
        });
        let metadata = get_kitty_image_metadata("\x1b_Ga=p,i=5;\x1b\\").unwrap();
        assert_eq!(metadata.image_id, 5);
        assert_eq!(metadata.rows, 3.0);
        assert_eq!(get_kitty_image_metadata("plain"), None);
    }

    #[test]
    fn crops_image_line() {
        register_kitty_image_metadata(KittyImageMetadata {
            image_id: 9,
            columns: 2.0,
            rows: 4.0,
            width_px: 200.0,
            height_px: 80.0,
        });
        let line = "\x1b_Ga=T,i=9;\x1b\\";
        let cropped = crop_kitty_image_line(line, 2.0, 2.0);
        assert!(cropped.contains("y=40"));
        assert!(cropped.contains("r=2"));
        // Full crop is a no-op.
        assert_eq!(crop_kitty_image_line(line, 0.0, 4.0), line);
        assert_eq!(crop_kitty_image_line("plain", 1.0, 1.0), "plain");
    }

    #[test]
    fn calculates_cell_size() {
        let dims = CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        };
        let size = calculate_image_cell_size(
            ImageDimensions {
                width_px: 90.0,
                height_px: 180.0,
            },
            20.0,
            None,
            dims,
        );
        assert_eq!(size, ImageCellSize {
            columns: 10.0,
            rows: 10.0,
        });
    }

    #[test]
    fn scales_down_to_max_width() {
        let dims = CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        };
        let size = calculate_image_cell_size(
            ImageDimensions {
                width_px: 900.0,
                height_px: 180.0,
            },
            10.0,
            None,
            dims,
        );
        assert_eq!(size.columns, 10.0);
        assert_eq!(size.rows, 1.0);
    }

    #[test]
    fn base64_encode_utf8_works() {
        assert_eq!(base64_encode_utf8("hello"), "aGVsbG8=");
        assert_eq!(base64_encode_utf8("a"), "YQ==");
        assert_eq!(base64_encode_utf8("ab"), "YWI=");
    }
}
