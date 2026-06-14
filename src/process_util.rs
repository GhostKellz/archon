//! Helpers for running external subprocesses with a hard timeout.
//!
//! `std::process::Command::output` blocks until the child exits, which lets a
//! hung or unresponsive subprocess (a stuck `docker` daemon, a wedged
//! `systemctl` call) stall the caller indefinitely. [`run_with_timeout`] spawns
//! the child, drains its pipes on dedicated threads to avoid buffer deadlock,
//! and kills the process if it overruns the deadline.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};
use wait_timeout::ChildExt;

/// Run `command`, capturing stdout/stderr, but abort if it exceeds `timeout`.
///
/// On timeout the child is killed and an error is returned. Spawn failures are
/// surfaced as the underlying [`std::io::Error`] (no extra context), so callers
/// can still inspect e.g. [`std::io::ErrorKind::NotFound`].
pub fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;

    // Drain both pipes concurrently; otherwise a child that writes more than the
    // OS pipe buffer can hold would block forever while we wait on it.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });

    let status = match child.wait_timeout(timeout)? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("subprocess timed out after {}s", timeout.as_secs());
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_within_timeout() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf out; printf err 1>&2; exit 3"]);
        let output = run_with_timeout(cmd, Duration::from_secs(5)).expect("command runs");
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
        assert_eq!(output.status.code(), Some(3));
    }

    #[test]
    fn kills_process_that_overruns() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let err = run_with_timeout(cmd, Duration::from_millis(150)).expect_err("should time out");
        assert!(err.to_string().contains("timed out"));
    }

    #[test]
    fn surfaces_spawn_errors_as_io_error() {
        let cmd = Command::new("archon-nonexistent-binary-xyz");
        let err = run_with_timeout(cmd, Duration::from_secs(5)).expect_err("missing binary");
        let io_err = err
            .downcast_ref::<std::io::Error>()
            .expect("spawn failure preserved as io::Error");
        assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
    }
}
