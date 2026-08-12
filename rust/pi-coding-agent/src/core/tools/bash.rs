//! Bash tool, port of `tools/bash.ts`. The TUI render components are
//! skipped; execute logic, spawn context resolution, and error formatting
//! are ported. Child processes run through the shell config (stdin or
//! -c transport) with process-tree kill on timeout/abort.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use pi_protocol::Value;

use super::output_accumulator::OutputAccumulator;
use super::path_utils::resolve_to_cwd;
use super::truncate::{format_size, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use crate::utils::shell::{get_shell_config, get_shell_env, kill_process_tree, track_detached_child_pid, untrack_detached_child_pid};

pub const BASH_TOOL_SYSTEM_PROMPT_CONTRIBUTION_SNIPPET: &str = "Execute bash commands (ls, grep, find, etc.)";
pub const BASH_TOOL_SYSTEM_PROMPT_GUIDELINES: [&str; 1] =
    ["You can inspect PI_* environment variables for current model and session details."];

const MAX_TIMEOUT_MS: u64 = 2_147_483_647;

fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<u64>, String> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err("Invalid timeout: must be a finite number of seconds".to_string());
    }
    let timeout_ms = (timeout * 1000.0) as u64;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(format!("Invalid timeout: maximum is {} seconds", MAX_TIMEOUT_MS / 1000));
    }
    Ok(Some(timeout_ms))
}

/// Pluggable operations for the bash tool.
pub trait BashToolOperations {
    /// Execute a command and stream raw output; return the exit code.
    fn exec(
        &self,
        command: &str,
        cwd: &str,
        on_data: std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>,
        cancelled: &AtomicBool,
        timeout_ms: Option<u64>,
    ) -> Result<Option<i64>, String>;
}

/// Local shell execution (port of createLocalBashOperations).
pub struct LocalBashOperations {
    pub shell_path: Option<String>,
}

impl LocalBashOperations {
    pub fn new(shell_path: Option<String>) -> Self {
        Self { shell_path }
    }
}

impl BashToolOperations for LocalBashOperations {
    fn exec(
        &self,
        command: &str,
        cwd: &str,
        on_data: std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>,
        cancelled: &AtomicBool,
        timeout_ms: Option<u64>,
    ) -> Result<Option<i64>, String> {
        if cancelled.load(Ordering::SeqCst) {
            return Err("aborted".to_string());
        }
        let shell_config = get_shell_config(self.shell_path.as_deref()).map_err(|error| error)?;
        if !std::path::Path::new(cwd).exists() {
            return Err(format!(
                "Working directory does not exist: {cwd}\nCannot execute bash commands."
            ));
        }

        let command_from_stdin = shell_config.command_transport.as_deref() == Some("stdin");
        let mut process = Command::new(&shell_config.shell);
        if command_from_stdin {
            process.args(&shell_config.args);
            process.stdin(Stdio::piped());
        } else {
            let mut args = shell_config.args.clone();
            args.push(command.to_string());
            process.args(&args);
            process.stdin(Stdio::null());
        }
        process
            .current_dir(cwd)
            .envs(get_shell_env().into_iter())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }

        let mut child = process.spawn().map_err(|error| error.to_string())?;
        let pid = child.id();
        if pid != 0 {
            track_detached_child_pid(pid);
        }

        let mut timed_out = false;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        // Read stdout/stderr concurrently; on_data is Fn + Send + Sync so it
        // can be shared across both reader threads. The Arc keeps the
        // reference alive for the reader threads' lifetime.
        let stdout_thread = {
            let on_data = on_data.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                let mut reader = std::io::BufReader::new(stdout);
                use std::io::Read;
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            let data = buffer[..n].to_vec();
                            on_data(&data);
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        let stderr_thread = {
            let on_data = on_data.clone();
            std::thread::spawn(move || {
                let mut buffer = [0u8; 8192];
                let mut reader = std::io::BufReader::new(stderr);
                use std::io::Read;
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            let data = buffer[..n].to_vec();
                            on_data(&data);
                        }
                        Err(_) => break,
                    }
                }
            })
        };

        let deadline = timeout_ms.map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
        let exit_code: Option<i64> = loop {
            if cancelled.load(Ordering::SeqCst) {
                if pid != 0 {
                    kill_process_tree(pid);
                }
                break None;
            }
            if let Some(deadline) = deadline {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    if pid != 0 {
                        kill_process_tree(pid);
                    }
                    break None;
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.code().map(|code| code as i64),
                Ok(None) => {}
                Err(_) => break None,
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };

        let _ = stdout_thread.join();
        let _ = stderr_thread.join();
        if pid != 0 {
            untrack_detached_child_pid(pid);
        }
        let _ = child.wait();

        if cancelled.load(Ordering::SeqCst) {
            return Err("aborted".to_string());
        }
        if timed_out {
            let timeout_secs = timeout_ms.map(|ms| ms / 1000).unwrap_or(0);
            return Err(format!("timeout:{timeout_secs}"));
        }
        Ok(exit_code)
    }
}

#[derive(Clone, Debug)]
pub struct BashToolDetails {
    pub truncation: Option<super::truncate::TruncationResult>,
    pub full_output_path: Option<String>,
}

/// Execute the bash tool (sync analog of createBashToolDefinition.execute).
pub fn execute_bash_tool(
    cwd: &str,
    command: &str,
    timeout: Option<f64>,
    operations: &dyn BashToolOperations,
    cancelled: &AtomicBool,
) -> Result<(Vec<pi_ai::types::Content>, Option<BashToolDetails>), String> {
    let timeout_ms = resolve_timeout_ms(timeout)?;
    let resolved_command = command.to_string();
    let resolved_cwd = resolve_to_cwd(cwd, cwd);

    let output = OutputAccumulator::new(super::output_accumulator::OutputAccumulatorOptions {
        max_lines: Some(DEFAULT_MAX_LINES),
        max_bytes: Some(DEFAULT_MAX_BYTES),
        temp_file_prefix: Some("pi-bash".to_string()),
    });

    let output_arc = std::sync::Arc::new(std::sync::Mutex::new(output));
    let handle_data: std::sync::Arc<dyn Fn(&[u8]) + Send + Sync> = {
        let output_arc = output_arc.clone();
        std::sync::Arc::new(move |data: &[u8]| {
            output_arc.lock().unwrap().append(data);
        })
    };

    let exec_result = operations.exec(&resolved_command, &resolved_cwd, handle_data, cancelled, timeout_ms);

    let format_output = |output: &std::sync::Mutex<OutputAccumulator>, empty_text: &str| -> (String, Option<BashToolDetails>) {
        let mut output = output.lock().unwrap();
        output.finish();
        let snapshot = output.snapshot(true);
        let truncation = snapshot.truncation.clone();
        let mut text = if snapshot.content.is_empty() {
            empty_text.to_string()
        } else {
            snapshot.content.clone()
        };
        let mut details: Option<BashToolDetails> = None;
        if truncation.truncated {
            details = Some(BashToolDetails {
                truncation: Some(truncation.clone()),
                full_output_path: snapshot.full_output_path.clone(),
            });
            let start_line = truncation.total_lines.saturating_sub(truncation.output_lines) + 1;
            let end_line = truncation.total_lines;
            if truncation.last_line_partial {
                let last_line_size = format_size(output.get_last_line_bytes() as f64);
                text.push_str(&format!(
                    "\n\n[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {}]",
                    format_size(truncation.output_bytes as f64),
                    snapshot.full_output_path.as_deref().unwrap_or("")
                ));
            } else if truncation.truncated_by == Some("lines") {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {}]",
                    truncation.total_lines,
                    snapshot.full_output_path.as_deref().unwrap_or("")
                ));
            } else {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {}]",
                    truncation.total_lines,
                    format_size(DEFAULT_MAX_BYTES as f64),
                    snapshot.full_output_path.as_deref().unwrap_or("")
                ));
            }
        }
        (text, details)
    };

    let exit_code = match exec_result {
        Ok(exit_code) => exit_code,
        Err(error) => {
            let (text, _) = format_output(&output_arc, "");
            if error == "aborted" {
                return Err(format!("{text}\n\nCommand aborted"));
            }
            if let Some(timeout_secs) = error.strip_prefix("timeout:") {
                return Err(format!("{text}\n\nCommand timed out after {timeout_secs} seconds"));
            }
            return Err(error);
        }
    };

    let (output_text, details) = format_output(&output_arc, "(no output)");
    if exit_code != Some(0) {
        if let Some(code) = exit_code {
            return Err(format!("{output_text}\n\nCommand exited with code {code}"));
        }
    }
    Ok((
        vec![pi_ai::types::Content::Text(pi_ai::types::TextContent {
            text: output_text,
            text_signature: None,
        })],
        details,
    ))
}

pub fn bash_tool_parameters() -> Value {
    Value::Map(vec![
        (
            "command".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Bash command to execute".to_string()),
            )]),
        ),
        (
            "timeout".to_string(),
            Value::Map(vec![(
                "description".to_string(),
                Value::String("Timeout in seconds (optional, no default timeout)".to_string()),
            )]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_validation() {
        assert_eq!(resolve_timeout_ms(None).unwrap(), None);
        assert_eq!(resolve_timeout_ms(Some(5.0)).unwrap(), Some(5000));
        assert!(resolve_timeout_ms(Some(0.0)).is_err());
        assert!(resolve_timeout_ms(Some(-1.0)).is_err());
        assert!(resolve_timeout_ms(Some(f64::NAN)).is_err());
        assert!(resolve_timeout_ms(Some(2_147_484.0)).is_err());
    }

    #[test]
    fn executes_command() {
        let operations = LocalBashOperations::new(None);
        let cancelled = AtomicBool::new(false);
        let (content, details) = execute_bash_tool(
            "/tmp",
            "echo hello",
            None,
            &operations,
            &cancelled,
        )
        .unwrap();
        assert!(matches!(&content[0], pi_ai::types::Content::Text(text) if text.text.trim() == "hello"));
        assert!(details.is_none());
    }

    #[test]
    fn nonzero_exit_errors() {
        let operations = LocalBashOperations::new(None);
        let cancelled = AtomicBool::new(false);
        let error = execute_bash_tool("/tmp", "exit 3", None, &operations, &cancelled).unwrap_err();
        assert!(error.contains("Command exited with code 3"));
    }

    #[test]
    fn missing_cwd_errors() {
        let operations = LocalBashOperations::new(None);
        let cancelled = AtomicBool::new(false);
        let error = execute_bash_tool("/definitely/not/a/dir-xyz", "echo hi", None, &operations, &cancelled).unwrap_err();
        assert!(error.contains("Working directory does not exist"));
    }

    #[test]
    fn timeout_kills() {
        let operations = LocalBashOperations::new(None);
        let cancelled = AtomicBool::new(false);
        let error = execute_bash_tool("/tmp", "sleep 5", Some(0.1), &operations, &cancelled).unwrap_err();
        assert!(error.contains("Command timed out after 0 seconds"));
    }
}
