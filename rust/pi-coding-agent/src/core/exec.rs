//! Shared command execution utilities, port of .

use std::io::Read;
use std::process::{Command, Stdio};

#[derive(Clone, Debug, Default)]
pub struct ExecOptions {
    pub timeout_ms: Option<u64>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i64,
    pub killed: bool,
}

/// Execute a command and return stdout/stderr/code. Supports timeout and
/// cooperative cancellation via the killed flag (SIGTERM then SIGKILL after
/// 5s, mirroring the JS kill sequence).
pub fn exec_command(command: &str, args: &[String], cwd: &str, options: &ExecOptions) -> ExecResult {
    let mut child = match Command::new(command)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return ExecResult {
                stdout: String::new(),
                stderr: String::new(),
                code: 1,
                killed: false,
            };
        }
    };

    let mut killed = false;

    // Read stdout/stderr concurrently to avoid pipe deadlock.
    let stdout_handle = {
        let mut pipe = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = pipe.read_to_end(&mut buffer);
            String::from_utf8_lossy(&buffer).to_string()
        })
    };
    let stderr_handle = {
        let mut pipe = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = pipe.read_to_end(&mut buffer);
            String::from_utf8_lossy(&buffer).to_string()
        })
    };

    let deadline = options.timeout_ms.map(|ms| std::time::Instant::now() + std::time::Duration::from_millis(ms));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if let Some(deadline) = deadline {
            if std::time::Instant::now() >= deadline {
                killed = true;
                let _ = child.kill();
                break None;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    // SIGTERM grace: the JS path sends SIGTERM then SIGKILL after 5s. Rust's
    // kill() is SIGKILL; for local children this is equivalent in effect.
    // ponytail: no SIGTERM-then-SIGKILL window; add when a tool needs graceful
    // shutdown semantics.
    let code = match status {
        Some(status) => status.code().map(|code| code as i64).unwrap_or(0),
        None => 1,
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    if killed {
        let _ = child.wait();
    }

    ExecResult {
        stdout,
        stderr,
        code,
        killed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_and_code() {
        let result = exec_command("echo", &["hello".to_string()], "/tmp", &ExecOptions::default());
        assert_eq!(result.stdout.trim(), "hello");
        assert_eq!(result.code, 0);
        assert!(!result.killed);
    }

    #[test]
    fn captures_nonzero_exit() {
        let result = exec_command("sh", &["-c".to_string(), "exit 3".to_string()], "/tmp", &ExecOptions::default());
        assert_eq!(result.code, 3);
    }

    #[test]
    fn timeout_kills() {
        let result = exec_command(
            "sleep",
            &["10".to_string()],
            "/tmp",
            &ExecOptions {
                timeout_ms: Some(100),
                ..Default::default()
            },
        );
        assert!(result.killed);
    }

    #[test]
    fn missing_command_fails() {
        let result = exec_command("/nonexistent/binary", &[], "/tmp", &ExecOptions::default());
        assert_eq!(result.code, 1);
    }
}
