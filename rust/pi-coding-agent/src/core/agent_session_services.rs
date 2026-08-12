//! Cwd-bound runtime services factory, port of `core/agent-session-services.ts`.
//! Extension flag application and pending provider registrations are deferred;
//! the service assembly (cwd/agentDir resolution, ModelRuntime, settings,
//! resource loader) is ported.

use super::agent_session_runtime::AgentSessionRuntimeDiagnostic;
use super::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use super::resource_loader::{DefaultResourceLoader, DefaultResourceLoaderOptions};
use super::session_paths::resolve_path;
use super::settings_manager::SettingsManager;
use crate::config::get_agent_dir;

pub struct CreateAgentSessionServicesOptions {
    pub cwd: String,
    pub agent_dir: Option<String>,
    pub settings_manager: Option<SettingsManager>,
    pub model_runtime: Option<ModelRuntime>,
    pub model_runtime_signal: Option<bool>, // ponytail: abort signals are inert
    pub extension_flag_values: Option<Vec<(String, bool_or_string::Value)>>,
    pub resource_loader_options: Option<DefaultResourceLoaderOptions>,
}

pub mod bool_or_string {
    #[derive(Clone, Debug, PartialEq)]
    pub enum Value {
        Bool(bool),
        Str(String),
    }
}

pub struct AgentSessionServices {
    pub cwd: String,
    pub agent_dir: String,
    pub model_runtime: ModelRuntime,
    pub settings_manager: SettingsManager,
    pub resource_loader: DefaultResourceLoader,
    pub diagnostics: Vec<AgentSessionRuntimeDiagnostic>,
}

fn apply_extension_flag_values(
    resource_loader: &mut DefaultResourceLoader,
    extension_flag_values: Option<&[(String, bool_or_string::Value)]>,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let Some(extension_flag_values) = extension_flag_values else {
        return Vec::new();
    };
    if extension_flag_values.is_empty() {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    // ponytail: extension flag registry lookup is deferred; unknown flags
    // report the same diagnostic, boolean flags are set, string flags accept
    // only string values.
    let mut unknown_flags: Vec<String> = Vec::new();
    for (name, value) in extension_flag_values {
        match value {
            bool_or_string::Value::Bool(_) => {
                // Registered boolean flags set to true; unregistered are unknown.
                unknown_flags.push(name.clone());
            }
            bool_or_string::Value::Str(value) => {
                diagnostics.push(AgentSessionRuntimeDiagnostic {
                    r#type: "error".into(),
                    message: format!("Extension flag \"--{name}\" requires a value"),
                });
                let _ = value;
            }
        }
    }
    if !unknown_flags.is_empty() {
        diagnostics.push(AgentSessionRuntimeDiagnostic {
            r#type: "error".into(),
            message: format!(
                "Unknown option{}: {}",
                if unknown_flags.len() == 1 { "" } else { "s" },
                unknown_flags
                    .iter()
                    .map(|name| format!("--{name}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
    }
    let _ = resource_loader;
    diagnostics
}

/// Create cwd-bound runtime services. Sync analog of createAgentSessionServices.
pub fn create_agent_session_services(
    options: CreateAgentSessionServicesOptions,
) -> Result<AgentSessionServices, String> {
    let cwd = resolve_path(&options.cwd, None);
    let agent_dir = options
        .agent_dir
        .map(|dir| resolve_path(&dir, None))
        .unwrap_or_else(get_agent_dir);
    let model_runtime = match options.model_runtime {
        Some(runtime) => runtime,
        None => ModelRuntime::create(CreateModelRuntimeOptions {
            auth_path: Some(format!("{agent_dir}/auth.json")),
            models_path: Some(format!("{agent_dir}/models.json")),
            models_store_path: None,
        }),
    };
    let settings_manager = match options.settings_manager {
        Some(manager) => manager,
        None => SettingsManager::create(&cwd, &agent_dir, true),
    };
    let mut resource_loader = DefaultResourceLoader::new(match options.resource_loader_options {
        Some(mut loader_options) => {
            loader_options.cwd = cwd.clone();
            loader_options.agent_dir = agent_dir.clone();
            loader_options.settings_manager = None;
            loader_options
        }
        None => DefaultResourceLoaderOptions {
            cwd: cwd.clone(),
            agent_dir: agent_dir.clone(),
            settings_manager: None,
            ..Default::default()
        },
    });
    resource_loader.reload();

    let diagnostics = apply_extension_flag_values(&mut resource_loader, options.extension_flag_values.as_deref());

    Ok(AgentSessionServices {
        cwd,
        agent_dir,
        model_runtime,
        settings_manager,
        resource_loader,
        diagnostics,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-svc-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn creates_services_with_defaults() {
        let agent_dir = temp_dir("agent");
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: "/tmp".into(),
            agent_dir: Some(agent_dir),
            settings_manager: None,
            model_runtime: None,
            model_runtime_signal: None,
            extension_flag_values: None,
            resource_loader_options: None,
        })
        .unwrap();
        assert_eq!(services.cwd, "/tmp");
        assert!(services.diagnostics.is_empty());
    }

    #[test]
    fn unknown_flags_reported() {
        let agent_dir = temp_dir("agent2");
        let services = create_agent_session_services(CreateAgentSessionServicesOptions {
            cwd: "/tmp".into(),
            agent_dir: Some(agent_dir),
            settings_manager: None,
            model_runtime: None,
            model_runtime_signal: None,
            extension_flag_values: Some(vec![
                ("unknown-flag".into(), bool_or_string::Value::Bool(true)),
                ("needs-value".into(), bool_or_string::Value::Str("x".into())),
            ]),
            resource_loader_options: None,
        })
        .unwrap();
        assert!(
            services
                .diagnostics
                .iter()
                .any(|d| d.message.contains("Unknown option: --unknown-flag"))
        );
        assert!(
            services
                .diagnostics
                .iter()
                .any(|d| d.message.contains("needs-value"))
        );
    }
}
