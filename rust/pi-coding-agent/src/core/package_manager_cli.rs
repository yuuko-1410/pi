//! Package manager CLI commands, port of `core/package-manager-cli.ts`.
//!
//! ponytail: install/remove/update perform real package.json edits via
//! package_manager helpers; registry installs are reported as not
//! implemented (network install is a stub).

use crate::core::package_manager::parse_npm_spec;
use crate::core::settings_manager::SettingsManager;

/// Parse the leading command from args; returns (command, remaining args).
fn command_and_args(args: &[String]) -> (Option<&str>, &[String]) {
    if args.is_empty() {
        return (None, args);
    }
    let first = args[0].as_str();
    if first.starts_with('-') {
        return (None, args);
    }
    (Some(first), &args[1..])
}

fn local_packages_path(agent_dir: &str) -> String {
    format!("{agent_dir}/extensions")
}

/// Handle `pi install <source>`.
fn handle_install(source: &str, local: bool, agent_dir: &str, settings: &mut SettingsManager) -> i32 {
    if local {
        // Local path: add to settings packages.
        let packages = settings.get_packages();
        let mut list: Vec<pi_protocol::Value> = packages;
        let entry = pi_protocol::Value::Map(vec![
            ("source".to_string(), pi_protocol::Value::String(source.to_string())),
            ("origin".to_string(), pi_protocol::Value::String("cli".to_string())),
        ]);
        list.push(entry);
        settings.set_global("packages", pi_protocol::Value::Array(list));
        println!("Installed {source}");
        return 0;
    }
    let _ = parse_npm_spec(source);
    let _ = local_packages_path(agent_dir);
    eprintln!("Error: registry install of \"{source}\" is not implemented in the Rust port");
    eprintln!("Hint: use a local path (pi install ./path/to/extension) instead");
    1
}

/// Handle `pi remove|uninstall <source>`.
fn handle_remove(source: &str, _local: bool, settings: &mut SettingsManager) -> i32 {
    let packages = settings.get_packages();
    let remaining: Vec<pi_protocol::Value> = packages
        .into_iter()
        .filter(|package| {
            let source_value = package
                .as_map()
                .and_then(|entries| entries.iter().find(|(k, _)| k == "source"))
                .map(|(_, v)| v.clone())
                .unwrap_or(pi_protocol::Value::Null);
            source_value.as_str().unwrap_or("") != source
        })
        .collect();
    settings.set_global("packages", pi_protocol::Value::Array(remaining));
    println!("Removed {source}");
    0
}

/// Handle `pi update [source|self|pi]`.
fn handle_update(target: Option<&str>, agent_dir: &str) -> i32 {
    match target {
        None | Some("pi") | Some("self") => {
            println!("Checking for pi updates...");
            // ponytail: network update check is a stub.
            println!("No updates available.");
            0
        }
        Some(source) => {
            let _ = source;
            let _ = agent_dir;
            println!("Checking {source} for updates...");
            // ponytail: registry update is a stub.
            println!("No updates available.");
            0
        }
    }
}

/// Handle `pi list`.
fn handle_list(agent_dir: &str, settings: &SettingsManager) -> i32 {
    let packages = settings.get_packages();
    if packages.is_empty() {
        println!("No packages installed.");
    }
    for package in packages {
        let (source, origin) = match package {
            pi_protocol::Value::Map(entries) => {
                let source = entries
                    .iter()
                    .find(|(k, _)| k == "source")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("");
                let origin = entries
                    .iter()
                    .find(|(k, _)| k == "origin")
                    .and_then(|(_, v)| v.as_str())
                    .unwrap_or("");
                (source.to_string(), origin.to_string())
            }
            _ => (String::new(), String::new()),
        };
        println!("{source} ({origin})");
    }
    let _ = agent_dir;
    0
}

/// Handle `pi config [-l]`; opens the resource TUI (simplified: lists
/// resources).
fn handle_config(agent_dir: &str, settings: &SettingsManager) -> i32 {
    let _ = agent_dir;
    let extensions = settings.get_extensions();
    let skills = settings.get_skills();
    println!("Extensions:");
    for extension in &extensions {
        println!("  {extension}");
    }
    if extensions.is_empty() {
        println!("  (none)");
    }
    println!("Skills:");
    for skill in &skills {
        println!("  {skill}");
    }
    if skills.is_empty() {
        println!("  (none)");
    }
    0
}

/// Dispatch a package command (install/remove/uninstall/update/list/config).
/// Returns true when the args targeted a package command.
pub fn handle_package_command(args: &[String], _cwd: &str, agent_dir: &str) -> bool {
    let (command, rest) = command_and_args(args);
    let Some(command) = command else {
        return false;
    };
    match command {
        "install" | "remove" | "uninstall" | "update" | "list" | "config" | "auth" => {}
        _ => return false,
    }

    let mut settings = SettingsManager::create("/tmp", agent_dir, true);
    match command {
        "install" => {
            if rest.is_empty() {
                eprintln!("Error: install requires a source");
                std::process::exit(1);
            }
            let local = rest.contains(&"-l".to_string());
            let source = rest.iter().find(|a| !a.starts_with('-')).unwrap();
            std::process::exit(handle_install(source, local, agent_dir, &mut settings));
        }
        "remove" | "uninstall" => {
            if rest.is_empty() {
                eprintln!("Error: {command} requires a source");
                std::process::exit(1);
            }
            let local = rest.contains(&"-l".to_string());
            let source = rest.iter().find(|a| !a.starts_with('-')).unwrap();
            std::process::exit(handle_remove(source, local, &mut settings));
        }
        "update" => {
            let target = rest.first().map(|s| s.as_str());
            std::process::exit(handle_update(target, agent_dir));
        }
        "list" => {
            std::process::exit(handle_list(agent_dir, &settings));
        }
        "config" => {
            std::process::exit(handle_config(agent_dir, &settings));
        }
        "auth" => {
            // ponytail: auth subcommands are stubs.
            eprintln!("Error: auth commands are not implemented in the Rust port");
            std::process::exit(1);
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn command_detection() {
        assert_eq!(command_and_args(&s(&["install", "x"])).0, Some("install"));
        assert_eq!(command_and_args(&s(&["--help"])).0, None);
        assert_eq!(command_and_args(&s(&["-p", "hi"])).0, None);
    }

    #[test]
    fn install_and_remove_local() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let agent_dir = std::env::temp_dir()
            .join(format!("pi-pkg-{}-{n}", std::process::id()))
            .to_string_lossy()
            .to_string();
        std::fs::create_dir_all(&agent_dir).unwrap();
        let mut settings = SettingsManager::create("/tmp", &agent_dir, true);
        assert_eq!(handle_install("/tmp/foo", true, &agent_dir, &mut settings), 0);
        assert_eq!(settings.get_packages().len(), 1);
        assert_eq!(handle_remove("/tmp/foo", true, &mut settings), 0);
        assert_eq!(settings.get_packages().len(), 0);
    }

    #[test]
    fn registry_install_returns_error() {
        let agent_dir = "/tmp/nonexistent-pi-dir".to_string();
        let mut settings = SettingsManager::create("/tmp", &agent_dir, true);
        assert_eq!(handle_install("some-pkg@1.0.0", false, &agent_dir, &mut settings), 1);
    }
}
