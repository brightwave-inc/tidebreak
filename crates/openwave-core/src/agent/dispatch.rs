
use chrono::Utc;
use futures::future::{self, Either};
use serde_json::Value;

use crate::approval::{
    ApprovalDecision, ApprovalJournalIdentity, ApprovalRequest, ApprovalRequiredPublication,
    GrantScope, ToolApprovalKind,
};
use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::id::{AgentRunId, CallId, ChatId, TurnId};
use crate::image::ImageAttachments;
use crate::model::{
    Chat, PermissionMode, Role, ToolCallExecution, ToolCallRecord, ToolCallResolution,
    ToolCallStatus,
};
use crate::preview::{ToolActionPreview, ToolResultPreview};
use crate::provider::{ChatMessage, ContentBlock, MessageReasoning};
use crate::storage::ResolveToolCallOutcome;
use crate::tool::{ApprovalClass, ToolCtx, ToolErrorCategory, ToolOutput};

use super::events::EventSink;
use super::transcript::{
    parse_args, parse_tool_args, tool_result_blocks, truncate_to_bytes, exec_preview_images,
};
use super::types::{
    ForegroundAgentWaitRequest, SandboxAgentSpawnRequest,
};
use super::{
    call_action_preview, provider_executed_entries, AcceptedServerCall, Agent,
    CallIsolation, ClientArgumentResolution, PendingCall, SandboxSpawnGate,
};

impl Agent {
    /// Why `call` cannot run beside its siblings, if it cannot.
    ///
    /// A name this turn does not advertise is deliberately plain: it reaches
    /// [`Self::run_tool`], which answers it with `unknown tool` rather than
    /// bending the batch around a call that was never going to run.
    pub(crate) fn call_isolation(&self, call: &PendingCall) -> Option<CallIsolation> {
        if self.tools.execution(&call.name) == Some(ToolCallExecution::Client) {
            return Some(CallIsolation::Client);
        }
        if self.agent_orchestration_active() {
            if self.tools.is_foreground_sandbox_spawn(&call.name) {
                return Some(CallIsolation::SandboxSpawn);
            }
            if self.tools.is_foreground_agent_wait(&call.name) {
                return Some(CallIsolation::AgentWait);
            }
        }
        None
    }

    /// Whether `call` parks on the approval gate before it may run.
    ///
    /// Sensitive calls stay in-step but are admitted one at a time, after the
    /// plain siblings: [`Self::resume_pending_server_calls`] recovers an
    /// interrupted approval by identity and cannot choose between two pending
    /// rows, so a second row must not exist while one can be parked. Standing
    /// grants are deliberately not consulted here — whether a grant covers the
    /// call is decided against its parsed arguments inside [`Self::run_tool`],
    /// and sequencing must not depend on getting the same answer twice.
    pub(crate) fn call_is_sensitive(&self, call: &PendingCall) -> bool {
        self.tools
            .get(&call.name)
            .is_some_and(|tool| tool.approval_class() == ApprovalClass::Sensitive)
    }

    /// Whether a call may overlap the read-only calls before it in this step.
    ///
    /// Unknown names intentionally stay sequential so they follow the ordinary
    /// `unknown tool` path without widening the concurrent surface. Every
    /// workspace write, approval-bearing call, and checkpoint is a boundary.
    pub(crate) fn call_is_parallel_eligible(&self, call: &PendingCall) -> bool {
        self.tools
            .get(&call.name)
            .is_some_and(|tool| tool.approval_class() == ApprovalClass::ReadOnly)
    }

    /// Persist one call the provider already ran, and announce it.
    ///
    /// The work is finished before OpenWave sees it, so the row is written and
    /// resolved back to back rather than admitted pending. Everything after
    /// that is deliberately identical to a host tool call of the same name: the
    /// journal carries the same started/completed pair, the activity card is
    /// built by the same projection, and a later turn rebuilds it as an
    /// ordinary `ToolUse`/`ToolResult` pair, because it is an ordinary row.
    ///
    /// A row that cannot be written is not a reason to fail the turn — the
    /// search already happened and the model already has its results — so the
    /// step continues without it.
    pub(crate) async fn record_provider_executed_call(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        block: &ContentBlock,
        events: &EventSink<'_>,
    ) -> Result<()> {
        let ContentBlock::ProviderExecutedToolCall {
            name,
            input,
            output,
            is_error,
            replay,
        } = block
        else {
            return Ok(());
        };
        let call_id = CallId::new();
        let result = output.to_string();
        let tool_output = if *is_error {
            ToolOutput::error(result.clone())
        } else {
            ToolOutput::text(result.clone()).with_entries(provider_executed_entries(output))
        };
        let preview = ToolResultPreview::build(name, &tool_output);
        let record = ToolCallRecord {
            id: call_id,
            chat_id,
            turn_id,
            // No client loop ever answers this call, so the id only has to be
            // unique and stable for the row it identifies.
            provider_id: format!("provider_executed_{call_id}"),
            name: name.clone(),
            arguments: input.clone(),
            raw_arguments: None,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: replay.clone(),
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: Utc::now(),
            resolved_at: None,
        };
        events.send(AgentEvent::ToolCallStarted {
            call_id,
            name: name.clone(),
        });
        if !matches!(
            self.accept_server_call_retry(&record).await?,
            AcceptedServerCall::Accepted
        ) {
            return Ok(());
        }
        let resolution = if *is_error {
            ToolCallResolution::Failed {
                result,
                error_code: output
                    .get("error_code")
                    .and_then(Value::as_str)
                    .unwrap_or(ToolErrorCategory::ToolFailed.as_str())
                    .to_owned(),
                error_detail: None,
            }
        } else {
            ToolCallResolution::Completed { result }
        };
        self.resolve_server_call_retry(chat_id, turn_id, call_id, &resolution, preview.as_ref())
            .await?;
        events.send(AgentEvent::ToolCallCompleted {
            call_id,
            output: tool_output,
            action: ToolActionPreview::build(name, input),
            result: preview,
        });
        Ok(())
    }

    /// Admit one server-executed call to the durable record before it runs, so
    /// a crash mid-tool still leaves a reconstructable `ToolUse` on the next
    /// turn.
    ///
    /// Returns the result an earlier attempt already committed for this call,
    /// which the caller replays instead of repeating the side effect.
    pub(crate) async fn accept_server_call(
        &self,
        chat_id: crate::id::ChatId,
        turn_id: TurnId,
        call: &PendingCall,
    ) -> Result<Option<ToolOutput>> {
        let (arguments, raw_arguments) = parse_args(&call.args);
        let record = ToolCallRecord {
            id: call.call_id,
            chat_id,
            turn_id,
            provider_id: call.provider_id.clone(),
            name: call.name.clone(),
            arguments,
            raw_arguments,
            execution: ToolCallExecution::Server,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: Utc::now(),
            resolved_at: None,
        };
        match self.accept_server_call_retry(&record).await? {
            AcceptedServerCall::Accepted => Ok(None),
            AcceptedServerCall::Existing(existing) if existing.status.is_terminal() => {
                let images = existing
                    .result_preview
                    .as_ref()
                    .and_then(exec_preview_images)
                    .unwrap_or(&[])
                    .to_vec();
                let content = existing.result.ok_or_else(|| {
                    AgentError::Store(format!(
                        "terminal tool call {} is missing its result",
                        call.call_id
                    ))
                })?;
                Ok(Some(ToolOutput {
                    content,
                    data: None,
                    is_error: existing.status != ToolCallStatus::Completed,
                    // Recovered from a durable row, whose category is already
                    // recorded there; re-deriving one here would be a guess.
                    error_category: None,
                    ui_view: None,
                    images,
                    image_data: ImageAttachments::new(),
                }))
            }
            AcceptedServerCall::Existing(_) => Ok(None),
            AcceptedServerCall::IdentityConflict => Err(AgentError::Store(format!(
                "tool call {} identity conflicts with its canonical request",
                call.call_id
            ))),
            AcceptedServerCall::LeaseLost => Err(AgentError::Store(format!(
                "turn {turn_id} lost its lease while accepting tool call {}",
                call.call_id
            ))),
        }
    }

    /// Run one admitted server call, announce it, and commit its result.
    pub(crate) async fn execute_server_call(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        call: &PendingCall,
        events: &EventSink<'_>,
        recovered: Option<ToolOutput>,
        repeat_refusal: Option<String>,
    ) -> Result<ToolOutput> {
        let (mut output, needs_resolution) = match recovered {
            Some(output) => (output, false),
            None if self.cancel.is_cancelled() => (
                ToolOutput::failed(
                    ToolErrorCategory::UserCancelled,
                    "turn cancelled before tool execution",
                ),
                true,
            ),
            // A repeated-call refusal answers the admitted row without
            // dispatching the tool, then resolves it below like any other
            // failure so recovery never finds it pending.
            None => match repeat_refusal {
                Some(reason) => (ToolOutput::error(reason), true),
                None => {
                    self.ensure_durable_lease_current(turn_id).await?;
                    (self.run_tool(chat, turn_id, call, events, None).await, true)
                }
            },
        };
        if needs_resolution {
            self.publish_tool_images(&mut output).await?;
        }
        let preview = ToolResultPreview::build(&call.name, &output);
        events.send(AgentEvent::ToolCallCompleted {
            call_id: call.call_id,
            output: self.tool_output_for_event(&output, call.call_id),
            action: call_action_preview(call),
            result: preview.clone(),
        });
        if needs_resolution {
            let resolution = if output.is_error {
                ToolCallResolution::Failed {
                    result: output.content.clone(),
                    error_code: output
                        .error_category
                        .unwrap_or(ToolErrorCategory::ToolFailed)
                        .as_str()
                        .into(),
                    error_detail: None,
                }
            } else {
                ToolCallResolution::Completed {
                    result: output.content.clone(),
                }
            };
            let outcome = self
                .resolve_server_call_retry(
                    chat.id,
                    turn_id,
                    call.call_id,
                    &resolution,
                    preview.as_ref(),
                )
                .await?;
            if !matches!(
                outcome,
                ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
            ) {
                return Err(AgentError::Store(format!(
                    "tool call {} could not be resolved: {outcome:?}",
                    call.call_id
                )));
            }
        }
        Ok(output)
    }

    pub(crate) async fn publish_tool_images(&self, output: &mut ToolOutput) -> Result<()> {
        if output.images.is_empty() {
            return Ok(());
        }
        let Some(blobs) = self.blobs.as_ref() else {
            output.images.clear();
            output.image_data.clear();
            output.content.push_str(
                "\n\nPreview images could not be retained because blob storage is unavailable.",
            );
            return Ok(());
        };
        for image in &output.images {
            image
                .validate()
                .map_err(|reason| AgentError::Store(reason.into()))?;
            let data = output.image_data.get(image.blob_id).ok_or_else(|| {
                AgentError::Store(format!(
                    "tool preview image {} is missing its bytes",
                    image.blob_id
                ))
            })?;
            if data.media_type() != image.media_type
                || u64::try_from(data.len()).unwrap_or(u64::MAX) != image.byte_len
            {
                return Err(AgentError::Store(format!(
                    "tool preview image {} does not match its descriptor",
                    image.blob_id
                )));
            }
            blobs.put(image.blob_id, data.bytes().to_vec()).await?;
        }
        output.image_data.clear();
        Ok(())
    }

    /// Answer a call this step did not run.
    ///
    /// The reader saw it start, so it has to be seen to finish; the model gets
    /// a result it can act on instead of a discarded step. Nothing is written
    /// to the record because nothing happened — the call has no side effect to
    /// recover and no place in a rebuilt history.
    pub(crate) fn decline_call(
        &self,
        call: &PendingCall,
        events: &EventSink<'_>,
        reason: String,
    ) -> ToolOutput {
        let output = ToolOutput::error(reason);
        events.send(AgentEvent::ToolCallCompleted {
            call_id: call.call_id,
            output: self.tool_output_for_event(&output, call.call_id),
            action: call_action_preview(call),
            result: ToolResultPreview::build(&call.name, &output),
        });
        output
    }

    /// Map one client call's model-facing arguments onto the canonical durable
    /// arguments its checkpoint stores.
    ///
    /// The output write-back tool is the only mapping today: the model names a
    /// published output by display filename (ids are never in its vocabulary),
    /// and the host resolves that name against the chat's live outputs exactly
    /// like the output scan does — `list_outputs` orders newest-updated first
    /// and excludes deleted outputs, so the first filename match is the live
    /// record the model named. Payloads that fail to parse pass through
    /// unchanged so [`Self::client_checkpoint`] reports them with its standard
    /// malformed-arguments answer.
    pub(crate) async fn resolve_client_call_arguments(
        &self,
        chat: &Chat,
        call: &PendingCall,
    ) -> Result<ClientArgumentResolution> {
        if call.name != crate::WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL {
            return Ok(ClientArgumentResolution::Unchanged);
        }
        let Some(proposal) = parse_tool_args(&call.args).and_then(|arguments| {
            serde_json::from_value::<crate::WriteOutputToConnectedFolderProposal>(arguments).ok()
        }) else {
            return Ok(ClientArgumentResolution::Unchanged);
        };
        if !proposal.is_well_formed() {
            return Ok(ClientArgumentResolution::Unchanged);
        }
        let outputs = self
            .store
            .list_outputs(chat.id, crate::OUTPUT_LOOKUP_LIMIT)
            .await?;
        let Some(output) = outputs
            .iter()
            .find(|output| output.filename == proposal.filename)
        else {
            return Ok(ClientArgumentResolution::Refused(format!(
                "not run: no live output in this conversation is named \"{}\". Use the exact filename of an output reported as published.",
                proposal.filename
            )));
        };
        let canonical = crate::WriteOutputToConnectedFolderArgs {
            output_id: *output.id.as_uuid(),
            root_id: proposal.root_id,
            path: proposal.path,
            mode: proposal.mode,
        };
        let arguments = serde_json::to_value(canonical)
            .map_err(|error| AgentError::Store(format!("unencodable write-back: {error}")))?;
        Ok(ClientArgumentResolution::Resolved(arguments))
    }

    /// The client-tool checkpoint for `call`, or what the model is told when
    /// the request cannot be made.
    ///
    /// `resolved_arguments`, when present, replaces the model's raw arguments
    /// with the canonical form produced by
    /// [`Self::resolve_client_call_arguments`].
    pub(crate) fn client_checkpoint(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        call: &PendingCall,
        resolved_arguments: Option<Value>,
        steer_revision: Option<i64>,
    ) -> std::result::Result<(crate::model::ClientToolCallRequest, i64), &'static str> {
        if self.tools.is_foreground_client(&call.name) && !self.agent_orchestration_active() {
            return Err("not run: that user continuation is available only from a durably claimed foreground turn. Continue without it.");
        }
        // Plan turns advertise only read-only client tools, and a call that
        // slipped past advertisement is refused here for the same reason
        // server-side mutations are: client execution is ungated by design,
        // so the only write gate a plan turn has is never issuing the request.
        if matches!(chat.permission_mode, Some(PermissionMode::Plan))
            && self.tools.registered_class(&call.name) != Some(ApprovalClass::ReadOnly)
        {
            return Err(
                "not run: this tool is not available in plan mode; the chat is read-only until the reader leaves plan mode. Continue with read-only tools.",
            );
        }
        let arguments = match resolved_arguments {
            Some(arguments) => arguments,
            None => {
                let Some(arguments) = parse_tool_args(&call.args) else {
                    return Err("not run: the client tool arguments were not valid JSON. Ask again with one complete JSON value.");
                };
                arguments
            }
        };
        let request = crate::model::ClientToolCallRequest {
            id: call.call_id,
            chat_id: chat.id,
            turn_id,
            provider_id: call.provider_id.clone(),
            name: call.name.clone(),
            arguments,
        };
        if !request.is_well_formed()
            || !self
                .tools
                .client_arguments_are_valid(&request.name, &request.arguments)
        {
            return Err("not run: the client tool request was too large or malformed. Ask again with a valid tool identity and smaller arguments.");
        }
        let Some(steer_revision) = steer_revision else {
            return Err(
                "not run: client-executed tools are available only from a durably claimed turn.",
            );
        };
        Ok((request, steer_revision))
    }

    /// The sandbox delegation checkpoint for `call`, or what the model is told
    /// when the request cannot be made.
    pub(crate) fn sandbox_checkpoint(
        &self,
        call: &PendingCall,
        steer_revision: Option<i64>,
    ) -> std::result::Result<(SandboxAgentSpawnRequest, i64), &'static str> {
        let Some(arguments) = parse_tool_args(&call.args) else {
            return Err("not run: the sandbox task arguments were not valid JSON. Ask again with one complete task value.");
        };
        let Some(task) = self.tools.sandbox_spawn_task(&call.name, &arguments) else {
            return Err("not run: the sandbox task needs one non-empty, bounded `task`. It may also include one `resource` object containing only `root_id` and `relative_path`; omit `resource` entirely when unused rather than sending null. Ask again with that exact shape.");
        };
        let Some(steer_revision) = steer_revision else {
            return Err("not run: sandbox delegation is available only from a durably claimed foreground turn.");
        };
        Ok((
            SandboxAgentSpawnRequest {
                call_id: call.call_id,
                provider_id: call.provider_id.clone(),
                child_run_id: AgentRunId::sandbox_for_spawn_call(call.call_id),
                task,
                arguments,
                approval_gated: false,
            },
            steer_revision,
        ))
    }

    /// The ordered child-wait checkpoint for `call`, or what the model is told
    /// when the request cannot be made.
    pub(crate) fn agent_wait_checkpoint(
        &self,
        call: &PendingCall,
        steer_revision: Option<i64>,
    ) -> std::result::Result<(ForegroundAgentWaitRequest, i64), &'static str> {
        let Some(arguments) = parse_tool_args(&call.args) else {
            return Err("not run: the wait_for_agents arguments were not valid JSON. Ask again with one complete ordered agent_ids value.");
        };
        let Some(child_run_ids) = self.tools.wait_for_agent_ids(&call.name, &arguments) else {
            return Err("not run: wait_for_agents requires one non-empty, bounded, unique agent_ids list with no extra properties.");
        };
        let Some(steer_revision) = steer_revision else {
            return Err(
                "not run: wait_for_agents is available only from a durably claimed foreground turn.",
            );
        };
        Ok((
            ForegroundAgentWaitRequest {
                call_id: call.call_id,
                provider_id: call.provider_id.clone(),
                child_run_ids,
                arguments,
            },
            steer_revision,
        ))
    }
    /// Resolve approval and execute one tool call, returning its output. Tool and
    /// approval failures surface as error output, never `Err`.
    pub(crate) async fn run_tool(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        call: &PendingCall,
        events: &EventSink<'_>,
        durable_approval: Option<&crate::approval::ToolApproval>,
    ) -> ToolOutput {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolOutput::failed(
                ToolErrorCategory::NotFound,
                format!("unknown tool: {}", call.name),
            );
        };
        // A garbled or truncated stream must be answered as invalid JSON, not
        // coerced to `{}` and run with no arguments: the tool would report a
        // missing field and the model would try to fix a request it had in
        // fact sent correctly. Refuse before the approval gate so the reader
        // is never asked about a call whose arguments could not be read, and
        // return the advertised schema so the model can re-emit the call.
        let spec = tool.spec();
        let Some(arguments) = parse_tool_args(&call.args) else {
            return ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                format!(
                    "arguments for {} were not valid JSON; re-send the call with arguments \
                     matching this schema: {}",
                    call.name, spec.input_schema
                ),
            );
        };
        // Well-formed JSON can still be the wrong call: enforcement used to be
        // whatever each tool's deserializer happened to do, and a mounted MCP
        // server's advertised contract was decorative. Hold every call to the
        // schema the model was shown, at the same refusal point, so the model
        // can re-emit the call instead of debugging a tool it never reached.
        if let Some(mismatch) = self.tools.schema_mismatch(&call.name, &arguments) {
            return ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                format!(
                    "arguments for {} do not satisfy its schema: {mismatch}; re-send the call \
                     with arguments matching this schema: {}",
                    call.name, spec.input_schema
                ),
            );
        }
        // Policy, decided in order: a standing grant the reader already made
        // covers its calls in every mode; otherwise the chat's permission mode
        // says which classes park on the gate. ReadOnly never parks; Workspace
        // parks only in Ask; Sensitive parks in everything but Allow.
        // Commit the approval request *before* emitting ApprovalRequired so a
        // client that sees the event can never race a 404 against a request
        // that exists only in this process.
        let approval_class = durable_approval
            .map(|approval| approval.class)
            .unwrap_or_else(|| tool.approval_class());
        // Plan mode is read-only by construction: a mutating call is refused
        // outright, never parked, so nothing the reader could approve — and
        // no standing grant made in another mode — lets a plan turn write.
        // A recovered call keeps its durable-approval path so a card that
        // was already pending resolves instead of dangling.
        if durable_approval.is_none()
            && matches!(chat.permission_mode, Some(PermissionMode::Plan))
            && approval_class != ApprovalClass::ReadOnly
        {
            return ToolOutput::failed(
                ToolErrorCategory::NotFound,
                format!(
                    "{} is not available in plan mode; this chat is read-only until \
                     the reader leaves plan mode. Continue with read-only tools.",
                    call.name
                ),
            );
        }
        let kind_for_call = ToolApprovalKind::for_call(&call.name, approval_class);
        // The action a standing grant is matched against, and the one the card
        // shows if this call ends up parking. Built once so a grant can never
        // be tested against a different reading of the arguments than the
        // human was shown.
        let action = call_action_preview(call);
        let bypass_by_explicit_grant = durable_approval.is_none()
            && self.standing_grants.covers(
                chat.id,
                chat.project_id,
                &call.name,
                kind_for_call,
                &arguments,
            );
        // A recovered call re-enters the gate whatever the mode now says: its
        // durable approval may already hold a rejection the mode must not
        // outrun, and a still-pending card must resolve, not dangle.
        let mode = chat.permission_mode.unwrap_or(PermissionMode::Ask);
        let gate_required = durable_approval.is_some()
            || match approval_class {
                ApprovalClass::ReadOnly => false,
                ApprovalClass::Workspace => matches!(mode, PermissionMode::Ask),
                ApprovalClass::Sensitive => !matches!(mode, PermissionMode::Allow),
            };
        if gate_required && !bypass_by_explicit_grant {
            let kind = durable_approval
                .map(|approval| approval.kind)
                .unwrap_or(kind_for_call);
            // In Auto, an uncovered judgeable call is offered to the judge as
            // it parks, so the placeholder is on the card from its first
            // frame. Only an exactly-describable action qualifies: the judge
            // must see the real query, never a clamped rendering of it.
            let auto_judge = matches!(mode, PermissionMode::Auto)
                && durable_approval.is_none()
                && crate::approval::is_auto_judge_candidate(kind, &call.name, &arguments);
            let auto_judging = durable_approval.map_or(auto_judge, |approval| {
                matches!(
                    approval.auto_judge_status,
                    Some(crate::approval::AutoJudgeStatus::Judging)
                )
            });
            // A recovered call re-presents the preview durable state already
            // holds, so a reconnecting client sees the same command it was
            // asked about before the restart.
            let preview = match durable_approval {
                Some(approval) => approval.preview.clone(),
                None => action.clone(),
            };
            if self.durable_steer_lease.is_some() && events.flush().await.is_err() {
                return ToolOutput::error("approval event journal is unavailable");
            }
            let journal = match (self.durable_steer_lease, events.proposed_ordinal()) {
                (_, Err(_)) => return ToolOutput::error("approval event journal is unavailable"),
                (Some(lease_token), Ok(Some(event_ordinal))) => Some(ApprovalJournalIdentity {
                    lease_token,
                    event_ordinal,
                }),
                (None, Ok(None)) => None,
                _ => return ToolOutput::error("approval event journal identity is invalid"),
            };
            let registering = self.approvals.register(
                ApprovalRequest {
                    call_id: call.call_id,
                    chat_id: chat.id,
                    turn_id,
                    tool_name: call.name.clone(),
                    class: approval_class,
                    kind,
                    preview: preview.clone(),
                    auto_judge,
                },
                journal,
            );
            let registration = match future::select(registering, self.cancel.cancelled()).await {
                Either::Left((registration, _)) if !self.cancel.is_cancelled() => registration,
                Either::Left(_) | Either::Right(((), _)) => {
                    return ToolOutput::failed(
                        ToolErrorCategory::UserCancelled,
                        "turn cancelled while registering approval",
                    );
                }
            };
            let required = AgentEvent::ApprovalRequired {
                auto_judging,
                call_id: call.call_id,
                tool_name: call.name.clone(),
                class: approval_class,
                kind,
                grant_scopes: GrantScope::mintable_ladder_for(kind, &call.name, &arguments),
                preview,
            };
            let authorized_by_standing_grant = matches!(
                registration.publication,
                ApprovalRequiredPublication::StandingGrant
            );
            match registration.publication {
                ApprovalRequiredPublication::Ordinary => events.send(required),
                ApprovalRequiredPublication::Committed {
                    event_ordinal,
                    event,
                } => {
                    if events
                        .send_committed_proposed(event_ordinal, event)
                        .is_err()
                    {
                        return ToolOutput::error("approval event publication is unavailable");
                    }
                }
                ApprovalRequiredPublication::Recovered {
                    event_ordinal,
                    event,
                } => {
                    if events
                        .send_recovered_proposed(event_ordinal, event)
                        .is_err()
                    {
                        return ToolOutput::error("approval event recovery is unavailable");
                    }
                }
                ApprovalRequiredPublication::None => {}
                ApprovalRequiredPublication::StandingGrant => {}
            }
            let pending = registration.decision;
            // Race the decision against cancellation so a turn parked on approval
            // can still be stopped. On cancel we close the approval card
            // (`ApprovalDecided { approved: false }`) and return an error result;
            // the loop's post-tool check then ends the turn as cancelled.
            //
            // `future::select` polls the left arm first, so when both are ready
            // (approve lands in the same tick as cancel) the decision would win
            // and a Sensitive tool would still run. Prefer cancel whenever the
            // token is already tripped (same idea as the post-stream\n            // `is_cancelled()` re-check after `select`).
            let decision = match future::select(pending, self.cancel.cancelled()).await {
                Either::Left((decision, _)) if !self.cancel.is_cancelled() => decision,
                Either::Left(_) | Either::Right(((), _)) => {
                    if !authorized_by_standing_grant {
                        events.send(AgentEvent::ApprovalDecided {
                            call_id: call.call_id,
                            approved: false,
                        });
                    }
                    return ToolOutput::failed(
                        ToolErrorCategory::UserCancelled,
                        "turn cancelled while awaiting approval",
                    );
                }
            };
            let approved = matches!(decision, ApprovalDecision::Approve);
            if !authorized_by_standing_grant {
                events.send(AgentEvent::ApprovalDecided {
                    call_id: call.call_id,
                    approved,
                });
            }
            if let ApprovalDecision::Reject { reason } = decision {
                return ToolOutput::failed(ToolErrorCategory::UserDeclined, reason);
            }
            // A cancel that lands after Approve won `select` but before execute
            // (concurrent trip of the token) must not run the Sensitive tool.
            if self.cancel.is_cancelled() {
                return ToolOutput::failed(
                    ToolErrorCategory::UserCancelled,
                    "turn cancelled while awaiting approval",
                );
            }
        }
        // Cancellation can land after the caller's loop-level fence or while a
        // recovered call is being classified. Recheck at the final boundary
        // before any ReadOnly, Workspace, or approved Sensitive implementation
        // can observe arguments or perform a side effect.
        if self.cancel.is_cancelled() {
            return ToolOutput::failed(
                ToolErrorCategory::UserCancelled,
                "turn cancelled before tool execution",
            );
        }
        let ctx = self
            .config
            .tool_scratch
            .as_ref()
            .map_or_else(
                || ToolCtx::without_private_scratch(chat.id, chat.project_id),
                |scratch| ToolCtx::with_private_scratch(chat.id, chat.project_id, scratch.clone()),
            )
            .with_call_id(call.call_id);
        // `future::select` polls cancellation first. If it wins, dropping the
        // unselected execution future propagates cancellation into async tools
        // such as reqwest instead of leaving egress alive after the turn ends.
        // Recheck after the execution arm wins to close a same-tick race.
        let executing = tool.execute(&ctx, arguments);
        let mut output = match future::select(self.cancel.cancelled(), executing).await {
            Either::Left(((), _)) => ToolOutput::failed(
                ToolErrorCategory::UserCancelled,
                "turn cancelled during tool execution",
            ),
            Either::Right((_, _)) if self.cancel.is_cancelled() => ToolOutput::failed(
                ToolErrorCategory::UserCancelled,
                "turn cancelled during tool execution",
            ),
            Either::Right((result, _)) => match result {
                Ok(output) => output,
                Err(err) => ToolOutput::error(err.to_string()),
            },
        };
        // Clamped to what the record may hold, not to what the model is fed.
        // Those are different questions: one is storage, the other is a
        // context budget. Cutting to the feedback bound here used to destroy
        // the remainder before it was ever written down.
        if let Some(truncated) = truncate_to_bytes(
            &output.content,
            crate::model::ToolCallRecord::MAX_RESULT_BYTES,
            None,
        ) {
            output.content = truncated;
        }
        output
    }

    /// The tool result as the model sees it, bounded by the turn's feedback
    /// budget rather than by what the record holds.
    pub(crate) fn tool_result_for_model(&self, content: &str, call_id: CallId) -> String {
        truncate_to_bytes(content, self.config.max_tool_result_bytes, Some(call_id))
            .unwrap_or_else(|| content.to_owned())
    }

    /// The completion event's copy of a result.
    ///
    /// Bounded like the model's copy, not like the record's: this rides the
    /// journaled event stream, so it must not grow just because the record is
    /// now allowed to keep more.
    pub(crate) fn tool_output_for_event(&self, output: &ToolOutput, call_id: CallId) -> ToolOutput {
        ToolOutput {
            content: self.tool_result_for_model(&output.content, call_id),
            ..output.clone()
        }
    }

    /// Decide whether one delegation may create a child, before it does.
    ///
    /// A background run's own tool calls are advanced by the sandbox worker
    /// under its own lease and never re-enter this chat's gate, so the spawn is
    /// the only point at which the reader can be asked. It is asked once, for
    /// the whole run: nobody is watching a background run, and a mid-run card
    /// would stall it against its own deadline.
    ///
    /// A spawn's tool call is normally written already completed, in the same
    /// transaction that admits the child, which leaves the approval broker
    /// nothing to park on. A gated spawn therefore first accepts an ordinary
    /// pending server row and parks on that, exactly like any other Sensitive
    /// call; [`crate::Store::checkpoint_sandbox_spawn`] finalizes that same row
    /// when it admits the child, strictly after the decision has committed.
    ///
    /// An interrupted decision leaves the row pending, which
    /// [`Self::resume_pending_server_calls`] abandons on the next attempt. That
    /// is the fail-closed direction: no child is admitted by a decision nobody
    /// finished making.
    pub(crate) async fn gate_sandbox_spawn(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        request: &SandboxAgentSpawnRequest,
        events: &EventSink<'_>,
    ) -> Result<SandboxSpawnGate> {
        const KIND: ToolApprovalKind = ToolApprovalKind::DelegateMayRunBackgroundAgent;
        let mode = chat.permission_mode.unwrap_or(PermissionMode::Ask);
        // `Allow` is the chat saying it will not be asked. A standing grant is
        // the reader having already answered this exact question here, which is
        // what keeps repeat delegation in one conversation from re-prompting.
        // Either way the spawn takes the ordinary ungated path and no durable
        // pending row is created.
        if matches!(mode, PermissionMode::Allow)
            || self.standing_grants.covers(
                chat.id,
                chat.project_id,
                crate::SPAWN_SANDBOX_AGENT_TOOL,
                KIND,
                &request.arguments,
            )
        {
            return Ok(SandboxSpawnGate::Admit(request.clone()));
        }
        let call = PendingCall {
            call_id: request.call_id,
            provider_id: request.provider_id.clone(),
            name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            args: serde_json::to_string(&request.arguments)?,
        };
        // What the reader is deciding. The task says what the child is told to
        // do; the network policy says what it can do with what it learns, and
        // is the part that is actually being consented to — the run's workspace
        // is keyed by its own id and carries no folder grants, staged host
        // paths, or chat attachments.
        let preview = Some(ToolActionPreview::DelegateAgent {
            task: request.task.clone(),
            network: chat.network_policy.clone(),
        });
        if let Some(committed) = self.accept_server_call(chat.id, turn_id, &call).await? {
            // An earlier attempt already answered this exact call. Replay what
            // it committed rather than asking a second time.
            return Ok(SandboxSpawnGate::Declined(
                self.settle_gated_spawn(chat.id, turn_id, &call, events, preview, committed)
                    .await?,
            ));
        }
        macro_rules! refuse {
            ($output:expr) => {
                return Ok(SandboxSpawnGate::Declined(
                    self.settle_gated_spawn(chat.id, turn_id, &call, events, preview, $output)
                        .await?,
                ))
            };
        }
        if self.durable_steer_lease.is_some() && events.flush().await.is_err() {
            refuse!(ToolOutput::error("approval event journal is unavailable"));
        }
        let journal = match (self.durable_steer_lease, events.proposed_ordinal()) {
            (Some(lease_token), Ok(Some(event_ordinal))) => Some(ApprovalJournalIdentity {
                lease_token,
                event_ordinal,
            }),
            (None, Ok(None)) => None,
            _ => refuse!(ToolOutput::error(
                "approval event journal identity is invalid"
            )),
        };
        let registering = self.approvals.register(
            ApprovalRequest {
                call_id: request.call_id,
                chat_id: chat.id,
                turn_id,
                tool_name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
                class: ApprovalClass::Sensitive,
                kind: KIND,
                preview: preview.clone(),
                // A judge deciding a whole unattended run is not the same
                // question as a judge deciding one call, so delegation is never
                // handed to it.
                auto_judge: false,
            },
            journal,
        );
        let registration = match future::select(registering, self.cancel.cancelled()).await {
            Either::Left((registration, _)) if !self.cancel.is_cancelled() => registration,
            Either::Left(_) | Either::Right(((), _)) => {
                refuse!(ToolOutput::failed(
                    ToolErrorCategory::UserCancelled,
                    "turn cancelled while registering delegation approval",
                ));
            }
        };
        let required = AgentEvent::ApprovalRequired {
            auto_judging: false,
            call_id: request.call_id,
            tool_name: crate::SPAWN_SANDBOX_AGENT_TOOL.into(),
            class: ApprovalClass::Sensitive,
            kind: KIND,
            grant_scopes: GrantScope::mintable_ladder_for(
                KIND,
                crate::SPAWN_SANDBOX_AGENT_TOOL,
                &request.arguments,
            ),
            preview: preview.clone(),
        };
        let authorized_by_standing_grant = matches!(
            registration.publication,
            ApprovalRequiredPublication::StandingGrant
        );
        match registration.publication {
            ApprovalRequiredPublication::Ordinary => events.send(required),
            ApprovalRequiredPublication::Committed {
                event_ordinal,
                event,
            } => {
                if events
                    .send_committed_proposed(event_ordinal, event)
                    .is_err()
                {
                    refuse!(ToolOutput::error(
                        "approval event publication is unavailable"
                    ));
                }
            }
            ApprovalRequiredPublication::Recovered {
                event_ordinal,
                event,
            } => {
                if events
                    .send_recovered_proposed(event_ordinal, event)
                    .is_err()
                {
                    refuse!(ToolOutput::error("approval event recovery is unavailable"));
                }
            }
            ApprovalRequiredPublication::None | ApprovalRequiredPublication::StandingGrant => {}
        }
        let decision = match future::select(registration.decision, self.cancel.cancelled()).await {
            Either::Left((decision, _)) if !self.cancel.is_cancelled() => decision,
            Either::Left(_) | Either::Right(((), _)) => {
                if !authorized_by_standing_grant {
                    events.send(AgentEvent::ApprovalDecided {
                        call_id: request.call_id,
                        approved: false,
                    });
                }
                refuse!(ToolOutput::failed(
                    ToolErrorCategory::UserCancelled,
                    "turn cancelled while awaiting delegation approval",
                ));
            }
        };
        let approved = matches!(decision, ApprovalDecision::Approve);
        if !authorized_by_standing_grant {
            events.send(AgentEvent::ApprovalDecided {
                call_id: request.call_id,
                approved,
            });
        }
        if let ApprovalDecision::Reject { reason } = decision {
            refuse!(ToolOutput::failed(ToolErrorCategory::UserDeclined, reason));
        }
        // A cancel landing concurrently with the approval must not admit a
        // child that will keep running after the turn has stopped.
        if self.cancel.is_cancelled() {
            refuse!(ToolOutput::failed(
                ToolErrorCategory::UserCancelled,
                "turn cancelled after delegation approval",
            ));
        }
        Ok(SandboxSpawnGate::Admit(SandboxAgentSpawnRequest {
            approval_gated: true,
            ..request.clone()
        }))
    }

    /// Close a delegation that will not happen: resolve its durable pending row
    /// and publish the result the model reads.
    pub(crate) async fn settle_gated_spawn(
        &self,
        chat_id: ChatId,
        turn_id: TurnId,
        call: &PendingCall,
        events: &EventSink<'_>,
        preview: Option<ToolActionPreview>,
        output: ToolOutput,
    ) -> Result<ToolOutput> {
        let resolution = if output.is_error {
            ToolCallResolution::Failed {
                result: output.content.clone(),
                error_code: output
                    .error_category
                    .unwrap_or(ToolErrorCategory::ToolFailed)
                    .as_str()
                    .into(),
                error_detail: None,
            }
        } else {
            ToolCallResolution::Completed {
                result: output.content.clone(),
            }
        };
        let outcome = self
            .resolve_server_call_retry(chat_id, turn_id, call.call_id, &resolution, None)
            .await?;
        if !matches!(
            outcome,
            ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
        ) {
            return Err(AgentError::Store(format!(
                "refused delegation {} could not be resolved: {outcome:?}",
                call.call_id
            )));
        }
        events.send(AgentEvent::ToolCallCompleted {
            call_id: call.call_id,
            output: self.tool_output_for_event(&output, call.call_id),
            action: preview,
            result: None,
        });
        Ok(output)
    }

    /// Resume persisted server calls accepted by an earlier attempt before
    /// asking the provider for new output.
    ///
    /// An approval-bearing call is admitted only once every sibling in its step
    /// is terminal, so recovery never has to choose which of several pending
    /// rows an interrupted approval belonged to. The check below states that as
    /// an invariant rather than relying on it: a batch that violates it was
    /// written by something other than this loop, and guessing would risk
    /// re-running a call the reader approved for different arguments.
    pub(crate) async fn resume_pending_server_calls(
        &self,
        chat: &Chat,
        turn_id: TurnId,
        events: &EventSink<'_>,
        transcript: &mut Vec<ChatMessage>,
    ) -> Result<()> {
        let pending = self
            .store
            .list_tool_calls(chat.id)
            .await?
            .into_iter()
            .filter(|call| {
                call.turn_id == turn_id
                    && call.execution == ToolCallExecution::Server
                    && call.status == ToolCallStatus::Pending
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        let mut approval_bearing = 0usize;
        for call in &pending {
            if self.store.get_tool_call_approval(call.id).await?.is_some()
                || self
                    .tools
                    .get(&call.name)
                    .is_some_and(|tool| tool.approval_class() == ApprovalClass::Sensitive)
            {
                approval_bearing += 1;
            }
        }
        if approval_bearing > 0 && (pending.len() != 1 || approval_bearing != 1) {
            return Err(AgentError::Store(format!(
                "turn {turn_id} has an ambiguous pending sensitive tool batch"
            )));
        }
        for stored in pending {
            let call = PendingCall {
                call_id: stored.id,
                provider_id: stored.provider_id,
                name: stored.name,
                args: serde_json::to_string(&stored.arguments)?,
            };
            let durable_approval = self.store.get_tool_call_approval(call.call_id).await?;
            if self.durable_steer_lease.is_some() {
                // A pending call recovered at startup is ambiguous: the prior
                // process may have performed its side effect and died
                // before committing the result. Never execute it again. Commit
                // a deterministic failed result under this attempt's lease so
                // the model can recover without double-applying the effect.
                let output = ToolOutput::error(
                    "tool execution was interrupted before its result was committed; the call was not replayed",
                );
                let resolution = ToolCallResolution::Failed {
                    result: output.content.clone(),
                    error_code: "tool_execution_interrupted".into(),
                    error_detail: Some(
                        "a prior turn attempt may have executed this call; replay was suppressed"
                            .into(),
                    ),
                };
                let outcome = self
                    .abandon_inherited_server_call_retry(
                        chat.id,
                        turn_id,
                        call.call_id,
                        &resolution,
                    )
                    .await?;
                if !matches!(
                    outcome,
                    ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
                ) {
                    return Err(AgentError::Store(format!(
                        "inherited tool call {} could not be abandoned: {outcome:?}",
                        call.call_id
                    )));
                }
                if durable_approval.is_some() {
                    if let Some(approval) = self.store.get_tool_call_approval(call.call_id).await? {
                        events.send(AgentEvent::ApprovalDecided {
                            call_id: call.call_id,
                            approved: matches!(
                                approval.status,
                                crate::approval::ToolApprovalStatus::Approved
                            ),
                        });
                    }
                }
                events.send(AgentEvent::ToolCallCompleted {
                    call_id: call.call_id,
                    output: self.tool_output_for_event(&output, call.call_id),
                    action: call_action_preview(&call),
                    result: ToolResultPreview::build(&call.name, &output),
                });
                transcript.push(ChatMessage {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: call.provider_id,
                        content: output.content,
                        is_error: true,
                    }],
                    reasoning: MessageReasoning::default(),
                });
                continue;
            }
            let tool_available = self.tools.get(&call.name).is_some();
            let cancelled_before_run = self.cancel.is_cancelled();
            let mut output = if cancelled_before_run {
                ToolOutput::failed(
                    ToolErrorCategory::UserCancelled,
                    "turn cancelled before recovered tool execution",
                )
            } else {
                self.run_tool(chat, turn_id, &call, events, durable_approval.as_ref())
                    .await
            };
            self.publish_tool_images(&mut output).await?;
            let preview = ToolResultPreview::build(&call.name, &output);
            let resolution = if output.is_error {
                ToolCallResolution::Failed {
                    result: output.content.clone(),
                    error_code: output
                        .error_category
                        .unwrap_or(ToolErrorCategory::ToolFailed)
                        .as_str()
                        .into(),
                    error_detail: None,
                }
            } else {
                ToolCallResolution::Completed {
                    result: output.content.clone(),
                }
            };
            let outcome = self
                .store
                .resolve_server_tool_call_with_artifacts(
                    call.call_id,
                    &resolution,
                    Utc::now(),
                    preview.as_ref(),
                )
                .await?;
            if !matches!(
                outcome,
                ResolveToolCallOutcome::Resolved | ResolveToolCallOutcome::Existing
            ) {
                return Err(AgentError::Store(format!(
                    "pending tool call {} could not be recovered: {outcome:?}",
                    call.call_id
                )));
            }
            // A missing implementation cannot enter `run_tool`'s approval
            // branch. Resolution above atomically closes any still-pending
            // approval with the failed call. Read back the winner so an
            // approve-vs-resolution race projects the authoritative decision.
            if durable_approval.is_some() && (!tool_available || cancelled_before_run) {
                if let Some(approval) = self.store.get_tool_call_approval(call.call_id).await? {
                    events.send(AgentEvent::ApprovalDecided {
                        call_id: call.call_id,
                        approved: matches!(
                            approval.status,
                            crate::approval::ToolApprovalStatus::Approved
                        ),
                    });
                }
            }
            events.send(AgentEvent::ToolCallCompleted {
                call_id: call.call_id,
                output: self.tool_output_for_event(&output, call.call_id),
                action: call_action_preview(&call),
                result: preview,
            });
            transcript.push(ChatMessage {
                role: Role::User,
                reasoning: MessageReasoning::default(),
                content: tool_result_blocks(
                    call.provider_id,
                    self.tool_result_for_model(&output.content, call.call_id),
                    output.is_error,
                    &output.images,
                    self.config.image_input,
                ),
            });
        }
        Ok(())
    }

}
