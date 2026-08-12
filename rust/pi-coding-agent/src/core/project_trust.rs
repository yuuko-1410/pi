//! Project trust resolution, port of `core/project-trust.ts`.
//! Extension project_trust events are deferred (no extension runner emit);
//! the UI prompt path is simplified to a callback.

use super::settings_manager::SettingsManager;
use super::trust_manager::{get_project_trust_options, has_trust_requiring_project_resources, ProjectTrustStore};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AppMode {
    Interactive,
    Print,
    Json,
    Rpc,
}

pub struct ResolveProjectTrustedOptions<'a> {
    pub cwd: &'a str,
    pub trust_store: &'a ProjectTrustStore,
    pub trust_override: Option<bool>,
    pub default_project_trust: Option<&'a str>,
    pub has_ui: bool,
    /// UI selection callback: (prompt, labels) -> selected label index.
    pub select: Option<&'a dyn Fn(&str, &[String]) -> Option<usize>>,
}

fn format_project_trust_prompt(cwd: &str) -> String {
    format!(
        "Trust project folder?\n{cwd}\n\nThis allows pi to load .pi settings and resources, install missing project packages, and execute project extensions."
    )
}

/// Resolve whether a project is trusted.
pub fn resolve_project_trusted(options: &ResolveProjectTrustedOptions) -> bool {
    if let Some(trust_override) = options.trust_override {
        return trust_override;
    }
    if !has_trust_requiring_project_resources(options.cwd) {
        return true;
    }

    // Extension project_trust events are deferred; go straight to the store.
    let decision = options.trust_store.get(options.cwd);
    if decision.is_some() {
        return decision.unwrap_or(false);
    }

    match options.default_project_trust.unwrap_or("ask") {
        "always" => return true,
        "never" => return false,
        _ => {}
    }

    if !options.has_ui {
        return false;
    }

    let Some(select) = options.select else {
        return false;
    };

    let trust_options = get_project_trust_options(options.cwd, true);
    let labels: Vec<String> = trust_options.iter().map(|option| option.label.clone()).collect();
    let prompt = format_project_trust_prompt(options.cwd);
    let selected = select(&prompt, &labels);
    match selected {
        Some(index) => {
            let option = &trust_options[index];
            if !option.updates.is_empty() {
                let updates: Vec<(String, Option<bool>)> = option
                    .updates
                    .iter()
                    .map(|update| (update.path.clone(), update.decision))
                    .collect();
                let _ = options.trust_store.set_many(
                    &updates
                        .iter()
                        .map(|(path, decision)| super::trust_manager::ProjectTrustUpdate {
                            path: path.clone(),
                            decision: *decision,
                        })
                        .collect::<Vec<_>>(),
                );
            }
            option.trusted
        }
        None => false,
    }
}

/// Helper for callers holding a SettingsManager default.
pub fn default_project_trust_from_settings(settings: &SettingsManager) -> Option<String> {
    settings.get("projectTrust").and_then(|value| value.as_str().map(|value| value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_agent_dir() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-ptrust-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn override_wins() {
        let store = ProjectTrustStore::new(&temp_agent_dir());
        let options = ResolveProjectTrustedOptions {
            cwd: "/tmp",
            trust_store: &store,
            trust_override: Some(false),
            default_project_trust: None,
            has_ui: false,
            select: None,
        };
        assert!(!resolve_project_trusted(&options));
    }

    #[test]
    fn no_resources_means_trusted() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-ptrust2-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = ProjectTrustStore::new(&temp_agent_dir());
        let options = ResolveProjectTrustedOptions {
            cwd: &dir.to_string_lossy(),
            trust_store: &store,
            trust_override: None,
            default_project_trust: None,
            has_ui: false,
            select: None,
        };
        assert!(resolve_project_trusted(&options));
    }

    #[test]
    fn stored_decision_respected() {
        let agent_dir = temp_agent_dir();
        let store = ProjectTrustStore::new(&agent_dir);
        let cwd = format!("{agent_dir}/proj-store");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(format!("{cwd}/.pi/skills")).unwrap();
        store.set(&cwd, Some(false)).unwrap();
        let options = ResolveProjectTrustedOptions {
            cwd: &cwd,
            trust_store: &store,
            trust_override: None,
            default_project_trust: None,
            has_ui: false,
            select: None,
        };
        assert!(!resolve_project_trusted(&options));
    }

    #[test]
    fn default_always_never() {
        let agent_dir = temp_agent_dir();
        let store = ProjectTrustStore::new(&agent_dir);
        let cwd = format!("{agent_dir}/proj-default");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(format!("{cwd}/.pi/skills")).unwrap();
        let options = ResolveProjectTrustedOptions {
            cwd: &cwd,
            trust_store: &store,
            trust_override: None,
            default_project_trust: Some("always"),
            has_ui: false,
            select: None,
        };
        assert!(resolve_project_trusted(&options));
        let options = ResolveProjectTrustedOptions {
            cwd: &cwd,
            trust_store: &store,
            trust_override: None,
            default_project_trust: Some("never"),
            has_ui: false,
            select: None,
        };
        assert!(!resolve_project_trusted(&options));
    }

    #[test]
    fn ui_selection_saves() {
        let agent_dir = temp_agent_dir();
        let store = ProjectTrustStore::new(&agent_dir);
        let cwd = format!("{agent_dir}/proj");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(format!("{cwd}/.pi/skills")).unwrap();
        let select = |_: &str, labels: &[String]| -> Option<usize> {
            labels.iter().position(|label| label == "Trust")
        };
        let options = ResolveProjectTrustedOptions {
            cwd: &cwd,
            trust_store: &store,
            trust_override: None,
            default_project_trust: None,
            has_ui: true,
            select: Some(&select),
        };
        assert!(resolve_project_trusted(&options));
        // Decision persisted.
        assert_eq!(store.get(&cwd), Some(true));
    }

    #[test]
    fn no_ui_returns_false() {
        let agent_dir = temp_agent_dir();
        let store = ProjectTrustStore::new(&agent_dir);
        let cwd = format!("{agent_dir}/proj2");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(format!("{cwd}/.pi/skills")).unwrap();
        let options = ResolveProjectTrustedOptions {
            cwd: &cwd,
            trust_store: &store,
            trust_override: None,
            default_project_trust: None,
            has_ui: false,
            select: None,
        };
        assert!(!resolve_project_trusted(&options));
    }
}
