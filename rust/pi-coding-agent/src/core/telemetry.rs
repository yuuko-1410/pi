//! Install telemetry flag, port of `core/telemetry.ts`.

use super::settings_manager::SettingsManager;

fn is_truthy_env_flag(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(value) => {
            let value = value.to_lowercase();
            value == "1" || value == "true" || value == "yes"
        }
    }
}

/// Whether install telemetry is enabled (env override wins over settings).
pub fn is_install_telemetry_enabled(
    settings_manager: &SettingsManager,
    telemetry_env: Option<&str>,
) -> bool {
    match telemetry_env {
        Some(env) => is_truthy_env_flag(Some(env)),
        None => settings_manager.get_enable_install_telemetry(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_flag_parsing() {
        assert!(is_truthy_env_flag(Some("1")));
        assert!(is_truthy_env_flag(Some("TRUE")));
        assert!(is_truthy_env_flag(Some("yes")));
        assert!(!is_truthy_env_flag(Some("0")));
        assert!(!is_truthy_env_flag(Some("no")));
        assert!(!is_truthy_env_flag(None));
    }
}
