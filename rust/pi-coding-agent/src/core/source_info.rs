//! Source info, port of `core/source-info.ts`. The JS PathMetadata comes
//! from package-manager; the Rust port keeps the fields needed here.

#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: String,   // "user" | "project" | "temporary"
    pub origin: String,  // "package" | "top-level"
    pub base_dir: Option<String>,
}

/// Options for synthetic source info: (source, scope, baseDir).
pub type SyntheticOptions = (String, String, Option<String>);

pub fn create_synthetic_source_info(path: &str, options: Option<SyntheticOptions>) -> SourceInfo {
    let (source, scope, base_dir) = options.unwrap_or_else(|| {
        ("".to_string(), "temporary".to_string(), None)
    });
    SourceInfo {
        path: path.to_string(),
        source,
        scope,
        origin: "top-level".to_string(),
        base_dir,
    }
}

/// Source info from package metadata (source/scope/origin from PathMetadata).
pub fn create_source_info_from_metadata(path: &str, source: &str, scope: &str, origin: &str, base_dir: Option<&str>) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: source.to_string(),
        scope: scope.to_string(),
        origin: origin.to_string(),
        base_dir: base_dir.map(|value| value.to_string()),
    }
}
