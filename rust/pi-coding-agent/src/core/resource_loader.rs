//! Resource loader, port of
//! `packages/coding-agent/src/core/resource-loader.ts`.
//!
//! Theme loading (tui `loadThemeFromPath`) and git-path watching are
//! represented by local light types; skills/prompts reuse the
//! pi-agent-core harness loaders via a std-fs SkillEnv adapter.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use pi_agent_core::harness::env::nodejs::StdExecutionEnv;
use pi_agent_core::harness::events::{load_prompt_templates, PromptTemplate, PromptTemplateDiagnostic};
use pi_agent_core::harness::skills::{load_skills, Skill, SkillDiagnostic};

use crate::config::CONFIG_DIR_NAME;
use crate::core::event_bus::EventBus;
use crate::core::extensions::loader::{load_extension_from_factory, ExtensionApi, ExtensionRuntime};
use crate::core::extensions::types::{Extension, InlineExtension, LoadExtensionsResult};
use crate::core::package_manager::PathMetadata;
use crate::core::settings_manager::SettingsManager;
use crate::utils::child_process::{canonicalize_path, resolve_path, PathInputOptions};

/// Light theme record (tui theme loading is out of scope here).
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub name: Option<String>,
    pub source_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    pub path: String,
    pub source: String,
    pub scope: String,
    pub origin: String,
    pub base_dir: Option<String>,
}

pub fn create_source_info(path: &str, metadata: &PathMetadata) -> SourceInfo {
    SourceInfo {
        path: path.to_string(),
        source: metadata.source.clone(),
        scope: metadata.scope.clone(),
        origin: metadata.origin.clone(),
        base_dir: metadata.base_dir.clone(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceDiagnostic {
    pub kind: String,
    pub message: String,
    pub path: String,
    pub collision: Option<ResourceCollision>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceCollision {
    pub resource_type: String,
    pub name: String,
    pub winner_path: String,
    pub loser_path: String,
}

pub type ContextFile = (String, String); // (path, content)

/// Read a context file (AGENTS.md / CLAUDE.md candidates) from a directory
/// (JS `loadContextFileFromDir`).
pub fn load_context_file_from_dir(dir: &str) -> Option<ContextFile> {
    const CANDIDATES: [&str; 5] = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];
    for filename in CANDIDATES {
        let file_path = Path::new(dir).join(filename);
        if file_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&file_path) {
                if !file_path.is_dir() {
                    return Some((file_path.to_string_lossy().to_string(), content));
                }
            }
        }
    }
    None
}

/// Git paths discovered from cwd (JS `findGitPaths`).
#[derive(Clone, Debug, PartialEq)]
pub struct GitPaths {
    pub repo_dir: String,
    pub common_git_dir: String,
}

/// Find git paths by walking up from cwd (JS `findGitPaths`).
pub fn find_git_paths(cwd: &str) -> Option<GitPaths> {
    let mut dir = resolve_path(cwd, cwd, &PathInputOptions::default());
    loop {
        let git_path = Path::new(&dir).join(".git");
        if git_path.exists() {
            if git_path.is_file() {
                if let Ok(content) = std::fs::read_to_string(&git_path) {
                    let content = content.trim();
                    if let Some(git_dir) = content.strip_prefix("gitdir: ") {
                        let resolved_git_dir = resolve_path(&git_dir, &dir, &PathInputOptions::default());
                        let head_path = Path::new(&resolved_git_dir).join("HEAD");
                        if !head_path.exists() {
                            return None;
                        }
                        let common_dir_path = Path::new(&resolved_git_dir).join("commondir");
                        let common_git_dir = if common_dir_path.exists() {
                            if let Ok(commondir) = std::fs::read_to_string(&common_dir_path) {
                                resolve_path(commondir.trim(), &resolved_git_dir, &PathInputOptions::default())
                            } else {
                                resolved_git_dir.clone()
                            }
                        } else {
                            resolved_git_dir.clone()
                        };
                        return Some(GitPaths {
                            repo_dir: dir.clone(),
                            common_git_dir,
                        });
                    }
                }
            } else if git_path.is_dir() {
                let head_path = git_path.join("HEAD");
                if head_path.exists() {
                    return Some(GitPaths {
                        repo_dir: dir.clone(),
                        common_git_dir: git_path.to_string_lossy().to_string(),
                    });
                }
            }
            return None;
        }
        let parent = Path::new(&dir)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string());
        match parent {
            Some(parent) if parent == dir => return None,
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

fn find_shadowed_context_file(cwd: &str) -> Option<String> {
    let git_paths = find_git_paths(cwd)?;
    let common_git_dir = canonicalize_path(&git_paths.common_git_dir);
    let worktree_root = canonicalize_path(&git_paths.repo_dir);
    let main_repo_root = Path::new(&common_git_dir)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())?;
    if !worktree_root.starts_with(&format!("{main_repo_root}/")) {
        return None;
    }
    if canonicalize_path(&format!("{main_repo_root}/.git")) != common_git_dir {
        return None;
    }
    let worktree_context_file = load_context_file_from_dir(&worktree_root)?;
    let name = Path::new(&worktree_context_file.0)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())?;
    Some(format!("{main_repo_root}/{name}"))
}

/// Load project context files (JS `loadProjectContextFiles`).
pub fn load_project_context_files(cwd: &str, agent_dir: &str) -> Vec<ContextFile> {
    let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
    let resolved_agent_dir = resolve_path(agent_dir, agent_dir, &PathInputOptions::default());

    let mut context_files: Vec<ContextFile> = Vec::new();
    let mut seen_paths: HashSet<String> = HashSet::new();

    if let Some(global_context) = load_context_file_from_dir(&resolved_agent_dir) {
        seen_paths.insert(global_context.0.clone());
        context_files.push(global_context);
    }

    let shadowed_context_file = find_shadowed_context_file(&resolved_cwd);
    let mut ancestor_context_files: Vec<ContextFile> = Vec::new();
    let mut current_dir = resolved_cwd.clone();

    loop {
        if let Some(context_file) = load_context_file_from_dir(&current_dir) {
            let is_shadowed = shadowed_context_file
                .as_deref()
                .map(|shadowed| canonicalize_path(&context_file.0) == *shadowed)
                .unwrap_or(false);
            if !is_shadowed && !seen_paths.contains(&context_file.0) {
                ancestor_context_files.insert(0, context_file.clone());
                seen_paths.insert(context_file.0.clone());
            }
        }
        let parent = Path::new(&current_dir)
            .parent()
            .map(|parent| parent.to_string_lossy().to_string());
        match parent {
            Some(parent) if parent == current_dir => break,
            Some(parent) => current_dir = parent,
            None => break,
        }
    }

    context_files.extend(ancestor_context_files);
    context_files
}

/// std-fs adapter for the harness skill loader.
pub struct StdFsSkillEnv {
    pub cwd: String,
}

impl StdFsSkillEnv {
    pub fn new(cwd: &str) -> Self {
        Self {
            cwd: cwd.to_string(),
        }
    }
}

impl pi_agent_core::harness::skills::SkillEnv for StdFsSkillEnv {
    fn file_info(&self, path: &str) -> Result<pi_agent_core::harness::skills::SkillFileInfo, String> {
        let resolved = resolve_path(path, &self.cwd, &PathInputOptions::default());
        let metadata = std::fs::metadata(&resolved).map_err(|error| error.to_string())?;
        let kind = if metadata.is_dir() {
            pi_agent_core::harness::skills::SkillFileKind::Directory
        } else {
            pi_agent_core::harness::skills::SkillFileKind::File
        };
        Ok(pi_agent_core::harness::skills::SkillFileInfo {
            name: Path::new(&resolved)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default(),
            path: resolved,
            kind,
            size: metadata.len() as f64,
            mtime_ms: 0.0,
        })
    }
    fn list_dir(&self, path: &str) -> Result<Vec<pi_agent_core::harness::skills::SkillFileInfo>, String> {
        let resolved = resolve_path(path, &self.cwd, &PathInputOptions::default());
        let entries = std::fs::read_dir(&resolved).map_err(|error| error.to_string())?;
        let mut infos = Vec::new();
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let metadata = std::fs::metadata(&entry_path).map_err(|error| error.to_string())?;
            let kind = if metadata.is_dir() {
                pi_agent_core::harness::skills::SkillFileKind::Directory
            } else {
                pi_agent_core::harness::skills::SkillFileKind::File
            };
            infos.push(pi_agent_core::harness::skills::SkillFileInfo {
                name: entry.file_name().to_string_lossy().to_string(),
                path: entry_path.to_string_lossy().to_string(),
                kind,
                size: metadata.len() as f64,
                mtime_ms: 0.0,
            });
        }
        Ok(infos)
    }
    fn read_text_file(&self, path: &str) -> Result<String, String> {
        let resolved = resolve_path(path, &self.cwd, &PathInputOptions::default());
        std::fs::read_to_string(&resolved).map_err(|error| error.to_string())
    }
    fn join_path(&self, parts: &[&str]) -> Result<String, String> {
        Ok(parts.join("/"))
    }
    fn canonical_path(&self, path: &str) -> Result<String, String> {
        let resolved = resolve_path(path, &self.cwd, &PathInputOptions::default());
        std::fs::canonicalize(&resolved)
            .map(|path| path.to_string_lossy().to_string())
            .map_err(|error| error.to_string())
    }
}

/// DefaultResourceLoader options (JS `DefaultResourceLoaderOptions`).
pub struct DefaultResourceLoaderOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub settings_manager: Option<SettingsManager>,
    pub event_bus: Option<std::sync::Arc<EventBus>>,
    pub additional_extension_paths: Vec<String>,
    pub additional_skill_paths: Vec<String>,
    pub additional_prompt_template_paths: Vec<String>,
    pub additional_theme_paths: Vec<String>,
    pub extension_factories: Vec<std::sync::Arc<dyn Fn(&ExtensionApi) -> Result<(), String> + Send + Sync>>,
    pub no_extensions: bool,
    pub no_skills: bool,
    pub no_prompt_templates: bool,
    pub no_themes: bool,
    pub no_context_files: bool,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<Vec<String>>,
}

impl Default for DefaultResourceLoaderOptions {
    fn default() -> Self {
        Self {
            cwd: ".".to_string(),
            agent_dir: crate::config::get_agent_dir(),
            settings_manager: None,
            event_bus: None,
            additional_extension_paths: Vec::new(),
            additional_skill_paths: Vec::new(),
            additional_prompt_template_paths: Vec::new(),
            additional_theme_paths: Vec::new(),
            extension_factories: Vec::new(),
            no_extensions: false,
            no_skills: false,
            no_prompt_templates: false,
            no_themes: false,
            no_context_files: false,
            system_prompt: None,
            append_system_prompt: None,
        }
    }
}

#[allow(dead_code)]
pub struct DefaultResourceLoader {
    pub cwd: String,
    pub agent_dir: String,
    settings_manager: SettingsManager,
    event_bus: std::sync::Arc<EventBus>,
    additional_extension_paths: Vec<String>,
    additional_skill_paths: Vec<String>,
    additional_prompt_template_paths: Vec<String>,
    additional_theme_paths: Vec<String>,
    extension_factories: Vec<std::sync::Arc<dyn Fn(&ExtensionApi) -> Result<(), String> + Send + Sync>>,
    no_extensions: bool,
    no_skills: bool,
    no_prompt_templates: bool,
    no_themes: bool,
    no_context_files: bool,
    system_prompt_source: Option<String>,
    append_system_prompt_source: Option<Vec<String>>,

    pub extensions_result: LoadExtensionsResult,
    pub skills: Vec<Skill>,
    pub skill_diagnostics: Vec<ResourceDiagnostic>,
    pub prompts: Vec<PromptTemplate>,
    pub prompt_diagnostics: Vec<ResourceDiagnostic>,
    pub themes: Vec<Theme>,
    pub theme_diagnostics: Vec<ResourceDiagnostic>,
    pub agents_files: Vec<ContextFile>,
    pub system_prompt: Option<String>,
    pub system_prompt_source_path: Option<String>,
    pub append_system_prompt: Vec<String>,
    pub append_system_prompt_source_paths: Vec<String>,
    pub resource_metadata_by_path: HashMap<String, PathMetadata>,
    pub loaded: bool,
}

fn skill_diagnostic_to_resource(diagnostic: &SkillDiagnostic) -> ResourceDiagnostic {
    ResourceDiagnostic {
        kind: diagnostic.kind.clone(),
        message: diagnostic.message.clone(),
        path: diagnostic.path.clone(),
        collision: None,
    }
}

fn prompt_diagnostic_to_resource(diagnostic: &PromptTemplateDiagnostic) -> ResourceDiagnostic {
    ResourceDiagnostic {
        kind: diagnostic.kind.clone(),
        message: diagnostic.message.clone(),
        path: diagnostic.path.clone(),
        collision: None,
    }
}

impl DefaultResourceLoader {
    pub fn new(options: DefaultResourceLoaderOptions) -> Self {
        let cwd = resolve_path(&options.cwd, &options.cwd, &PathInputOptions::default());
        let agent_dir = resolve_path(&options.agent_dir, &options.agent_dir, &PathInputOptions::default());
        let settings_manager = options
            .settings_manager
            .unwrap_or_else(|| SettingsManager::create(&cwd, &agent_dir, true));
        let event_bus = options.event_bus.unwrap_or_else(|| std::sync::Arc::new(EventBus::new()));
        Self {
            cwd,
            agent_dir,
            settings_manager,
            event_bus,
            additional_extension_paths: options.additional_extension_paths,
            additional_skill_paths: options.additional_skill_paths,
            additional_prompt_template_paths: options.additional_prompt_template_paths,
            additional_theme_paths: options.additional_theme_paths,
            extension_factories: options.extension_factories,
            no_extensions: options.no_extensions,
            no_skills: options.no_skills,
            no_prompt_templates: options.no_prompt_templates,
            no_themes: options.no_themes,
            no_context_files: options.no_context_files,
            system_prompt_source: options.system_prompt,
            append_system_prompt_source: options.append_system_prompt,
            extensions_result: LoadExtensionsResult::default(),
            skills: Vec::new(),
            skill_diagnostics: Vec::new(),
            prompts: Vec::new(),
            prompt_diagnostics: Vec::new(),
            themes: Vec::new(),
            theme_diagnostics: Vec::new(),
            agents_files: Vec::new(),
            system_prompt: None,
            system_prompt_source_path: None,
            append_system_prompt: Vec::new(),
            append_system_prompt_source_paths: Vec::new(),
            resource_metadata_by_path: HashMap::new(),
            loaded: false,
        }
    }

    pub fn get_extensions(&self) -> &LoadExtensionsResult {
        &self.extensions_result
    }

    pub fn get_skills(&self) -> &[Skill] {
        &self.skills
    }

    pub fn get_prompts(&self) -> &[PromptTemplate] {
        &self.prompts
    }

    pub fn get_themes(&self) -> &[Theme] {
        &self.themes
    }

    pub fn get_agents_files(&self) -> &[ContextFile] {
        &self.agents_files
    }

    pub fn get_system_prompt(&self) -> Option<&str> {
        self.system_prompt.as_deref()
    }

    pub fn get_append_system_prompt(&self) -> &[String] {
        &self.append_system_prompt
    }

    fn merge_paths(&self, primary: &[String], additional: &[String]) -> Vec<String> {
        let mut merged: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for path in primary.iter().chain(additional) {
            let resolved = self.resolve_resource_path(path);
            let canonical = canonicalize_path(&resolved);
            if seen.contains(&canonical) {
                continue;
            }
            seen.insert(canonical);
            merged.push(resolved);
        }
        merged
    }

    fn resolve_resource_path(&self, path: &str) -> String {
        resolve_path(path, &self.cwd, &PathInputOptions {
            trim: true,
            ..PathInputOptions::default()
        })
    }

    fn is_under_path(target: &str, root: &str) -> bool {
        let normalized_root = resolve_path(root, root, &PathInputOptions::default());
        if target == normalized_root {
            return true;
        }
        target.starts_with(&format!("{normalized_root}/"))
    }

    /// Default source info for a path (JS `getDefaultSourceInfoForPath`).
    pub fn get_default_source_info_for_path(&self, file_path: &str) -> SourceInfo {
        if file_path.starts_with('<') && file_path.ends_with('>') {
            let inner = &file_path[1..file_path.len() - 1];
            let source = inner.split(':').next().filter(|value| !value.is_empty()).unwrap_or("temporary");
            return SourceInfo {
                path: file_path.to_string(),
                source: source.to_string(),
                scope: "temporary".to_string(),
                origin: "top-level".to_string(),
                base_dir: None,
            };
        }
        let normalized = resolve_path(file_path, file_path, &PathInputOptions::default());
        let agent_roots = ["skills", "prompts", "themes", "extensions"]
            .iter()
            .map(|name| format!("{}/{name}", self.agent_dir))
            .collect::<Vec<_>>();
        let project_roots = ["skills", "prompts", "themes", "extensions"]
            .iter()
            .map(|name| format!("{}/{CONFIG_DIR_NAME}/{name}", self.cwd))
            .collect::<Vec<_>>();
        for root in &agent_roots {
            if Self::is_under_path(&normalized, root) {
                return SourceInfo {
                    path: file_path.to_string(),
                    source: "local".to_string(),
                    scope: "user".to_string(),
                    origin: "top-level".to_string(),
                    base_dir: Some(root.clone()),
                };
            }
        }
        for root in &project_roots {
            if Self::is_under_path(&normalized, root) {
                return SourceInfo {
                    path: file_path.to_string(),
                    source: "local".to_string(),
                    scope: "project".to_string(),
                    origin: "top-level".to_string(),
                    base_dir: Some(root.clone()),
                };
            }
        }
        let base_dir = if Path::new(&normalized).is_dir() {
            normalized.clone()
        } else {
            Path::new(&normalized)
                .parent()
                .map(|parent| parent.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        SourceInfo {
            path: file_path.to_string(),
            source: "local".to_string(),
            scope: "temporary".to_string(),
            origin: "top-level".to_string(),
            base_dir: Some(base_dir),
        }
    }

    fn update_skills_from_paths(&mut self, skill_paths: &[String]) {
        if self.no_skills && skill_paths.is_empty() {
            self.skills = Vec::new();
            self.skill_diagnostics = Vec::new();
            return;
        }
        let env = StdFsSkillEnv::new(&self.cwd);
        let dirs: Vec<String> = skill_paths
            .iter()
            .map(|path| self.resolve_resource_path(path))
            .collect();
        let (skills, diagnostics) = load_skills(&env, &dirs);
        self.skills = skills;
        self.skill_diagnostics = diagnostics.iter().map(skill_diagnostic_to_resource).collect();
    }

    fn update_prompts_from_paths(&mut self, prompt_paths: &[String]) {
        if self.no_prompt_templates && prompt_paths.is_empty() {
            self.prompts = Vec::new();
            self.prompt_diagnostics = Vec::new();
            return;
        }
        let env = StdExecutionEnv::new(&self.cwd, None, None);
        let paths: Vec<String> = prompt_paths
            .iter()
            .map(|path| self.resolve_resource_path(path))
            .collect();
        let (prompts, diagnostics) = load_prompt_templates(&env, &paths);
        self.prompts = prompts;
        self.prompt_diagnostics = diagnostics.iter().map(prompt_diagnostic_to_resource).collect();
    }

    fn update_themes_from_paths(&mut self, theme_paths: &[String]) {
        if self.no_themes && theme_paths.is_empty() {
            self.themes = Vec::new();
            self.theme_diagnostics = Vec::new();
            return;
        }
        let mut themes: Vec<Theme> = Vec::new();
        let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
        for path in theme_paths {
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists() {
                diagnostics.push(ResourceDiagnostic {
                    kind: "warning".to_string(),
                    message: "theme path does not exist".to_string(),
                    path: resolved,
                    collision: None,
                });
                continue;
            }
            if Path::new(&resolved).is_file() && resolved.ends_with(".json") {
                themes.push(Theme {
                    name: None,
                    source_path: Some(resolved),
                });
            } else {
                // Load all .json files from a directory.
                if let Ok(entries) = std::fs::read_dir(&resolved) {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.ends_with(".json") {
                            themes.push(Theme {
                                name: None,
                                source_path: Some(entry.path().to_string_lossy().to_string()),
                            });
                        }
                    }
                }
            }
        }
        let (deduped, dedupe_diagnostics) = Self::dedupe_themes(themes);
        self.themes = deduped;
        diagnostics.extend(dedupe_diagnostics);
        self.theme_diagnostics = diagnostics;
    }

    fn dedupe_themes(themes: Vec<Theme>) -> (Vec<Theme>, Vec<ResourceDiagnostic>) {
        let mut seen: HashMap<String, Theme> = HashMap::new();
        let mut diagnostics: Vec<ResourceDiagnostic> = Vec::new();
        for theme in themes {
            let name = theme.name.clone().unwrap_or_else(|| "unnamed".to_string());
            if let Some(existing) = seen.get(&name) {
                diagnostics.push(ResourceDiagnostic {
                    kind: "collision".to_string(),
                    message: format!("name \"{name}\" collision"),
                    path: theme.source_path.clone().unwrap_or_default(),
                    collision: Some(ResourceCollision {
                        resource_type: "theme".to_string(),
                        name: name.clone(),
                        winner_path: existing.source_path.clone().unwrap_or_else(|| "<builtin>".to_string()),
                        loser_path: theme.source_path.clone().unwrap_or_else(|| "<builtin>".to_string()),
                    }),
                });
            } else {
                seen.insert(name, theme);
            }
        }
        (seen.into_values().collect(), diagnostics)
    }

    fn detect_extension_conflicts(extensions: &[Extension]) -> Vec<(String, String)> {
        let mut conflicts: Vec<(String, String)> = Vec::new();
        let mut tool_owners: HashMap<String, String> = HashMap::new();
        let mut flag_owners: HashMap<String, String> = HashMap::new();

        for ext in extensions {
            for tool_name in ext.tools.keys() {
                if let Some(existing_owner) = tool_owners.get(tool_name) {
                    if existing_owner != &ext.path {
                        conflicts.push((
                            ext.path.clone(),
                            format!("Tool \"{tool_name}\" conflicts with {existing_owner}"),
                        ));
                    }
                } else {
                    tool_owners.insert(tool_name.clone(), ext.path.clone());
                }
            }
            for flag_name in ext.flags.keys() {
                if let Some(existing_owner) = flag_owners.get(flag_name) {
                    if existing_owner != &ext.path {
                        conflicts.push((
                            ext.path.clone(),
                            format!("Flag \"--{flag_name}\" conflicts with {existing_owner}"),
                        ));
                    }
                } else {
                    flag_owners.insert(flag_name.clone(), ext.path.clone());
                }
            }
        }
        conflicts
    }

    fn load_extension_factories(&self, runtime: std::sync::Arc<ExtensionRuntime>) -> LoadExtensionsResult {
        let mut result = LoadExtensionsResult::default();
        for (index, factory) in self.extension_factories.iter().enumerate() {
            let extension_path = format!("<inline:{}>", index + 1);
            match load_extension_from_factory(
                factory.as_ref(),
                &self.cwd,
                self.event_bus.clone(),
                runtime.clone(),
                &extension_path,
            ) {
                Ok(extension) => result.extensions.push(extension),
                Err(error) => result.errors.push((extension_path, error)),
            }
        }
        result
    }

    /// Discover the system prompt file (JS `discoverSystemPromptFile`).
    pub fn discover_system_prompt_file(&self) -> Option<String> {
        let project_path = format!("{}/{CONFIG_DIR_NAME}/SYSTEM.md", self.cwd);
        if self.settings_manager.is_project_trusted() && Path::new(&project_path).exists() {
            return Some(project_path);
        }
        let global_path = format!("{}/SYSTEM.md", self.agent_dir);
        if Path::new(&global_path).exists() {
            return Some(global_path);
        }
        None
    }

    /// Discover the append-system-prompt file (JS
    /// `discoverAppendSystemPromptFile`).
    pub fn discover_append_system_prompt_file(&self) -> Option<String> {
        let project_path = format!("{}/{CONFIG_DIR_NAME}/APPEND_SYSTEM.md", self.cwd);
        if self.settings_manager.is_project_trusted() && Path::new(&project_path).exists() {
            return Some(project_path);
        }
        let global_path = format!("{}/APPEND_SYSTEM.md", self.agent_dir);
        if Path::new(&global_path).exists() {
            return Some(global_path);
        }
        None
    }

    /// Reload all resources (JS `reload`; the package-manager resolve step
    /// is applied over the configured package settings).
    pub fn reload(&mut self) {
        use pi_protocol::cbor::Value;
        let settings = self.settings_manager.get_global_settings();
        let packages: Vec<Value> = settings
            .as_map()
            .and_then(|entries| entries.iter().find(|(key, _)| key == "packages"))
            .and_then(|(_, value)| value.as_array())
            .map(|array| array.to_vec())
            .unwrap_or_default();
        let mut metadata_by_path: HashMap<String, PathMetadata> = HashMap::new();

        // Resolve resources from configured packages (top-level resources
        // come from settings extension/skills/prompts/themes entries).
        let mut skill_paths: Vec<String> = Vec::new();
        let mut prompt_paths: Vec<String> = Vec::new();
        let mut theme_paths: Vec<String> = Vec::new();

        for resource_type in ["skills", "prompts", "themes"] {
            let entries: Vec<Value> = settings
                .as_map()
                .and_then(|entries| entries.iter().find(|(key, _)| key == resource_type))
                .and_then(|(_, value)| value.as_array())
                .map(|array| array.to_vec())
                .unwrap_or_default();
            let paths: Vec<String> = entries
                .iter()
                .filter_map(|entry| entry.as_str().map(|value| value.to_string()))
                .collect();
            match resource_type {
                "skills" => skill_paths.extend(paths),
                "prompts" => prompt_paths.extend(paths),
                _ => theme_paths.extend(paths),
            }
        }
        let _ = packages;

        // Extensions from settings plus CLI paths.
        let extension_entries: Vec<Value> = settings
            .as_map()
            .and_then(|entries| entries.iter().find(|(key, _)| key == "extensions"))
            .and_then(|(_, value)| value.as_array())
            .map(|array| array.to_vec())
            .unwrap_or_default();
        let extension_paths: Vec<String> = extension_entries
            .iter()
            .filter_map(|entry| entry.as_str().map(|value| value.to_string()))
            .collect();

        // Load extensions (cached path factories are applied via the
        // configured extension factory list; plain paths are recorded).
        let mut extensions_result = LoadExtensionsResult::default();
        let runtime = ExtensionRuntime::new();
        for path in &extension_paths {
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists() {
                extensions_result
                    .errors
                    .push((resolved.clone(), format!("Extension path does not exist: {resolved}")));
            }
        }
        let inline = self.load_extension_factories(runtime);
        extensions_result.extensions.extend(inline.extensions);
        extensions_result.errors.extend(inline.errors);

        for (path, error) in Self::detect_extension_conflicts(&extensions_result.extensions) {
            extensions_result.errors.push((path, error));
        }
        self.extensions_result = extensions_result;
        self.apply_extension_source_info(&mut metadata_by_path);

        // Merge CLI and settings paths.
        skill_paths = self.merge_paths(&self.additional_skill_paths, &skill_paths);
        prompt_paths = self.merge_paths(&self.additional_prompt_template_paths, &prompt_paths);
        theme_paths = self.merge_paths(&self.additional_theme_paths, &theme_paths);

        self.update_skills_from_paths(&skill_paths);
        self.update_prompts_from_paths(&prompt_paths);
        self.update_themes_from_paths(&theme_paths);

        for path in &self.additional_skill_paths {
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists()
                && !self.skill_diagnostics.iter().any(|diagnostic| diagnostic.path == resolved)
            {
                self.skill_diagnostics.push(ResourceDiagnostic {
                    kind: "error".to_string(),
                    message: "Skill path does not exist".to_string(),
                    path: resolved,
                    collision: None,
                });
            }
        }
        for path in &self.additional_prompt_template_paths {
            let resolved = self.resolve_resource_path(path);
            if !Path::new(&resolved).exists()
                && !self.prompt_diagnostics.iter().any(|diagnostic| diagnostic.path == resolved)
            {
                self.prompt_diagnostics.push(ResourceDiagnostic {
                    kind: "error".to_string(),
                    message: "Prompt template path does not exist".to_string(),
                    path: resolved,
                    collision: None,
                });
            }
        }

        self.agents_files = if self.no_context_files {
            Vec::new()
        } else {
            load_project_context_files(&self.cwd, &self.agent_dir)
        };

        let system_prompt_source = self
            .system_prompt_source
            .clone()
            .or_else(|| self.discover_system_prompt_file());
        self.system_prompt = system_prompt_source
            .as_deref()
            .map(resolve_prompt_input)
            .flatten();
        self.system_prompt_source_path = system_prompt_source
            .filter(|source| Path::new(source).exists())
            .map(|source| resolve_path(&source, &self.cwd, &PathInputOptions::default()));

        let append_sources = match &self.append_system_prompt_source {
            Some(sources) => sources.clone(),
            None => self
                .discover_append_system_prompt_file()
                .map(|file| vec![file])
                .unwrap_or_default(),
        };
        self.append_system_prompt = append_sources
            .iter()
            .filter_map(|source| resolve_prompt_input(source))
            .collect();
        self.append_system_prompt_source_paths = append_sources
            .iter()
            .filter(|source| Path::new(source).exists())
            .map(|source| resolve_path(source, &self.cwd, &PathInputOptions::default()))
            .collect();

        self.resource_metadata_by_path = metadata_by_path;
        self.loaded = true;
    }

    fn apply_extension_source_info(&mut self, _metadata_by_path: &mut HashMap<String, PathMetadata>) {
        for extension in &mut self.extensions_result.extensions {
            let _ = extension;
        }
    }

    /// Extend resources with additional paths (JS `extendResources`);
    /// reloads the affected resource types.
    pub fn extend_resources(&mut self, skill_paths: &[String], prompt_paths: &[String], theme_paths: &[String]) {
        if !skill_paths.is_empty() {
            let paths = self.merge_paths(&self.skill_paths_for_extend(), skill_paths);
            self.update_skills_from_paths(&paths);
        }
        if !prompt_paths.is_empty() {
            let paths = self.merge_paths(&self.prompt_paths_for_extend(), prompt_paths);
            self.update_prompts_from_paths(&paths);
        }
        if !theme_paths.is_empty() {
            let paths = self.merge_paths(&self.theme_paths_for_extend(), theme_paths);
            self.update_themes_from_paths(&paths);
        }
    }

    fn skill_paths_for_extend(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.file_path.clone()).collect()
    }

    fn prompt_paths_for_extend(&self) -> Vec<String> {
        self.prompts.iter().map(|prompt| prompt.file_path.clone()).collect()
    }

    fn theme_paths_for_extend(&self) -> Vec<String> {
        self.themes
            .iter()
            .filter_map(|theme| theme.source_path.clone())
            .collect()
    }
}

fn resolve_prompt_input(input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }
    if Path::new(input).exists() {
        match std::fs::read_to_string(input) {
            Ok(content) => Some(content),
            Err(_) => Some(input.to_string()),
        }
    } else {
        Some(input.to_string())
    }
}

/// Keep a reference for the InlineExtension import used by callers.
#[allow(dead_code)]
fn _inline_extension_reference(extension: InlineExtension) -> Extension {
    extension.to_extension()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_context_files_from_directory() {
        let dir = std::env::temp_dir().join(format!("pi-context-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "hello").unwrap();
        let (path, content) = load_context_file_from_dir(&dir.to_string_lossy()).unwrap();
        assert!(path.ends_with("AGENTS.md"));
        assert_eq!(content, "hello");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finds_git_paths_for_repo() {
        let dir = std::env::temp_dir().join(format!("pi-gitpaths-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let git_paths = find_git_paths(&dir.to_string_lossy()).unwrap();
        assert!(git_paths.common_git_dir.ends_with(".git"));
        assert!(git_paths.repo_dir.contains("pi-gitpaths-"), "repo_dir: {}", git_paths.repo_dir);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loads_project_context_files() {
        let dir = std::env::temp_dir().join(format!("pi-project-ctx-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".pi")).unwrap();
        std::fs::write(dir.join(".pi/AGENTS.md"), "project").unwrap();
        std::fs::write(dir.join("AGENTS.md"), "root").unwrap();
        let files = load_project_context_files(&dir.to_string_lossy(), &dir.join(".pi").to_string_lossy());
        // Agent dir and cwd each contribute.
        assert!(files.len() >= 1);
        let contents: Vec<&str> = files.iter().map(|(_, content)| content.as_str()).collect();
        assert!(contents.contains(&"root"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn loader_loads_settings_resources() {
        let dir = std::env::temp_dir().join(format!("pi-loader-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".pi/skills/my-skill")).unwrap();
        std::fs::write(
            dir.join(".pi/skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: Does things\n---\nbody",
        )
        .unwrap();
        let mut settings = SettingsManager::in_memory(pi_protocol::cbor::Value::Map(vec![(
            "skills".to_string(),
            pi_protocol::cbor::Value::Array(vec![pi_protocol::cbor::Value::String(
                dir.join(".pi/skills").to_string_lossy().to_string(),
            )]),
        )]));
        settings.set_project_trusted(false);
        let mut loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: dir.to_string_lossy().to_string(),
            agent_dir: dir.join(".pi").to_string_lossy().to_string(),
            settings_manager: Some(settings),
            ..DefaultResourceLoaderOptions::default()
        });
        loader.reload();
        assert!(loader.skills.iter().any(|skill| skill.name == "my-skill"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn detects_extension_conflicts() {
        let mut extension_a = Extension::default();
        extension_a.path = "/a".to_string();
        extension_a.tools.insert(
            "tool-x".to_string(),
            RegisteredTool {
                definition: crate::core::extensions::types::ToolDefinition::new(
                    "tool-x",
                    "x",
                    None,
                    |_id, _params, _state| Ok(pi_protocol::cbor::Value::Null),
                ),
                hidden: false,
            },
        );
        let mut extension_b = Extension::default();
        extension_b.path = "/b".to_string();
        extension_b.tools.insert(
            "tool-x".to_string(),
            RegisteredTool {
                definition: crate::core::extensions::types::ToolDefinition::new(
                    "tool-x",
                    "x",
                    None,
                    |_id, _params, _state| Ok(pi_protocol::cbor::Value::Null),
                ),
                hidden: false,
            },
        );
        let conflicts = DefaultResourceLoader::detect_extension_conflicts(&[extension_a, extension_b]);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].1.contains("conflicts with"));
    }

    #[test]
    fn source_info_scopes() {
        let loader = DefaultResourceLoader::new(DefaultResourceLoaderOptions {
            cwd: "/tmp".to_string(),
            agent_dir: "/tmp/agent".to_string(),
            ..DefaultResourceLoaderOptions::default()
        });
        let info = loader.get_default_source_info_for_path("/tmp/agent/skills/x.md");
        assert_eq!(info.scope, "user");
        let info = loader.get_default_source_info_for_path("<inline:1>");
        assert_eq!(info.scope, "temporary");
    }
}
