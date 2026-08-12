//! Tool management (fd/ripgrep download), port of
//! `packages/coding-agent/src/utils/tools-manager.ts`.

use std::path::Path;

use crate::config::get_bin_dir;
use crate::utils::child_process::{spawn_process_sync, SpawnOptions};
use crate::utils::management_http::{fetch_with_retry, RetryOptions};

const NETWORK_TIMEOUT_MS: u64 = 10_000;
const DOWNLOAD_TIMEOUT_MS: u64 = 120_000;

fn is_offline_mode_enabled() -> bool {
    match std::env::var("PI_OFFLINE") {
        Ok(value) => value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes"),
        Err(_) => false,
    }
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

fn architecture_name() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

fn is_windows() -> bool {
    cfg!(windows)
}

struct ToolConfig {
    name: &'static str,
    repo: &'static str,
    binary_name: &'static str,
    system_binary_names: &'static [&'static str],
    tag_prefix: &'static str,
    get_asset_name: fn(version: &str, plat: &str, architecture: &str) -> Option<String>,
}

fn fd_asset(version: &str, plat: &str, architecture: &str) -> Option<String> {
    let arch_str = if architecture == "arm64" { "aarch64" } else { "x86_64" };
    match plat {
        "darwin" => Some(format!("fd-v{version}-{arch_str}-apple-darwin.tar.gz")),
        "linux" => Some(format!("fd-v{version}-{arch_str}-unknown-linux-gnu.tar.gz")),
        "win32" => Some(format!("fd-v{version}-{arch_str}-pc-windows-msvc.zip")),
        _ => None,
    }
}

fn rg_asset(version: &str, plat: &str, architecture: &str) -> Option<String> {
    match plat {
        "darwin" => {
            let arch_str = if architecture == "arm64" { "aarch64" } else { "x86_64" };
            Some(format!("ripgrep-{version}-{arch_str}-apple-darwin.tar.gz"))
        }
        "linux" => {
            if architecture == "arm64" {
                Some(format!("ripgrep-{version}-aarch64-unknown-linux-gnu.tar.gz"))
            } else {
                Some(format!("ripgrep-{version}-x86_64-unknown-linux-musl.tar.gz"))
            }
        }
        "win32" => {
            let arch_str = if architecture == "arm64" { "aarch64" } else { "x86_64" };
            Some(format!("ripgrep-{version}-{arch_str}-pc-windows-msvc.zip"))
        }
        _ => None,
    }
}

fn tools() -> [ToolConfig; 2] {
    [
        ToolConfig {
            name: "fd",
            repo: "sharkdp/fd",
            binary_name: "fd",
            system_binary_names: &["fd", "fdfind"],
            tag_prefix: "v",
            get_asset_name: fd_asset,
        },
        ToolConfig {
            name: "ripgrep",
            repo: "BurntSushi/ripgrep",
            binary_name: "rg",
            system_binary_names: &["rg"],
            tag_prefix: "",
            get_asset_name: rg_asset,
        },
    ]
}

static TOOLS_TABLE: std::sync::LazyLock<[ToolConfig; 2]> = std::sync::LazyLock::new(|| tools());

fn tool_config(tool: &str) -> Option<&'static ToolConfig> {
    // ponytail: static table; matches the JS TOOLS record.
    TOOLS_TABLE.iter().find(|config| config.binary_name == tool)
}

fn command_exists(cmd: &str) -> bool {
    let result = spawn_process_sync(cmd, &["--version".to_string()], &SpawnOptions::default());
    match result {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Get the path to a tool (system-wide or in the tools dir; JS
/// `getToolPath`).
pub fn get_tool_path(tool: &str) -> Option<String> {
    let config = tool_config(tool)?;
    let binary_ext = if is_windows() { ".exe" } else { "" };
    let local_path = format!("{}/{}{binary_ext}", get_bin_dir(), config.binary_name);
    if Path::new(&local_path).exists() {
        return Some(local_path);
    }
    for system_binary_name in config.system_binary_names {
        if command_exists(system_binary_name) {
            return Some(system_binary_name.to_string());
        }
    }
    None
}

/// Fetch the latest release version from GitHub (JS `getLatestVersion`).
fn get_latest_version(repo: &str) -> Result<String, String> {
    let response = fetch_with_retry(
        &format!("https://api.github.com/repos/{repo}/releases/latest"),
        Some(vec![(
            "User-Agent".to_string(),
            format!("{}-coding-agent", crate::config::APP_NAME),
        )]),
        RetryOptions {
            max_retries: Some(2),
            retry_on_status: Some(true),
            timeout_ms: Some(NETWORK_TIMEOUT_MS),
        },
    )?;
    if !(200..300).contains(&response.status) {
        return Err(format!("GitHub API error: {}", response.status));
    }
    let value: pi_protocol::cbor::Value =
        pi_ai::utils::json::parse_json_with_repair(&response.body).map_err(|error| format!("{error}"))?;
    let entries: Vec<(String, pi_protocol::cbor::Value)> = value.as_map().map(|map| map.to_vec()).unwrap_or_default();
    let tag_name = entries
        .iter()
        .find(|(key, _)| key == "tag_name")
        .and_then(|(_, value)| value.as_str())
        .ok_or_else(|| "Missing tag_name".to_string())?;
    Ok(tag_name.strip_prefix('v').unwrap_or(tag_name).to_string())
}

/// Download a file from a URL (JS `downloadFile`).
fn download_file(url: &str, dest: &str) -> Result<(), String> {
    let response = fetch_with_retry(
        url,
        None,
        RetryOptions {
            max_retries: Some(2),
            retry_on_status: Some(true),
            timeout_ms: Some(DOWNLOAD_TIMEOUT_MS),
        },
    )?;
    if !(200..300).contains(&response.status) {
        return Err(format!("Failed to download: {}", response.status));
    }
    std::fs::write(dest, response.body.as_bytes()).map_err(|error| error.to_string())
}

fn find_binary_recursively(root_dir: &str, binary_file_name: &str) -> Option<String> {
    let mut stack: Vec<String> = vec![root_dir.to_string()];
    while let Some(current_dir) = stack.pop() {
        let entries = std::fs::read_dir(&current_dir).ok()?;
        for entry in entries.flatten() {
            let full_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().map(|kind| kind.is_file()).unwrap_or(false) && file_name == binary_file_name {
                return Some(full_path.to_string_lossy().to_string());
            }
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                stack.push(full_path.to_string_lossy().to_string());
            }
        }
    }
    None
}

fn run_extraction_command(command: &str, args: &[String]) -> Option<String> {
    let result = spawn_process_sync(command, args, &SpawnOptions::default());
    match result {
        Ok(result) if result.status == Some(0) => None,
        Ok(result) => Some(format!(
            "{command}: {}",
            result
                .stderr
                .trim()
                .split('\n')
                .next()
                .unwrap_or("unknown error")
                .to_string()
        )),
        Err(error) => Some(format!("{command}: {error}")),
    }
}

fn extract_tar_gz_archive(archive_path: &str, extract_dir: &str, asset_name: &str) -> Result<(), String> {
    let failure = run_extraction_command(
        "tar",
        &["xzf".to_string(), archive_path.to_string(), "-C".to_string(), extract_dir.to_string()],
    );
    match failure {
        None => Ok(()),
        Some(failure) => Err(format!("Failed to extract {asset_name}: {failure}")),
    }
}

fn extract_zip_archive(archive_path: &str, extract_dir: &str, asset_name: &str) -> Result<(), String> {
    let mut failures: Vec<String> = Vec::new();
    #[cfg(not(windows))]
    {
        if let Some(failure) = run_extraction_command(
            "unzip",
            &["-q".to_string(), archive_path.to_string(), "-d".to_string(), extract_dir.to_string()],
        ) {
            failures.push(failure);
        } else {
            return Ok(());
        }
        if let Some(failure) = run_extraction_command(
            "tar",
            &["xf".to_string(), archive_path.to_string(), "-C".to_string(), extract_dir.to_string()],
        ) {
            failures.push(failure);
        } else {
            return Ok(());
        }
    }
    #[cfg(windows)]
    {
        if let Some(failure) = run_extraction_command(
            "tar.exe",
            &["xf".to_string(), archive_path.to_string(), "-C".to_string(), extract_dir.to_string()],
        ) {
            failures.push(failure);
        } else {
            return Ok(());
        }
    }
    Err(format!("Failed to extract {asset_name}: {}", failures.join("; ")))
}

/// Download and install a tool (JS `downloadTool`).
fn download_tool(tool: &str) -> Result<String, String> {
    let config = tool_config(tool).ok_or_else(|| format!("Unknown tool: {tool}"))?;
    let plat = platform_name();
    let architecture = architecture_name();

    let mut version = get_latest_version(config.repo)?;
    if tool == "fd" && plat == "darwin" && architecture == "x64" {
        version = "10.3.0".to_string();
    }

    let asset_name = (config.get_asset_name)(&version, plat, architecture)
        .ok_or_else(|| format!("Unsupported platform: {plat}/{architecture}"))?;

    let tools_dir = get_bin_dir();
    std::fs::create_dir_all(&tools_dir).map_err(|error| error.to_string())?;

    let download_url = format!(
        "https://github.com/{}/releases/download/{}{version}/{asset_name}",
        config.repo, config.tag_prefix
    );
    let archive_path = format!("{tools_dir}/{asset_name}");
    let binary_ext = if is_windows() { ".exe" } else { "" };
    let binary_path = format!("{tools_dir}/{}{binary_ext}", config.binary_name);

    download_file(&download_url, &archive_path)?;

    let extract_dir = format!(
        "{tools_dir}/extract_tmp_{}_{}_{}_{:08x}",
        config.binary_name,
        std::process::id(),
        now_millis(),
        rand_suffix()
    );
    std::fs::create_dir_all(&extract_dir).map_err(|error| error.to_string())?;

    let result = (|| -> Result<(), String> {
        if asset_name.ends_with(".tar.gz") {
            extract_tar_gz_archive(&archive_path, &extract_dir, &asset_name)?;
        } else if asset_name.ends_with(".zip") {
            extract_zip_archive(&archive_path, &extract_dir, &asset_name)?;
        } else {
            return Err(format!("Unsupported archive format: {asset_name}"));
        }

        let binary_file_name = format!("{}{binary_ext}", config.binary_name);
        let extracted_dir = extract_dir.trim_end_matches('/').to_string();
        let stripped = asset_name.replace(".tar.gz", "").replace(".zip", "");
        let versioned_dir = format!("{extracted_dir}/{stripped}");
        let mut extracted_binary = vec![format!("{versioned_dir}/{binary_file_name}"), format!("{extracted_dir}/{binary_file_name}")]
            .into_iter()
            .find(|candidate| Path::new(candidate).exists());

        if extracted_binary.is_none() {
            extracted_binary = find_binary_recursively(&extract_dir, &binary_file_name);
        }

        if let Some(extracted_binary) = extracted_binary {
            std::fs::rename(&extracted_binary, &binary_path).map_err(|error| error.to_string())?;
        } else {
            return Err(format!(
                "Binary not found in archive: expected {binary_file_name} under {extract_dir}"
            ));
        }

        if !is_windows() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })();

    let _ = std::fs::remove_file(&archive_path);
    let _ = std::fs::remove_dir_all(&extract_dir);
    result?;

    Ok(binary_path)
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn rand_suffix() -> u32 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0))
        ^ std::process::id()
}

/// Ensure a tool is available, downloading if necessary (JS `ensureTool`).
pub fn ensure_tool(tool: &str, silent: bool) -> Result<Option<String>, ()> {
    if let Some(existing_path) = get_tool_path(tool) {
        return Ok(Some(existing_path));
    }
    let config = match tool_config(tool) {
        Some(config) => config,
        None => return Ok(None),
    };

    if is_offline_mode_enabled() {
        if !silent {
            eprintln!("\x1b[33m{} not found. Offline mode enabled, skipping download.\x1b[0m", config.name);
        }
        return Ok(None);
    }
    if platform_name() == "android" {
        if !silent {
            eprintln!("\x1b[33m{} not found. Install with: pkg install {}\x1b[0m", config.name, tool);
        }
        return Ok(None);
    }

    if !silent {
        eprintln!("\x1b[2m{} not found. Downloading...\x1b[0m", config.name);
    }
    match download_tool(tool) {
        Ok(path) => {
            if !silent {
                eprintln!("\x1b[2m{} installed to {path}\x1b[0m", config.name);
            }
            Ok(Some(path))
        }
        Err(error) => {
            if !silent {
                eprintln!("\x1b[33mFailed to download {}: {error}\x1b[0m", config.name);
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_configs_exist() {
        assert!(tool_config("fd").is_some());
        assert!(tool_config("rg").is_some());
        assert!(tool_config("nope").is_none());
    }

    #[test]
    fn asset_names_match_platform() {
        let fd = tool_config("fd").unwrap();
        let asset = (fd.get_asset_name)("1.0.0", "darwin", "arm64").unwrap();
        assert_eq!(asset, "fd-v1.0.0-aarch64-apple-darwin.tar.gz");
        let rg = tool_config("rg").unwrap();
        let asset = (rg.get_asset_name)("14.1.0", "linux", "x64").unwrap();
        assert_eq!(asset, "ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz");
        assert!((fd.get_asset_name)("1.0.0", "plan9", "x64").is_none());
    }

    #[test]
    fn offline_mode_detection() {
        std::env::set_var("PI_OFFLINE", "true");
        assert!(is_offline_mode_enabled());
        std::env::set_var("PI_OFFLINE", "0");
        assert!(!is_offline_mode_enabled());
        std::env::remove_var("PI_OFFLINE");
    }

    #[test]
    fn finds_binary_recursively() {
        let dir = std::env::temp_dir().join(format!("pi-tools-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("nested/deep")).unwrap();
        std::fs::write(dir.join("nested/deep/fd"), b"x").unwrap();
        let found = find_binary_recursively(&dir.to_string_lossy(), "fd").unwrap();
        assert!(found.ends_with("fd"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_tool_download_fails() {
        assert!(download_tool("nope").is_err());
    }
}
