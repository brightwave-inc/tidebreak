use std::time::Instant;

use base64::Engine as _;

use crate::{
    CodeExecutionError, CodeExecutionProviderKind, CodeExecutionResponse, MAX_CAPTURE_BYTES,
};

#[derive(Clone, Copy)]
pub(crate) enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Default)]
pub(crate) struct Capture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    total: usize,
    truncated: bool,
}

impl Capture {
    pub(crate) fn append(&mut self, bytes: &[u8], kind: StreamKind) {
        let available = MAX_CAPTURE_BYTES.saturating_sub(self.total);
        let kept = available.min(bytes.len());
        let target = match kind {
            StreamKind::Stdout => &mut self.stdout,
            StreamKind::Stderr => &mut self.stderr,
        };
        target.extend_from_slice(&bytes[..kept]);
        self.total += kept;
        self.truncated |= kept < bytes.len();
    }

    pub(crate) fn append_base64(
        &mut self,
        value: &str,
        kind: StreamKind,
        provider: &str,
    ) -> Result<(), CodeExecutionError> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(|_| {
                CodeExecutionError::Unavailable(format!(
                    "{provider} returned invalid encoded output"
                ))
            })?;
        self.append(&decoded, kind);
        Ok(())
    }

    pub(crate) fn response(
        self,
        provider: CodeExecutionProviderKind,
        started: Instant,
        exit_code: Option<i32>,
        timed_out: bool,
    ) -> CodeExecutionResponse {
        CodeExecutionResponse {
            provider,
            exit_code,
            stdout: String::from_utf8_lossy(&self.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&self.stderr).into_owned(),
            timed_out,
            output_truncated: self.truncated,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            sync_notes: Vec::new(),
        }
    }
}
