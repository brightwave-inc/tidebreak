use std::fmt::Write;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tidebreak_core::preview::MAX_ACTION_SUMMARY_CHARS;
use tidebreak_core::{
    ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolSpec, SUMMARY_ARGUMENT_DESCRIPTION,
};

use crate::{
    CodeExecutionProvider, CodeExecutionRequest, ExecutionId, ExecutionWorkspaceId, MAX_ARGUMENTS,
    MAX_COMMAND_BYTES, MAX_CWD_BYTES, MAX_STAGED_PATHS,
};

pub const EXEC_TOOL_NAME: &str = "exec";

/// Model-facing command execution backed by a host-selected provider.
pub struct ExecTool {
    provider: Arc<dyn CodeExecutionProvider>,
}

impl ExecTool {
    #[must_use]
    pub fn new(provider: Arc<dyn CodeExecutionProvider>) -> Self {
        Self { provider }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArguments {
    command: String,
    /// The model's own account of what this call is doing, shown to the user
    /// in place of the argument vector. Required in the schema so a model
    /// reliably writes one; optional here so a call that omits it still runs
    /// and the card falls back to showing the command.
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "read from the canonical arguments by the action preview"
    )]
    summary: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_cwd")]
    cwd: String,
    #[serde(default)]
    files: Vec<String>,
}

fn default_cwd() -> String {
    ".".into()
}

#[async_trait]
impl Tool for ExecTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: EXEC_TOOL_NAME.into(),
            description: "Run one executable with an argument vector in the configured isolated \
                          execution provider. No shell parses the arguments unless you explicitly \
                          invoke a shell. The local provider runs directly in the chat's private \
                          scratch and blocks network and user-data access outside it plus any \
                          current host-resolved folder grants listed in the operating context; \
                          folder paths are host state, never tool arguments. Managed cloud \
                          sandboxes (E2B, Daytona) see ONLY the scratch paths you list in the \
                          'files' argument, plus whatever earlier commands in the same sandbox \
                          session created. List every file or directory the command reads — \
                          including files you just wrote with write_file and attached documents \
                          under documents/ — or the command will not find them there. A listed \
                          directory stages recursively; a listed path that does not exist fails \
                          the call on every provider. Every provider returns bounded \
                          stdout/stderr. Files you save in output/ are published to the user \
                          automatically as durable outputs; output/ and preview/ are copied back \
                          for you and need never be listed. Everything else the command leaves in \
                          scratch is intermediate and ephemeral: it may not survive to a later \
                          command or turn, and there is no undo for it. Reach for exec to run a \
                          program, not to edit workspace text through shell redirection — \
                          read_file and write_file do that directly and durably. To update an \
                          output you already published, save to the same filename in output/ — it \
                          becomes a new version of the same output in place; you never track \
                          output ids. For \
                          visual review, save up to three PNG, JPEG, or WebP images in preview/; \
                          overview, grid, thumbnail, page, and slide filenames are prioritized; \
                          preview images are for your own review and never become outputs. When \
                          bundled document helpers are present, invoke them directly from \
                          .tidebreak/exec-scripts (always available without listing them). \
                          Examples: command python3 with args \
                          [\".tidebreak/exec-scripts/render_pdf.py\", \"documents/report.pdf\", \
                          \"--pages\", \"1-2\"] and files [\"documents/report.pdf\"]; command \
                          python3 with args [\".tidebreak/exec-scripts/extract_pdf_figures.py\", \
                          \"documents/report.pdf\"] and files [\"documents/report.pdf\"]; or \
                          command python3 with args [\".tidebreak/exec-scripts/analyze_xlsx.py\", \
                          \"documents/model.xlsx\"] and files [\"documents/model.xlsx\"]. Each \
                          helper writes visual review files to preview/ and prints a concise \
                          summary; a missing Python or document dependency is reported as a \
                          command error."
                .into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "summary": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_ACTION_SUMMARY_CHARS,
                        "description": SUMMARY_ARGUMENT_DESCRIPTION
                    },
                    "command": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_COMMAND_BYTES,
                        "description": "Executable name or path."
                    },
                    "args": {
                        "type": "array",
                        "maxItems": MAX_ARGUMENTS,
                        "items": { "type": "string" },
                        "description": "Arguments passed directly to the executable."
                    },
                    "cwd": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_CWD_BYTES,
                        "description": "Private-scratch-relative working directory (defaults to '.')."
                    },
                    "files": {
                        "type": "array",
                        "maxItems": MAX_STAGED_PATHS,
                        "items": { "type": "string" },
                        "description": "Scratch-relative files or directories staged into a managed sandbox before the command runs; directories stage recursively. Managed sandboxes see only these paths (plus what earlier commands in the session created); a path that does not exist fails the call on every provider."
                    }
                },
                "required": ["summary", "command"]
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        // Command execution remains an explicit consent boundary even though
        // the provider confines writes and independently enforces the chat's
        // network policy.
        ApprovalClass::Sensitive
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let arguments: ExecArguments = match serde_json::from_value(args) {
            Ok(arguments) => arguments,
            Err(error) => {
                return Ok(ToolOutput::error(format!("invalid arguments: {error}")));
            }
        };
        let Some(call_id) = ctx.call_id else {
            return Ok(ToolOutput::error(
                "code execution requires a stable tool-call identity",
            ));
        };
        let execution_id = match ExecutionId::parse(call_id.to_string()) {
            Ok(id) => id,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let workspace_id = match ExecutionWorkspaceId::parse(ctx.chat_id.to_string()) {
            Ok(id) => id,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let request = match CodeExecutionRequest::new(
            execution_id.clone(),
            workspace_id.clone(),
            arguments.command,
            arguments.args,
            arguments.cwd,
        )
        .and_then(|request| request.with_staged_files(arguments.files))
        {
            Ok(request) => request,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let response = match self.provider.execute(request).await {
            Ok(response) => response,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        // Regardless of the exit code: a failing later step must not hide
        // files the command already durably wrote to output/.
        let artifacts = match self
            .provider
            .collect_output_artifacts(&workspace_id, &execution_id)
            .await
        {
            Ok(artifacts) => artifacts,
            Err(error) => crate::OutputArtifactScan {
                entries: Vec::new(),
                notes: vec![format!("outputs unavailable: {error}")],
            },
        };
        let successful = !response.timed_out && response.exit_code == Some(0);
        let previews = if successful {
            match self.provider.collect_preview_images(&workspace_id).await {
                Ok(previews) => previews,
                Err(error) => crate::PreviewScan {
                    images: Vec::new(),
                    notes: vec![format!("preview images unavailable: {error}")],
                },
            }
        } else {
            crate::PreviewScan::default()
        };

        let exit = response
            .exit_code
            .map_or_else(|| "signal".into(), |code| code.to_string());
        let mut content = format!(
            "provider: {}\nexit: {exit}\nduration_ms: {}",
            response.provider, response.duration_ms
        );
        if response.timed_out {
            // A timeout is not an ordinary failure: the command was killed by
            // the host's time limit, so its output can be empty or partial.
            // Say so plainly, or an empty stderr reads as an inexplicable
            // crash and invites blind retries.
            let _ = write!(
                content,
                "\ntimed_out: true (the host killed this command at its execution time limit \
                 after {} ms; stdout/stderr may be empty or incomplete — split the work into \
                 smaller commands rather than rerunning this one unchanged)",
                response.duration_ms
            );
        }
        if response.output_truncated {
            content.push_str("\noutput_truncated: true");
        }
        if response.degraded.is_some() {
            // The model is told too: a run that installs its own dependencies
            // is slower and can fail on a network policy, and that is not a
            // fact it should have to infer from a pip log.
            content.push_str(
                "\n\nsandbox: the prepared Tidebreak sandbox image was unavailable, so this \
                 command ran on the backend's stock image. Document tooling installs its \
                 dependencies at run time here, which is slower and needs package-registry \
                 network access.",
            );
        }
        if !response.sync_notes.is_empty() {
            content.push_str("\n\nworkspace sync:");
            for note in &response.sync_notes {
                content.push('\n');
                content.push_str(note);
            }
        }
        if !previews.notes.is_empty() {
            content.push_str("\n\npreview scan:");
            for note in &previews.notes {
                content.push('\n');
                content.push_str(note);
            }
        }
        let published: Vec<String> = artifacts
            .entries
            .iter()
            .filter_map(|entry| match entry.status {
                crate::OutputArtifactStatus::Created => Some(format!(
                    "created {} (version {})",
                    entry.filename, entry.ordinal
                )),
                crate::OutputArtifactStatus::Updated => Some(format!(
                    "updated {} (version {})",
                    entry.filename, entry.ordinal
                )),
                // A file that still matches its published version is not news.
                crate::OutputArtifactStatus::Unchanged => None,
            })
            .collect();
        if !published.is_empty() || !artifacts.notes.is_empty() {
            content.push_str("\n\noutputs:");
            for line in published.iter().chain(&artifacts.notes) {
                content.push('\n');
                content.push_str(line);
            }
        }
        if !response.stdout.is_empty() {
            content.push_str("\n\nstdout:\n");
            content.push_str(&response.stdout);
        }
        if !response.stderr.is_empty() {
            content.push_str("\n\nstderr:\n");
            content.push_str(&response.stderr);
        }
        let failed = response.timed_out || response.exit_code != Some(0);
        // `stdout`/`stderr` ride here as well as in the model-facing content
        // so the renderer's closed result projection can read them field by
        // field rather than parsing them back out of prose.
        let mut output = ToolOutput::text(content)
            .with_data(json!({
                "provider": response.provider,
                "exit_code": response.exit_code,
                "timed_out": response.timed_out,
                "output_truncated": response.output_truncated,
                "duration_ms": response.duration_ms,
                "stdout": response.stdout,
                "stderr": response.stderr,
                "sync_notes": response.sync_notes,
                "degraded": response.degraded,
                "outputs": artifacts
                    .entries
                    .iter()
                    .map(|entry| {
                        json!({
                            "filename": entry.filename,
                            "output_id": entry.output_id,
                            "version": entry.ordinal,
                            "status": match entry.status {
                                crate::OutputArtifactStatus::Created => "created",
                                crate::OutputArtifactStatus::Updated => "updated",
                                crate::OutputArtifactStatus::Unchanged => "unchanged",
                            },
                        })
                    })
                    .collect::<Vec<_>>(),
            }))
            .with_images(previews.images);
        output.is_error = failed;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodeExecutionError, CodeExecutionProviderKind, CodeExecutionResponse};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tidebreak_core::{CallId, ChatId};

    #[derive(Default)]
    struct RecordingProvider {
        request: Mutex<Option<CodeExecutionRequest>>,
    }

    #[async_trait]
    impl CodeExecutionProvider for RecordingProvider {
        async fn execute(
            &self,
            request: CodeExecutionRequest,
        ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
            *self.request.lock().unwrap() = Some(request);
            Ok(CodeExecutionResponse {
                provider: CodeExecutionProviderKind::Local,
                exit_code: Some(0),
                stdout: "ok\n".into(),
                stderr: String::new(),
                timed_out: false,
                output_truncated: false,
                duration_ms: 3,
                sync_notes: Vec::new(),
                degraded: None,
            })
        }
    }

    #[tokio::test]
    async fn tool_passes_stable_host_identities_and_structured_arguments() {
        let provider = Arc::new(RecordingProvider::default());
        let tool = ExecTool::new(provider.clone());
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let ctx = ToolCtx::new_legacy_workspace(chat_id, None, PathBuf::from("/tmp/unused"))
            .with_call_id(call_id);

        let output = tool
            .execute(
                &ctx,
                json!({"command": "/bin/echo", "args": ["ok"], "cwd": ".", "files": ["data/in.csv"]}),
            )
            .await
            .unwrap();

        assert!(!output.is_error);
        assert!(output.content.contains("stdout:\nok"));
        let request = provider.request.lock().unwrap().clone().unwrap();
        assert_eq!(request.execution_id.as_str(), call_id.to_string());
        assert_eq!(request.workspace_id.as_str(), chat_id.to_string());
        assert_eq!(request.command, "/bin/echo");
        assert_eq!(request.files.len(), 1);
        assert_eq!(request.files[0].as_str(), "data/in.csv");

        // A traversal in the model-listed staged set is refused before any
        // provider sees the request, and the error names the path.
        let refused = tool
            .execute(
                &ctx,
                json!({"command": "/bin/echo", "files": ["../escape"]}),
            )
            .await
            .unwrap();
        assert!(refused.is_error);
        assert!(refused.content.contains("../escape"), "{}", refused.content);
    }

    #[test]
    fn exec_is_sensitive_and_has_a_closed_schema() {
        let tool = ExecTool::new(Arc::new(RecordingProvider::default()));
        assert_eq!(tool.approval_class(), ApprovalClass::Sensitive);
        let spec = tool.spec();
        assert_eq!(spec.name, EXEC_TOOL_NAME);
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert!(spec.input_schema["properties"]["files"].is_object());
        assert!(spec.description.contains("'files'"));
        assert!(spec.description.contains("preview/"));
        assert!(spec.description.contains(".tidebreak/exec-scripts"));
        assert!(spec.description.contains("render_pdf.py"));
        assert!(spec.description.contains("analyze_xlsx.py"));
    }

    struct TimedOutProvider;

    #[async_trait]
    impl CodeExecutionProvider for TimedOutProvider {
        async fn execute(
            &self,
            _request: CodeExecutionRequest,
        ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
            Ok(CodeExecutionResponse {
                provider: CodeExecutionProviderKind::Local,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
                output_truncated: false,
                duration_ms: 60_012,
                sync_notes: Vec::new(),
                degraded: None,
            })
        }
    }

    /// A timeout must read as the host's time limit, not as an inexplicable
    /// failure with empty stderr — the E2B incident showed the model retrying
    /// killed installs because the receipt said nothing.
    #[tokio::test]
    async fn timed_out_receipt_names_the_time_limit() {
        let tool = ExecTool::new(Arc::new(TimedOutProvider));
        let output = tool
            .execute(
                &ToolCtx::new_legacy_workspace(ChatId::new(), None, PathBuf::from("/tmp/unused"))
                    .with_call_id(CallId::new()),
                json!({"command": "pip", "args": ["install", "python-docx"]}),
            )
            .await
            .unwrap();

        assert!(output.is_error);
        assert!(
            output
                .content
                .contains("killed this command at its execution time limit"),
            "{}",
            output.content
        );
        assert!(output.content.contains("60012 ms"), "{}", output.content);
        assert_eq!(output.data.as_ref().unwrap()["timed_out"], true);
    }

    struct PreviewProvider {
        exit_code: i32,
        scans: AtomicUsize,
    }

    #[async_trait]
    impl CodeExecutionProvider for PreviewProvider {
        async fn execute(
            &self,
            _request: CodeExecutionRequest,
        ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
            Ok(CodeExecutionResponse {
                provider: CodeExecutionProviderKind::Local,
                exit_code: Some(self.exit_code),
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                output_truncated: false,
                duration_ms: 1,
                sync_notes: Vec::new(),
                degraded: None,
            })
        }

        async fn collect_preview_images(
            &self,
            _workspace: &ExecutionWorkspaceId,
        ) -> std::result::Result<crate::PreviewScan, CodeExecutionError> {
            self.scans.fetch_add(1, Ordering::SeqCst);
            let bytes = vec![1, 2, 3];
            let image = tidebreak_core::ImageRef {
                blob_id: tidebreak_core::DocumentBlob::from_bytes(&bytes).id,
                media_type: tidebreak_core::ImageMediaType::Png,
                width: 1,
                height: 1,
                byte_len: 3,
            };
            Ok(crate::PreviewScan {
                images: vec![(
                    image,
                    tidebreak_core::ImageData::new(image.media_type, bytes),
                )],
                notes: Vec::new(),
            })
        }
    }

    struct ArtifactProvider {
        exit_code: i32,
        scans: AtomicUsize,
    }

    #[async_trait]
    impl CodeExecutionProvider for ArtifactProvider {
        async fn execute(
            &self,
            _request: CodeExecutionRequest,
        ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
            Ok(CodeExecutionResponse {
                provider: CodeExecutionProviderKind::Local,
                exit_code: Some(self.exit_code),
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                output_truncated: false,
                duration_ms: 1,
                sync_notes: Vec::new(),
                degraded: None,
            })
        }

        async fn collect_output_artifacts(
            &self,
            _workspace: &ExecutionWorkspaceId,
            _execution: &ExecutionId,
        ) -> std::result::Result<crate::OutputArtifactScan, CodeExecutionError> {
            self.scans.fetch_add(1, Ordering::SeqCst);
            Ok(crate::OutputArtifactScan {
                entries: vec![
                    crate::OutputArtifactEntry {
                        filename: "report.md".into(),
                        output_id: "output-report".into(),
                        ordinal: 2,
                        status: crate::OutputArtifactStatus::Updated,
                    },
                    crate::OutputArtifactEntry {
                        filename: "data.csv".into(),
                        output_id: "output-data".into(),
                        ordinal: 1,
                        status: crate::OutputArtifactStatus::Unchanged,
                    },
                ],
                notes: vec!["output/huge.bin was not published: too large".into()],
            })
        }
    }

    /// The scan reports through the model-facing content and the structured
    /// data, and runs even when the command failed — a failing later step must
    /// not hide files that were already durably written.
    #[tokio::test]
    async fn published_outputs_are_reported_even_when_the_command_fails() {
        let provider = Arc::new(ArtifactProvider {
            exit_code: 2,
            scans: AtomicUsize::new(0),
        });
        let tool = ExecTool::new(provider.clone());
        let output = tool
            .execute(
                &ToolCtx::new_legacy_workspace(ChatId::new(), None, PathBuf::from("/tmp/unused"))
                    .with_call_id(CallId::new()),
                json!({"command": "/bin/false"}),
            )
            .await
            .unwrap();

        assert_eq!(provider.scans.load(Ordering::SeqCst), 1);
        assert!(output.is_error);
        assert!(output.content.contains("outputs:"));
        assert!(output.content.contains("updated report.md (version 2)"));
        // A file that still matches its published version is not news.
        assert!(!output.content.contains("data.csv"));
        assert!(output.content.contains("output/huge.bin was not published"));
        let outputs = output.data.as_ref().unwrap()["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0]["status"], "updated");
        assert_eq!(outputs[1]["status"], "unchanged");
    }

    #[tokio::test]
    async fn previews_are_collected_only_after_success() {
        for (exit_code, expected_scans, expected_images) in [(0, 1, 1), (2, 0, 0)] {
            let provider = Arc::new(PreviewProvider {
                exit_code,
                scans: AtomicUsize::new(0),
            });
            let tool = ExecTool::new(provider.clone());
            let output = tool
                .execute(
                    &ToolCtx::new_legacy_workspace(
                        ChatId::new(),
                        None,
                        PathBuf::from("/tmp/unused"),
                    )
                    .with_call_id(CallId::new()),
                    json!({"command": "/bin/true"}),
                )
                .await
                .unwrap();
            assert_eq!(provider.scans.load(Ordering::SeqCst), expected_scans);
            assert_eq!(output.images.len(), expected_images);
        }
    }
}
