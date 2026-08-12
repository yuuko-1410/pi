//! App configuration and path resolution, port of
//! `packages/coding-agent/src/config.ts` (path/constants subset; the full
//! settings-file layer is ported with settings-manager).

use std::path::PathBuf;

use crate::utils::child_process::{normalize_path, PathInputOptions};

/// APP_NAME: piConfig.name from package.json or "pi".
pub const APP_NAME: &str = "pi";
pub const APP_TITLE: &str = "π";
/// CONFIG_DIR_NAME: pkg.piConfig.configDir or ".pi".
pub const CONFIG_DIR_NAME: &str = ".pi";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// e.g., PI_CODING_AGENT_DIR
pub fn env_agent_dir() -> String {
    format!("{}_CODING_AGENT_DIR", APP_NAME.to_uppercase())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Agent directory: $PI_CODING_AGENT_DIR or ~/.pi/agent.
pub fn get_agent_dir() -> String {
    if let Ok(env_dir) = std::env::var(env_agent_dir()) {
        return normalize_path(&env_dir, &PathInputOptions::default());
    }
    home_dir()
        .map(|home| home.join(CONFIG_DIR_NAME).join("agent").to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string())
}

/// Bin directory for downloaded tools.
pub fn get_bin_dir() -> String {
    std::path::Path::new(&get_agent_dir())
        .join("bin")
        .to_string_lossy()
        .to_string()
}

/// Package directory: $PI_PACKAGE_DIR or the crate directory.
pub fn get_package_dir() -> String {
    if let Ok(env_dir) = std::env::var("PI_PACKAGE_DIR") {
        return normalize_path(&env_dir, &PathInputOptions::default());
    }
    // In the Rust workspace the package dir is the crate source dir.
    std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string())
}

/// Path to CHANGELOG.md.
pub fn get_changelog_path() -> String {
    std::path::Path::new(&get_package_dir())
        .join("CHANGELOG.md")
        .to_string_lossy()
        .to_string()
}

/// Sessions directory.
pub fn get_sessions_dir() -> String {
    std::path::Path::new(&get_agent_dir())
        .join("sessions")
        .to_string_lossy()
        .to_string()
}

/// Models path: $PI_MODELS_PATH or agent dir models.json.
pub fn get_models_path() -> String {
    if let Ok(path) = std::env::var("PI_MODELS_PATH") {
        return normalize_path(&path, &PathInputOptions::default());
    }
    std::path::Path::new(&get_agent_dir())
        .join("models.json")
        .to_string_lossy()
        .to_string()
}

/// Interactive assets directory.
pub fn get_interactive_assets_dir() -> String {
    std::path::Path::new(&get_package_dir())
        .join("src")
        .join("modes")
        .join("interactive")
        .join("assets")
        .to_string_lossy()
        .to_string()
}

/// Expand a tilde path (JS `expandTildePath`).
pub fn expand_tilde_path(path: &str) -> String {
    normalize_path(path, &PathInputOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_dir_defaults_to_home() {
        let agent_dir = get_agent_dir();
        assert!(agent_dir.ends_with("agent"));
    }

    #[test]
    fn bin_dir_under_agent() {
        let bin_dir = get_bin_dir();
        assert!(bin_dir.ends_with("bin"));
    }

    #[test]
    fn env_dir_override() {
        std::env::set_var("PI_CODING_AGENT_DIR", "/tmp/test-agent");
        assert_eq!(get_agent_dir(), "/tmp/test-agent");
        std::env::remove_var("PI_CODING_AGENT_DIR");
    }

    #[test]
    fn changelog_path_is_absolute() {
        assert!(get_changelog_path().ends_with("CHANGELOG.md"));
    }
}
