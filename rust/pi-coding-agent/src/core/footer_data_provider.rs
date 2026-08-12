//! Git branch and extension status provider, port of
//! `core/footer-data-provider.ts`.
//!
//! ponytail: the JS fs.watch/reftable watchers (branch change detection) are
//! replaced by an explicit refresh hook; the TUI footer calls
//! `refresh_branch()` on its render loop instead of receiving push events.
//! The WSL polling path is dropped (Linux CI only).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct GitPaths {
    pub repo_dir: String,
    pub common_git_dir: String,
    pub head_path: String,
}

/// Find git metadata paths by walking up from cwd. Handles regular repos
/// (.git directory) and worktrees (.git file with gitdir:).
pub fn find_git_paths(cwd: &str) -> Option<GitPaths> {
    let mut dir = cwd.to_string();
    loop {
        let git_path = Path::new(&dir).join(".git");
        if git_path.exists() {
            let metadata = fs::metadata(&git_path).ok()?;
            if metadata.is_file() {
                let content = fs::read_to_string(&git_path).ok()?.trim().to_string();
                if let Some(git_dir_value) = content.strip_prefix("gitdir: ") {
                    let git_dir = resolve_path(&dir, git_dir_value.trim());
                    let head_path = Path::new(&git_dir).join("HEAD");
                    if !head_path.exists() {
                        return None;
                    }
                    let common_dir_path = Path::new(&git_dir).join("commondir");
                    let common_git_dir = if common_dir_path.exists() {
                        let common = fs::read_to_string(&common_dir_path).ok()?.trim().to_string();
                        resolve_path(&git_dir, &common)
                    } else {
                        git_dir
                    };
                    return Some(GitPaths {
                        repo_dir: dir,
                        common_git_dir,
                        head_path: head_path.to_string_lossy().to_string(),
                    });
                }
            } else if metadata.is_dir() {
                let head_path = git_path.join("HEAD");
                if !head_path.exists() {
                    return None;
                }
                return Some(GitPaths {
                    repo_dir: dir,
                    common_git_dir: git_path.to_string_lossy().to_string(),
                    head_path: head_path.to_string_lossy().to_string(),
                });
            }
        }
        let parent = Path::new(&dir).parent().map(|p| p.to_string_lossy().to_string());
        let Some(parent) = parent else {
            return None;
        };
        if parent == dir {
            return None;
        }
        dir = parent;
    }
}

fn resolve_path(base: &str, path: &str) -> String {
    crate::core::session_paths::resolve_path(
        path,
        if Path::new(path).is_absolute() { None } else { Some(base) },
    )
}

/// Ask git for the current branch; None on detached HEAD or unavailable git.
fn resolve_branch_with_git_sync(repo_dir: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["--no-optional-locks", "symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(repo_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn resolve_git_branch_sync(git_paths: &GitPaths) -> Option<String> {
    let content = fs::read_to_string(&git_paths.head_path).ok()?.trim().to_string();
    if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
        if branch == ".invalid" {
            return Some(resolve_branch_with_git_sync(&git_paths.repo_dir).unwrap_or_else(|| "detached".to_string()));
        }
        return Some(branch.to_string());
    }
    Some("detached".to_string())
}

/// Provides git branch and extension statuses for the footer.
pub struct FooterDataProvider {
    cwd: String,
    extension_statuses: HashMap<String, String>,
    cached_branch: Option<Option<String>>, // None = not resolved yet
    git_paths: Option<GitPaths>,
    branch_change_callbacks: Vec<Box<dyn Fn() + Send + Sync>>,
    available_provider_count: usize,
    disposed: bool,
}

impl FooterDataProvider {
    pub fn new(cwd: String) -> Self {
        let git_paths = find_git_paths(&cwd);
        Self {
            cwd,
            extension_statuses: HashMap::new(),
            cached_branch: None,
            git_paths,
            branch_change_callbacks: Vec::new(),
            available_provider_count: 0,
            disposed: false,
        }
    }

    /// Current git branch: None if not in a repo, "detached" if detached HEAD.
    pub fn get_git_branch(&mut self) -> Option<String> {
        if self.cached_branch.is_none() {
            self.cached_branch = Some(match &self.git_paths {
                Some(git_paths) => resolve_git_branch_sync(git_paths),
                None => None,
            });
        }
        self.cached_branch.clone().flatten()
    }

    /// Force a re-read on the next get_git_branch (replaces the JS watchers).
    pub fn refresh_branch(&mut self) {
        if self.cached_branch.is_some() {
            let previous = self.cached_branch.clone().flatten();
            self.cached_branch = Some(match &self.git_paths {
                Some(git_paths) => resolve_git_branch_sync(git_paths),
                None => None,
            });
            if previous != self.cached_branch.clone().flatten() {
                self.notify_branch_change();
            }
        }
    }

    pub fn get_extension_statuses(&self) -> &HashMap<String, String> {
        &self.extension_statuses
    }

    pub fn on_branch_change(&mut self, callback: Box<dyn Fn() + Send + Sync>) {
        self.branch_change_callbacks.push(callback);
    }

    pub fn set_extension_status(&mut self, key: &str, text: Option<&str>) {
        match text {
            Some(text) => {
                self.extension_statuses.insert(key.to_string(), text.to_string());
            }
            None => {
                self.extension_statuses.remove(key);
            }
        }
    }

    pub fn clear_extension_statuses(&mut self) {
        self.extension_statuses.clear();
    }

    pub fn get_available_provider_count(&self) -> usize {
        self.available_provider_count
    }

    pub fn set_available_provider_count(&mut self, count: usize) {
        self.available_provider_count = count;
    }

    pub fn set_cwd(&mut self, cwd: &str) {
        if self.cwd == cwd {
            return;
        }
        self.cwd = cwd.to_string();
        self.cached_branch = None;
        self.git_paths = find_git_paths(cwd);
        self.notify_branch_change();
    }

    pub fn dispose(&mut self) {
        self.disposed = true;
        self.branch_change_callbacks.clear();
    }

    fn notify_branch_change(&self) {
        for callback in &self.branch_change_callbacks {
            callback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("pi-footer-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn finds_regular_repo() {
        let dir = temp_dir();
        std::fs::create_dir_all(Path::new(&dir).join(".git")).unwrap();
        std::fs::write(Path::new(&dir).join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let sub = Path::new(&dir).join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let paths = find_git_paths(&sub.to_string_lossy()).unwrap();
        assert_eq!(paths.repo_dir, dir);
        assert!(paths.head_path.ends_with(".git/HEAD"));

        let mut provider = FooterDataProvider::new(sub.to_string_lossy().to_string());
        assert_eq!(provider.get_git_branch().as_deref(), Some("main"));
    }

    #[test]
    fn detached_head() {
        let dir = temp_dir();
        std::fs::create_dir_all(Path::new(&dir).join(".git")).unwrap();
        std::fs::write(Path::new(&dir).join(".git/HEAD"), "abc123def\n").unwrap();

        let mut provider = FooterDataProvider::new(dir.clone());
        assert_eq!(provider.get_git_branch().as_deref(), Some("detached"));
    }

    #[test]
    fn not_a_repo() {
        let dir = temp_dir();
        let mut provider = FooterDataProvider::new(dir);
        assert_eq!(provider.get_git_branch(), None);
    }

    #[test]
    fn worktree_gitdir_file() {
        let dir = temp_dir();
        let worktree = Path::new(&dir).join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: ../.git/worktrees/wt\n").unwrap();
        std::fs::create_dir_all(Path::new(&dir).join(".git/worktrees/wt")).unwrap();
        std::fs::write(Path::new(&dir).join(".git/worktrees/wt/HEAD"), "ref: refs/heads/feature\n").unwrap();
        std::fs::write(Path::new(&dir).join(".git/worktrees/wt/commondir"), "../../\n").unwrap();

        let paths = find_git_paths(&worktree.to_string_lossy()).unwrap();
        assert_eq!(paths.repo_dir, worktree.to_string_lossy());
        assert_eq!(paths.common_git_dir, dir.clone() + "/.git");

        let mut provider = FooterDataProvider::new(worktree.to_string_lossy().to_string());
        assert_eq!(provider.get_git_branch().as_deref(), Some("feature"));
    }

    #[test]
    fn extension_statuses() {
        let mut provider = FooterDataProvider::new(temp_dir());
        provider.set_extension_status("key", Some("busy"));
        assert_eq!(provider.get_extension_statuses().get("key").map(|v| v.as_str()), Some("busy"));
        provider.set_extension_status("key", None);
        assert!(provider.get_extension_statuses().is_empty());
    }
}
