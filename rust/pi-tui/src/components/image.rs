//! Image component, port of `packages/tui/src/components/image.ts`.

use std::sync::Arc;

use crate::tui::Component;
use crate::terminal_image::{
    allocate_image_id, get_capabilities, get_cell_dimensions, get_image_dimensions, image_fallback,
    render_image, ImageDimensions,
};
use crate::utils::truncate_to_width;

pub struct ImageTheme {
    pub fallback_color: Arc<dyn Fn(&str) -> String + Send + Sync>,
}

pub struct ImageOptions {
    pub max_width_cells: Option<f64>,
    pub max_height_cells: Option<f64>,
    pub filename: Option<String>,
    pub image_id: Option<u64>,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            max_width_cells: None,
            max_height_cells: None,
            filename: None,
            image_id: None,
        }
    }
}

pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    fallback_color: Arc<dyn Fn(&str) -> String + Send + Sync>,
    options: ImageOptions,
    image_id: Option<u64>,
}

impl Image {
    pub fn new(
        base64_data: &str,
        mime_type: &str,
        fallback_color: Arc<dyn Fn(&str) -> String + Send + Sync>,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(base64_data, mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800.0,
                height_px: 600.0,
            });
        let image_id = options.image_id;
        Self {
            base64_data: base64_data.to_string(),
            mime_type: mime_type.to_string(),
            dimensions,
            fallback_color,
            options,
            image_id,
        }
    }

    pub fn get_image_id(&self) -> Option<u64> {
        self.image_id
    }
}

impl Component for Image {
    fn render(&self, width: usize) -> Vec<String> {
        let max_width = (width as f64 - 2.0).max(1.0).min(self.options.max_width_cells.unwrap_or(60.0));
        let cell_dimensions = get_cell_dimensions();
        let default_max_height = (max_width * cell_dimensions.width_px / cell_dimensions.height_px).ceil().max(1.0);
        let max_height = self.options.max_height_cells.unwrap_or(default_max_height);

        let caps = get_capabilities();
        let mut lines: Vec<String> = Vec::new();

        if caps.images.is_some() {
            let mut image_id = self.image_id;
            if caps.images.as_deref() == Some("kitty") && image_id.is_none() {
                image_id = Some(allocate_image_id());
            }
            let result = render_image(
                &self.base64_data,
                self.dimensions.clone(),
                Some(max_width),
                Some(max_height),
                image_id,
                Some(false),
            );
            match result {
                Some(result) => {
                    let _ = result.image_id;
                    if caps.images.as_deref() == Some("kitty") {
                        lines.push(result.sequence.clone());
                        for _ in 0..(result.rows as usize).saturating_sub(1) {
                            lines.push(String::new());
                        }
                    } else {
                        for _ in 0..(result.rows as usize).saturating_sub(1) {
                            lines.push(String::new());
                        }
                        let row_offset = (result.rows - 1.0).max(0.0) as usize;
                        let move_up = if row_offset > 0 {
                            format!("\x1b[{row_offset}A")
                        } else {
                            String::new()
                        };
                        lines.push(format!("{move_up}{}", result.sequence));
                    }
                }
                None => {
                    let fallback = image_fallback(&self.mime_type, Some(self.dimensions.clone()), self.options.filename.as_deref());
                    lines.push(truncate_to_width(&(self.fallback_color)(&fallback), width as f64, "...", false));
                }
            }
        } else {
            let fallback = image_fallback(&self.mime_type, Some(self.dimensions.clone()), self.options.filename.as_deref());
            lines.push(truncate_to_width(&(self.fallback_color)(&fallback), width as f64, "...", false));
        }

        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Arc<dyn Fn(&str) -> String + Send + Sync> {
        Arc::new(|text| text.to_string())
    }

    #[test]
    fn fallback_without_capabilities() {
        crate::terminal_image::set_capabilities(crate::terminal_image::TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        // 1x1 transparent PNG base64.
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let image = Image::new(png, "image/png", identity(), ImageOptions::default(), None);
        let lines = image.render(40);
        assert!(lines[0].contains("[Image:"));
        assert!(lines[0].contains("image/png"));
    }

    #[test]
    fn png_dimensions_parsed() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        let dims = crate::terminal_image::get_image_dimensions(png, "image/png").unwrap();
        assert_eq!(dims.width_px, 1.0);
        assert_eq!(dims.height_px, 1.0);
    }

    #[test]
    fn invalid_image_returns_none() {
        assert!(crate::terminal_image::get_image_dimensions("notbase64", "image/png").is_none());
        assert!(crate::terminal_image::get_image_dimensions("", "image/jpeg").is_none());
    }

    #[test]
    fn image_fallback_format() {
        let fallback = image_fallback("image/png", Some(ImageDimensions { width_px: 100.0, height_px: 50.0 }), None);
        assert!(fallback.contains("[Image:"));
        assert!(fallback.contains("[image/png]"));
        assert!(fallback.contains("100x50"));
    }
}
