use std::path::PathBuf;
use std::process::Child;
use std::thread;

use chrono::{DateTime, Utc};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::{EngineKind, LaunchMode};
use crate::engine::CommandSpec;
use crate::sync::{SyncEvent, SyncLayer};

/// Observes spawned browser processes and logs exit telemetry.
pub struct ProcessMonitor;

impl ProcessMonitor {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        session_id: Uuid,
        engine: EngineKind,
        mode: LaunchMode,
        profile: String,
        profile_path: PathBuf,
        command: CommandSpec,
        launched_at: DateTime<Utc>,
        pid: u32,
        mut child: Child,
        sync: SyncLayer,
    ) {
        thread::spawn(move || {
            let result = child.wait();
            let finished_at = Utc::now();
            let duration = finished_at
                .signed_duration_since(launched_at)
                .num_milliseconds()
                .max(0) as u64;

            match result {
                Ok(status) => {
                    let exit_code = status.code();
                    let success = Some(status.success());
                    let event = SyncEvent::exit(
                        session_id,
                        &profile,
                        &profile_path,
                        mode,
                        engine,
                        &command,
                        pid,
                        exit_code,
                        success,
                        Some(duration),
                        None,
                    );
                    if let Err(err) = sync.append_event(event) {
                        warn!(
                            session = %session_id,
                            pid,
                            error = %err,
                            "Failed to append exit telemetry"
                        );
                    } else {
                        info!(
                            session = %session_id,
                            pid,
                            exit_code,
                            duration_ms = duration,
                            "Process exited"
                        );
                    }
                }
                Err(err) => {
                    let event = SyncEvent::exit(
                        session_id,
                        &profile,
                        &profile_path,
                        mode,
                        engine,
                        &command,
                        pid,
                        None,
                        Some(false),
                        Some(duration),
                        Some(err.to_string()),
                    );
                    if let Err(write_err) = sync.append_event(event) {
                        warn!(
                            session = %session_id,
                            pid,
                            error = %write_err,
                            original_error = %err,
                            "Failed to append telemetry after spawn error"
                        );
                    } else {
                        warn!(
                            session = %session_id,
                            pid,
                            error = %err,
                            "Process wait failed"
                        );
                    }
                }
            }
        });
    }
}
