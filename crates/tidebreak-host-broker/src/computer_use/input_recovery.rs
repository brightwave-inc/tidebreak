//! Broker-owned journals bind recovery to one helper input invocation.
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

use super::{BackendError, BackendErrorKind};

const MAX_JOURNAL_BYTES: u64 = 16 * 1024;

pub(super) struct InputRecovery {
    _directory: tempfile::TempDir,
    journal: PathBuf,
    cancel: PathBuf,
    invocation: String,
    worker_running: AtomicBool,
}

fn failed(error: impl std::fmt::Display) -> BackendError {
    BackendError::new(
        BackendErrorKind::OperationFailed,
        format!("Cannot verify computer input recovery: {error}"),
    )
}

fn private_file(path: &std::path::Path, contents: &[u8]) -> Result<(), BackendError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(contents))
        .map_err(failed)
}

impl InputRecovery {
    pub(super) fn prepare(request: &mut Value) -> Result<Option<Self>, BackendError> {
        if !matches!(
            request.get("op").and_then(Value::as_str),
            Some("click" | "type_text" | "key_press" | "scroll" | "hover" | "drag")
        ) && request.get("execution_mode").and_then(Value::as_str) != Some("foreground")
        {
            return Ok(None);
        }
        let mut builder = tempfile::Builder::new();
        builder.prefix("tidebreak-cu-input-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir().map_err(failed)?;
        let root = directory.path().canonicalize().map_err(failed)?;
        let state = Self {
            _directory: directory,
            journal: root.join("input.json"),
            cancel: root.join("cancel"),
            invocation: uuid::Uuid::new_v4().to_string(),
            worker_running: AtomicBool::new(false),
        };
        private_file(&state.cancel, state.invocation.as_bytes())?;
        let journal = json!({"invocation_id": state.invocation, "held": []});
        private_file(
            &state.journal,
            &serde_json::to_vec(&journal).map_err(failed)?,
        )?;
        request["input_journal_path"] = json!(state.journal);
        request["input_cancel_path"] = json!(state.cancel);
        request["input_invocation_id"] = json!(state.invocation);
        Ok(Some(state))
    }

    pub(super) fn worker_started(&self) {
        self.worker_running.store(true, Ordering::Release);
    }

    pub(super) fn worker_exited(&self) {
        self.worker_running.store(false, Ordering::Release);
    }

    pub(super) fn cancel(&self) -> Result<(), BackendError> {
        std::fs::write(&self.cancel, b"stopped").map_err(failed)
    }

    pub(super) fn pending(&self) -> Result<bool, BackendError> {
        if self.worker_running.load(Ordering::Acquire) {
            return Err(failed("the input worker has not been confirmed stopped"));
        }
        let metadata = std::fs::symlink_metadata(&self.journal).map_err(failed)?;
        if !metadata.is_file() {
            return Err(failed("input journal is no longer a regular file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o777 != 0o600 {
                return Err(failed("input journal is no longer private"));
            }
        }
        let mut bytes = Vec::new();
        File::open(&self.journal)
            .map_err(failed)?
            .take(MAX_JOURNAL_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(failed)?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(failed("input journal exceeds its size limit"));
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(failed)?;
        if value["invocation_id"].as_str() != Some(&self.invocation) {
            return Err(failed("input journal belongs to another invocation"));
        }
        if let Some(target) = value.get("target").filter(|target| !target.is_null()) {
            if target["pid"]
                .as_i64()
                .is_none_or(|pid| pid <= 0 || pid > i64::from(i32::MAX))
                || target["bundle_id"]
                    .as_str()
                    .is_none_or(|id| id.is_empty() || id.len() > 256)
                || target["launched_at"]
                    .as_f64()
                    .is_none_or(|time| !time.is_finite() || time <= 0.0)
                || target["window_id"]
                    .as_u64()
                    .is_none_or(|id| id == 0 || id > u64::from(u32::MAX))
            {
                return Err(failed("invalid independent input destination"));
            }
        }
        let held = value["held"]
            .as_array()
            .ok_or_else(|| failed("missing held input list"))?;
        if held.len() > 16
            || held.iter().any(|item| {
                let code = item["code"].as_u64();
                match item["kind"].as_str() {
                    Some("key") => code.is_none_or(|code| code > 127),
                    Some("mouse") => code.is_none_or(|code| code > 1),
                    _ => true,
                }
            })
        {
            return Err(failed("invalid held input"));
        }
        Ok(!held.is_empty())
    }

    pub(super) fn preserve(self) -> PathBuf {
        self._directory.keep()
    }

    pub(super) fn cleanup_request(&self) -> Value {
        json!({"op": "release_recorded_input", "input_journal_path": self.journal,
               "input_invocation_id": self.invocation})
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn background_input_gets_recovery_but_observation_does_not() {
        for op in ["click", "type_text", "key_press", "scroll", "hover", "drag"] {
            let mut request = json!({"op": op, "execution_mode": "background"});
            let state = InputRecovery::prepare(&mut request).unwrap().unwrap();
            assert!(request["input_journal_path"].is_string());
            assert!(!state.pending().unwrap());
        }
        for op in ["capture", "read_ax_tree", "list_windows", "permissions"] {
            let mut request = json!({"op": op});
            assert!(InputRecovery::prepare(&mut request).unwrap().is_none());
        }
    }

    #[test]
    fn recovery_rejects_invalid_target_identity() {
        let mut request = json!({"op": "key_press", "execution_mode": "background"});
        let state = InputRecovery::prepare(&mut request).unwrap().unwrap();
        let target = json!({"pid": 123, "bundle_id": "dev.fixture", "launched_at": 1234,
                            "window_id": 42});
        for (field, invalid) in [
            ("pid", json!(0)),
            ("window_id", json!(0)),
            ("launched_at", json!(-1)),
            ("bundle_id", json!("")),
        ] {
            let mut invalid_target = target.clone();
            invalid_target[field] = invalid;
            std::fs::write(
                &state.journal,
                json!({"invocation_id": state.invocation,
                "target": invalid_target, "held": [{"kind": "key", "code": 0}]})
                .to_string(),
            )
            .unwrap();
            assert!(
                state.pending().is_err(),
                "{field} must not authorize cleanup"
            );
        }
        std::fs::write(
            &state.journal,
            json!({"invocation_id": state.invocation,
            "target": target, "held": [{"kind": "key", "code": 0}]})
            .to_string(),
        )
        .unwrap();
        assert!(state.pending().unwrap());
    }

    #[test]
    fn private_invocations_reject_stale_oversized_and_invalid_records() {
        let mut request = json!({"execution_mode": "foreground"});
        let state = InputRecovery::prepare(&mut request).unwrap().unwrap();
        assert_eq!(
            std::fs::metadata(&state.journal)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(state.journal.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(!state.pending().unwrap());
        state.worker_started();
        assert!(state.pending().is_err());
        state.worker_exited();
        assert!(!state.pending().unwrap());
        let wrong = json!({"invocation_id": "different", "held": []});
        std::fs::write(&state.journal, wrong.to_string()).unwrap();
        assert!(state.pending().is_err());
        std::fs::write(&state.journal, vec![b' '; MAX_JOURNAL_BYTES as usize + 1]).unwrap();
        assert!(state.pending().is_err());
        for held in [
            json!([{"kind":"mouse","code":5}]),
            json!([{"kind":"key","code":65535}]),
            json!([{"kind":"down","code":0}]),
        ] {
            std::fs::write(
                &state.journal,
                json!({"invocation_id":state.invocation,"held":held}).to_string(),
            )
            .unwrap();
            assert!(state.pending().is_err());
        }
    }
}
