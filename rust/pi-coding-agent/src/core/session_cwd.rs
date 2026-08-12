//! Session cwd validation, port of `core/session-cwd.ts`.

use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct SessionCwdIssue {
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

/// Source of the active session's cwd and file path.
pub trait SessionCwdSource {
    fn get_cwd(&self) -> String;
    fn get_session_file(&self) -> Option<String>;
}

pub fn get_missing_session_cwd_issue(source: &dyn SessionCwdSource, fallback_cwd: &str) -> Option<SessionCwdIssue> {
    let session_file = source.get_session_file()?;
    let session_cwd = source.get_cwd();
    if session_cwd.is_empty() || Path::new(&session_cwd).exists() {
        return None;
    }
    Some(SessionCwdIssue {
        session_file: Some(session_file),
        session_cwd,
        fallback_cwd: fallback_cwd.to_string(),
    })
}

pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = issue
        .session_file
        .as_deref()
        .map(|file| format!("\nSession file: {file}"))
        .unwrap_or_default();
    format!(
        "Stored session working directory does not exist: {}{}\nCurrent working directory: {}",
        issue.session_cwd, session_file, issue.fallback_cwd
    )
}

pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

#[derive(Debug)]
pub struct MissingSessionCwdError {
    pub issue: SessionCwdIssue,
}

impl std::fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format_missing_session_cwd_error(&self.issue))
    }
}

impl std::error::Error for MissingSessionCwdError {}

pub fn assert_session_cwd_exists(source: &dyn SessionCwdSource, fallback_cwd: &str) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(source, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError { issue }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stub {
        cwd: String,
        session_file: Option<String>,
    }

    impl SessionCwdSource for Stub {
        fn get_cwd(&self) -> String {
            self.cwd.clone()
        }
        fn get_session_file(&self) -> Option<String> {
            self.session_file.clone()
        }
    }

    #[test]
    fn missing_cwd_reported_only_when_file_exists_and_cwd_missing() {
        let source = Stub {
            cwd: "/definitely/not/a/real/dir-xyz".into(),
            session_file: Some("s.jsonl".into()),
        };
        let issue = get_missing_session_cwd_issue(&source, "/fallback").unwrap();
        assert_eq!(issue.session_cwd, "/definitely/not/a/real/dir-xyz");
        assert_eq!(issue.fallback_cwd, "/fallback");

        // No session file -> no issue.
        let source = Stub {
            cwd: "/definitely/not/a/real/dir-xyz".into(),
            session_file: None,
        };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());

        // Existing cwd -> no issue.
        let source = Stub {
            cwd: "/tmp".into(),
            session_file: Some("s.jsonl".into()),
        };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());
    }

    #[test]
    fn error_assertion() {
        let source = Stub {
            cwd: "/definitely/not/a/real/dir-xyz".into(),
            session_file: Some("s.jsonl".into()),
        };
        let error = assert_session_cwd_exists(&source, "/fallback").unwrap_err();
        assert!(error.to_string().contains("Stored session working directory does not exist"));
    }

    #[test]
    fn prompt_format() {
        let issue = SessionCwdIssue {
            session_file: None,
            session_cwd: "/gone".into(),
            fallback_cwd: "/here".into(),
        };
        assert_eq!(
            format_missing_session_cwd_prompt(&issue),
            "cwd from session file does not exist\n/gone\n\ncontinue in current cwd\n/here"
        );
    }
}
