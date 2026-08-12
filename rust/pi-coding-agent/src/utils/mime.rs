//! Image MIME detection, port of
//! `packages/coding-agent/src/utils/mime.ts`.

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

/// Detect a supported image MIME type from a buffer (JS
/// `detectSupportedImageMimeType`).
pub fn detect_supported_image_mime_type(buffer: &[u8]) -> Option<&'static str> {
    if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
        if buffer.get(3) == Some(&0xf7) {
            return None;
        }
        return Some("image/jpeg");
    }
    if starts_with(buffer, &PNG_SIGNATURE) {
        if is_png(buffer) && !is_animated_png(buffer) {
            return Some("image/png");
        }
        return None;
    }
    if starts_with_ascii(buffer, 0, "GIF") {
        return Some("image/gif");
    }
    if starts_with_ascii(buffer, 0, "RIFF") && starts_with_ascii(buffer, 8, "WEBP") {
        return Some("image/webp");
    }
    if starts_with_ascii(buffer, 0, "BM") && is_bmp(buffer) {
        return Some("image/bmp");
    }
    None
}

/// Detect a supported image MIME type from a file.
pub fn detect_supported_image_mime_type_from_file(file_path: &str) -> Option<&'static str> {
    let file = std::fs::File::open(file_path).ok()?;
    use std::io::Read;
    let mut buffer = [0u8; 4100];
    let bytes_read = file.take(4100).read(&mut buffer).ok()?;
    detect_supported_image_mime_type(&buffer[..bytes_read])
}

fn is_png(buffer: &[u8]) -> bool {
    buffer.len() >= 16
        && read_uint32_be(buffer, PNG_SIGNATURE.len()) == 13
        && starts_with_ascii(buffer, 12, "IHDR")
}

fn is_animated_png(buffer: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buffer.len() {
        let chunk_length = read_uint32_be(buffer, offset);
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buffer, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buffer, chunk_type_offset, "IDAT") {
            return false;
        }
        let next_offset = offset + 8 + chunk_length as usize + 4;
        if next_offset <= offset || next_offset > buffer.len() {
            return false;
        }
        offset = next_offset;
    }
    false
}

fn is_bmp(buffer: &[u8]) -> bool {
    if buffer.len() < 26 {
        return false;
    }
    let declared_file_size = read_uint32_le(buffer, 2);
    let pixel_data_offset = read_uint32_le(buffer, 10);
    let dib_header_size = read_uint32_le(buffer, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if pixel_data_offset < 14 + dib_header_size {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }
    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_uint16_le(buffer, 22), read_uint16_le(buffer, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buffer.len() < 30 {
            return false;
        }
        (read_uint16_le(buffer, 26), read_uint16_le(buffer, 28))
    } else {
        return false;
    };
    color_planes == 1 && matches!(bits_per_pixel, 1 | 4 | 8 | 16 | 24 | 32)
}

fn read_uint16_le(buffer: &[u8], offset: usize) -> u32 {
    (buffer.get(offset).copied().unwrap_or(0) as u32)
        + ((buffer.get(offset + 1).copied().unwrap_or(0) as u32) << 8)
}

fn read_uint32_be(buffer: &[u8], offset: usize) -> u32 {
    (buffer.get(offset).copied().unwrap_or(0) as u32) * 0x1000000
        + ((buffer.get(offset + 1).copied().unwrap_or(0) as u32) << 16)
        + ((buffer.get(offset + 2).copied().unwrap_or(0) as u32) << 8)
        + (buffer.get(offset + 3).copied().unwrap_or(0) as u32)
}

fn read_uint32_le(buffer: &[u8], offset: usize) -> u32 {
    (buffer.get(offset).copied().unwrap_or(0) as u32)
        + ((buffer.get(offset + 1).copied().unwrap_or(0) as u32) << 8)
        + ((buffer.get(offset + 2).copied().unwrap_or(0) as u32) << 16)
        + ((buffer.get(offset + 3).copied().unwrap_or(0) as u32) * 0x1000000)
}

fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
    buffer.len() >= bytes.len() && bytes.iter().zip(buffer.iter()).all(|(expected, actual)| expected == actual)
}

fn starts_with_ascii(buffer: &[u8], offset: usize, text: &str) -> bool {
    if buffer.len() < offset + text.len() {
        return false;
    }
    text.as_bytes()
        .iter()
        .zip(buffer[offset..].iter())
        .all(|(expected, actual)| expected == actual)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_jpeg() {
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        assert_eq!(detect_supported_image_mime_type(&jpeg), Some("image/jpeg"));
        // 0xf7 marker is rejected.
        let jpg2000 = [0xff, 0xd8, 0xff, 0xf7];
        assert_eq!(detect_supported_image_mime_type(&jpg2000), None);
    }

    #[test]
    fn detects_png() {
        let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        // IHDR chunk: length 13 + "IHDR"
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0; 13]);
        assert_eq!(detect_supported_image_mime_type(&png), Some("image/png"));
    }

    #[test]
    fn detects_animated_png_as_unsupported() {
        let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&[0, 0, 0, 8]);
        png.extend_from_slice(b"acTL");
        png.extend_from_slice(&[0; 8]);
        assert_eq!(detect_supported_image_mime_type(&png), None);
    }

    #[test]
    fn detects_gif_webp_bmp() {
        assert_eq!(detect_supported_image_mime_type(b"GIF89a..."), Some("image/gif"));
        let mut webp = b"RIFF....WEBP".to_vec();
        webp[4..8].copy_from_slice(&[0, 0, 0, 8]);
        assert_eq!(detect_supported_image_mime_type(&webp), Some("image/webp"));
        assert_eq!(detect_supported_image_mime_type(&[0u8; 10]), None);
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(detect_supported_image_mime_type(b"plain text"), None);
    }
}
