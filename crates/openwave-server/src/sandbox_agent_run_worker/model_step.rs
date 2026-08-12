//! Model-step request assembly and completion for sandbox agent runs.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use openwave_core::{
    parse_update_task_plan_arguments, sandbox_done_tool_spec, sandbox_exec_tool_spec,
    sandbox_folder_access_proposal_tool_spec, sandbox_read_delegated_file_tool_spec,
    sandbox_update_task_plan_tool_spec, sandbox_web_search_tool_spec,
    validate_sandbox_exec_arguments, validate_sandbox_read_delegated_file_arguments, AgentConfig,
    AgentError, AgentRun, ChatMessage, ChatRequest, ContentBlock, MessageReasoning, ModelProvider,
    ProviderEvent, RequestFolderAccessArgs, Result, Role, SandboxToolCall, SandboxToolCallStatus,
    StopReason, Store, ToolCallRecord, TurnWebSearch, SANDBOX_EXEC_TOOL, UPDATE_TASK_PLAN_TOOL,
};

use super::config::*;

/// Model steps a run may spend past its cadence answering calls it should not
/// have made.
///
/// Tool advertisement is withdrawn two steps before the cadence ends, but
/// withdrawal is not a guarantee: a model that has called `exec` on every step
/// of a long transcript will sometimes call it once more after it disappears.
/// That call still has to be answered — the transcript is rebuilt from durable
/// rows, so a call with no row never happened and would be re-made on replay —
/// and the answer costs a model step like any other. The grace is the room
/// those refusal steps live in. It is never advertised and nothing in it
/// executes; by the time it is spent the transcript says twice, in words, that
/// the run must finish.
pub(super) const SANDBOX_STEP_REFUSAL_GRACE: usize = 2;

pub(super) fn delegated_file_admission_matches(
    run: &AgentRun,
    admission: &openwave_core::SandboxAgentAdmission,
    chat: &openwave_core::Chat,
) -> bool {
    admission.child_run_id == run.id
        && admission.chat_id == run.chat_id
        && chat.id == run.chat_id
        && admission.resource.as_ref().is_some_and(|resource| {
            resource.is_well_formed()
                && chat
                    .root_attachments
                    .iter()
                    .any(|attachment| attachment.root_id == resource.root_id)
        })
}

/// Split a run's parked calls into the model steps that produced them.
///
/// The list arrives ordered by park attempt, park claim, then batch ordinal, so
/// each step is a contiguous run of rows sharing one park identity, and the
/// ordinal is the order within it.
pub(super) fn sandbox_call_steps(calls: &[SandboxToolCall]) -> Vec<&[SandboxToolCall]> {
    let mut steps = Vec::new();
    let mut start = 0;
    for index in 1..=calls.len() {
        let boundary = index == calls.len()
            || (
                calls[index].park_attempt_count,
                calls[index].park_claim_count,
            ) != (
                calls[start].park_attempt_count,
                calls[start].park_claim_count,
            );
        if boundary {
            steps.push(&calls[start..index]);
            start = index;
        }
    }
    steps
}

pub(super) async fn sandbox_request(
    config: &AgentConfig,
    task: String,
    calls: &[SandboxToolCall],
    store: &dyn Store,
    delegated_file_available: bool,
    skills: &[openwave_code_execution::SkillPackage],
    plugins: &[openwave_code_execution::PluginPackage],
) -> Result<ChatRequest> {
    // Steps are the one budget this request rides on: each is one model
    // completion, and the whole chain is replayed on every claim, so the
    // cadence bounds context growth and model spend at once. Rows are not
    // separately bounded — they cannot grow without steps growing.
    let steps = sandbox_call_steps(calls);
    if config.max_steps == 0
        || steps.len().saturating_add(1) > config.max_steps + SANDBOX_STEP_REFUSAL_GRACE
    {
        return Err(AgentError::msg("sandbox model-step budget exceeded"));
    }
    let mut messages = vec![ChatMessage::text(Role::User, task)];
    for step in &steps {
        // The row's name is the model's own emitted name — dispatch data, not
        // a replay contract. A call the host refused replays exactly like one
        // an executor ran: what matters is that it is finished and has an
        // answer to hand back.
        let mut tool_uses = Vec::with_capacity(step.len());
        let mut tool_results = Vec::with_capacity(step.len());
        for call in *step {
            if !call.status.is_terminal() {
                return Err(AgentError::msg("sandbox checkpoint cannot be resumed"));
            }
            let receipt = store
                .get_sandbox_tool_call_receipt(call.id)
                .await?
                .ok_or_else(|| AgentError::msg("sandbox checkpoint is missing its receipt"))?;
            tool_uses.push(ContentBlock::ToolUse {
                id: call.provider_id.clone(),
                name: call.name.clone(),
                input: call.arguments.clone(),
            });
            tool_results.push(ContentBlock::ToolResult {
                tool_use_id: call.provider_id.clone(),
                content: receipt.result,
                is_error: receipt.status != SandboxToolCallStatus::Completed,
            });
        }
        // One step is one assistant message carrying all of its tool uses, then
        // one message carrying all of their results — the shape the model
        // produced, so the shape it reads back.
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: tool_uses,
            reasoning: MessageReasoning::default(),
        });
        messages.push(ChatMessage {
            role: Role::User,
            content: tool_results,
            reasoning: MessageReasoning::default(),
        });
    }
    // A tool that runs out of cadence simply stops being offered, and a model
    // reading a transcript where it used that tool on every step reads the
    // absence as an oversight rather than a rule — then calls it anyway. Say
    // it in words instead. The notice is derived from the run's own step count,
    // so a replayed claim rebuilds the identical request.
    if let Some(notice) = cadence_notice(config, steps.len()) {
        if let Some(last) = messages.last_mut() {
            last.content.push(ContentBlock::Text { text: notice });
        }
    }
    Ok(ChatRequest {
        provider: config.provider.clone(),
        model: config.model.clone(),
        reasoning_model: config.reasoning_model,
        system: Some(sandbox_system_prompt(
            delegated_file_available,
            config.web_search,
            config.tools_supported,
            skills,
            plugins,
        )),
        messages,
        // A checkpoint costs one model completion now and one more to consume
        // its receipt, so anything that parks is advertised only while the
        // remaining cadence can pay for both — never advertise work the run
        // cannot consume.
        tools: sandbox_tools(config, steps.len(), delegated_file_available),
        max_tokens: config.max_tokens,
        temperature: config.temperature,
        reasoning_effort: config.reasoning_effort,
        // Set for exactly the runs the host routed to the provider's own
        // search. The budget is per request, and every claim replays the whole
        // chain, so a resumed run gets the same allowance its earlier steps had.
        vendor_web_search: match config.web_search {
            TurnWebSearch::Vendor(vendor) if config.tools_supported => Some(vendor),
            TurnWebSearch::Host | TurnWebSearch::Off => None,
            TurnWebSearch::Vendor(_) => None,
        },
        // Sandbox runs replay text and tool blocks from checkpoints; no path
        // puts an image block in this transcript.
        images: openwave_core::ImageAttachments::new(),
        ..Default::default()
    })
}

/// What to tell a run whose tools have started disappearing under it.
///
/// Withdrawing a tool is how the cadence is enforced, but it is not how the
/// cadence is communicated: nothing in the request says a tool used to be
/// there. This is the sentence that says so, and it names `done` explicitly
/// because the only useful move left is to submit what the run already has.
///
/// A run whose model has no tools at all is not told anything — it has no move
/// to make, and the step budget ends it.
fn cadence_notice(config: &AgentConfig, steps: usize) -> Option<String> {
    if !config.tools_supported {
        return None;
    }
    if steps.saturating_add(2) > config.max_steps {
        Some(
            "You have used this task's entire step budget. exec and search are no longer \
             available and calling them will not run anything. Finish now: call done with the \
             filenames you wrote under output/ and a short summary of what you produced."
                .to_owned(),
        )
    } else {
        None
    }
}

/// The tools one model step is offered, given what it can still afford.
fn sandbox_tools(
    config: &AgentConfig,
    steps: usize,
    delegated_file_available: bool,
) -> Vec<openwave_core::ToolSpec> {
    if !config.tools_supported {
        return Vec::new();
    }
    if steps.saturating_add(1) > config.max_steps {
        return Vec::new();
    }
    let can_checkpoint = steps.saturating_add(2) <= config.max_steps;
    let mut tools = Vec::new();
    if can_checkpoint {
        tools.push(sandbox_exec_tool_spec());
        // The host tool and the provider's own search are one capability with
        // one name, so the host one is advertised for exactly the runs the host
        // routed here. A vendor run's searches arrive already finished and
        // never become checkpoints; an off run is offered no search at all.
        if config.web_search == TurnWebSearch::Host {
            tools.push(sandbox_web_search_tool_spec());
        }
        tools.push(sandbox_update_task_plan_tool_spec());
    }
    // Both of these terminate the run in place of a final answer, so they need
    // no follow-up completion and stay available to the last step. Submission
    // especially: a run that spent its budget writing files must still be able
    // to hand them over.
    tools.push(sandbox_done_tool_spec());
    tools.push(sandbox_folder_access_proposal_tool_spec());
    if can_checkpoint && delegated_file_available {
        tools.push(sandbox_read_delegated_file_tool_spec());
    }
    tools
}

/// One completed model step: the tool checkpoint or terminal outcome it
/// produced, plus whatever the model said on the way there.
#[derive(Debug)]
pub(super) struct SandboxStep {
    /// Text the model produced before checkpointing on a tool.
    ///
    /// This is the run's own account of what it is about to do, and between
    /// checkpoints it is the only thing an observer could see. A step that ends
    /// the run carries nothing here — its result text already says it.
    pub(super) narration: String,
    pub(super) completion: SandboxCompletion,
    /// Searches the model provider ran on its own infrastructure during this
    /// step, in the order it reported them.
    ///
    /// They are already finished when they arrive and their results are inside
    /// the completion the model is writing, so nothing here executes or replays
    /// them — a later claim replays only durable checkpoints, and a provider
    /// that needs the same information again simply searches again.
    pub(super) provider_executed: Vec<ProviderExecutedCall>,
}

/// One tool call the model provider ran itself, as this run records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProviderExecutedCall {
    pub(super) name: String,
    /// The query the provider ran, when its arguments carry one.
    pub(super) query: Option<String>,
    pub(super) is_error: bool,
}

impl ProviderExecutedCall {
    /// One line of the run's own account of the call, for its progress feed.
    pub(super) fn progress_line(&self) -> String {
        let Self {
            name,
            query,
            is_error,
        } = self;
        match (query, is_error) {
            (Some(query), false) => format!("Ran {name}: {query}"),
            (None, false) => format!("Ran {name}."),
            (Some(query), true) => format!("{name} failed: {query}"),
            (None, true) => format!("{name} failed."),
        }
    }
}

#[derive(Debug)]
pub(super) enum SandboxCompletion {
    Final(String),
    /// The run's own files, offered as the deliverables for the task.
    Done {
        /// The call the model made, kept so the host can hand this step back
        /// unfinished instead of accepting it — see the plan reminder in
        /// [`super::worker`].
        provider_id: String,
        arguments: serde_json::Value,
        outputs: Vec<String>,
        summary: String,
    },
    /// Every tool call one model step made, in the order it emitted them.
    /// Each is either dispatched or answered in place; the step parks as one
    /// batch either way. Never empty.
    ToolCalls(Vec<SandboxToolCallIntent>),
    FolderAccessProposal {
        request: RequestFolderAccessArgs,
    },
}

/// One tool call the model emitted, classified for the host.
#[derive(Debug, Clone)]
pub(super) struct SandboxToolCallIntent {
    pub(super) provider_id: String,
    pub(super) name: String,
    /// The model's arguments as it sent them, `Null` when its JSON never
    /// parsed. A rejected call parks these verbatim so the replayed transcript
    /// shows the model exactly what it asked for.
    pub(super) arguments: serde_json::Value,
    pub(super) disposition: SandboxToolCallDisposition,
}

#[derive(Debug, Clone)]
pub(super) enum SandboxToolCallDisposition {
    /// Dispatch to the tool's executor lane.
    Execute,
    /// The host answers this call itself with an error result carrying this
    /// text, so the model can correct itself on the next step.
    Rejected {
        error_code: &'static str,
        message: String,
    },
}

/// The model-facing text of a refusal, shaped to what a durable receipt
/// accepts: never empty, never carrying a NUL the model's own tool name might
/// have contributed, and never longer than the receipt's result budget.
pub(super) fn rejection_result(message: &str) -> String {
    let mut result: String = message.chars().filter(|byte| *byte != '\0').collect();
    if result.len() > openwave_core::SandboxToolCall::MAX_RESULT_BYTES {
        let mut cut = openwave_core::SandboxToolCall::MAX_RESULT_BYTES;
        while cut > 0 && !result.is_char_boundary(cut) {
            cut -= 1;
        }
        result.truncate(cut);
    }
    if result.trim().is_empty() {
        return "The call could not be dispatched.".to_owned();
    }
    result
}

/// Decide whether a tool call the model emitted can be dispatched.
///
/// A call the host cannot dispatch is not an infrastructure failure: the model
/// asked for something that does not exist or sent arguments that do not fit,
/// and the corrective answer belongs in the transcript. Only a call with no
/// provider id is unanswerable — there is no id to attach a result to.
pub(super) fn classify_sandbox_tool_call(
    provider_id: String,
    name: String,
    raw_arguments: &str,
    advertised: &[String],
) -> Result<SandboxToolCallIntent> {
    if provider_id.is_empty() {
        return Err(AgentError::msg(
            "sandbox agent requested a tool without a call id",
        ));
    }
    // Parsed once, up front: whatever the model sent is what the checkpoint
    // records, so the replayed transcript shows it exactly what it asked for.
    let parsed = serde_json::from_str::<serde_json::Value>(raw_arguments).ok();
    let arguments = parsed.clone().unwrap_or(serde_json::Value::Null);
    if !advertised.contains(&name) {
        let available = advertised.join(", ");
        let message =
            format!("{name} is not available to a background task. Available tools: {available}.");
        return Ok(SandboxToolCallIntent {
            provider_id,
            name,
            arguments,
            disposition: SandboxToolCallDisposition::Rejected {
                error_code: "unavailable_tool",
                message,
            },
        });
    }
    let invalid = |arguments: serde_json::Value| SandboxToolCallIntent {
        provider_id: provider_id.clone(),
        name: name.clone(),
        arguments,
        disposition: SandboxToolCallDisposition::Rejected {
            error_code: "invalid_arguments",
            message: format!(
                "Arguments for {name} were not valid: re-send the call with arguments matching \
                 this tool's input schema."
            ),
        },
    };
    if parsed.is_none() {
        return Ok(invalid(serde_json::Value::Null));
    }
    // The plan validator's message is the correction — the rule it enforces
    // (one `in_progress` step, whole list every time) is not expressible in the
    // advertised schema, so a generic "arguments were not valid" would tell the
    // model nothing it could act on.
    if name == UPDATE_TASK_PLAN_TOOL {
        if let Err(correction) = parse_update_task_plan_arguments(&arguments) {
            return Ok(SandboxToolCallIntent {
                provider_id,
                name,
                arguments,
                disposition: SandboxToolCallDisposition::Rejected {
                    error_code: "invalid_arguments",
                    message: correction,
                },
            });
        }
        return Ok(SandboxToolCallIntent {
            provider_id,
            name,
            arguments,
            disposition: SandboxToolCallDisposition::Execute,
        });
    }
    let valid = match name.as_str() {
        SANDBOX_EXEC_TOOL => validate_sandbox_exec_arguments(&arguments),
        openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL => {
            validate_sandbox_read_delegated_file_arguments(&arguments)
        }
        openwave_core::SANDBOX_DONE_TOOL => {
            openwave_core::validate_sandbox_done_arguments(&arguments)
                && serde_json::from_value::<openwave_core::SandboxDoneArgs>(arguments.clone())
                    .is_ok()
        }
        openwave_core::REQUEST_FOLDER_ACCESS_TOOL => {
            serde_json::from_value::<RequestFolderAccessArgs>(arguments.clone())
                .is_ok_and(|request| request.is_well_formed())
        }
        // The web-search lane validates its own arguments when it claims the
        // call; parsing is all this step can check.
        _ => true,
    };
    if !valid {
        return Ok(invalid(arguments));
    }
    Ok(SandboxToolCallIntent {
        provider_id,
        name,
        arguments,
        disposition: SandboxToolCallDisposition::Execute,
    })
}

pub(super) async fn complete_sandbox_task(
    provider: Arc<dyn ModelProvider>,
    request: ChatRequest,
) -> Result<SandboxStep> {
    let advertised_tools: Vec<String> =
        request.tools.iter().map(|tool| tool.name.clone()).collect();
    let mut stream = provider.stream(request).await?;
    let mut text = String::new();
    let mut calls = std::collections::BTreeMap::<u32, (String, String, String)>::new();
    let mut provider_executed = Vec::<ProviderExecutedCall>::new();
    while let Some(event) = stream.next().await {
        match event {
            ProviderEvent::TextDelta { text: delta } => {
                text.push_str(&delta);
                if text.chars().count() > AgentRun::MAX_RESULT_LEN {
                    return Err(AgentError::msg(format!(
                        "sandbox agent output exceeds {} characters",
                        AgentRun::MAX_RESULT_LEN
                    )));
                }
            }
            ProviderEvent::ToolCallStarted { index, id, name } => {
                if calls.insert(index, (id, name, String::new())).is_some() {
                    return Err(AgentError::msg(
                        "sandbox agent emitted duplicate tool-call index",
                    ));
                }
            }
            ProviderEvent::ToolCallArgsDelta { index, fragment } => {
                let Some((_, _, arguments)) = calls.get_mut(&index) else {
                    return Err(AgentError::msg(
                        "sandbox agent emitted tool arguments before its call",
                    ));
                };
                if arguments.len().saturating_add(fragment.len())
                    > ToolCallRecord::MAX_ARGUMENT_BYTES
                {
                    return Err(AgentError::msg(
                        "sandbox agent tool arguments exceed the durable checkpoint limit",
                    ));
                }
                arguments.push_str(&fragment);
            }
            ProviderEvent::Stop { reason } => {
                if matches!(reason, StopReason::Cancelled) {
                    return Err(AgentError::msg(
                        "sandbox agent did not produce a final result",
                    ));
                }
                if matches!(reason, StopReason::Refusal) {
                    return Err(AgentError::Refusal(
                        "sandbox agent model declined the request (category: unspecified)".into(),
                    ));
                }
                if reason == StopReason::ToolUse {
                    if calls.is_empty() {
                        return Err(AgentError::msg(
                            "sandbox agent stopped for tool use without a tool call",
                        ));
                    }
                    // The map is keyed by the provider's own call index, so
                    // draining it in order is the order the model emitted them
                    // — which becomes the batch's durable order.
                    let mut intents = Vec::with_capacity(calls.len());
                    for (_, (provider_id, name, arguments)) in calls {
                        intents.push(classify_sandbox_tool_call(
                            provider_id,
                            name,
                            &arguments,
                            &advertised_tools,
                        )?);
                    }
                    // These two end the run in place rather than parking work,
                    // so they are consumed here and their narration is dropped
                    // — the result they carry already speaks for the run. That
                    // only works when the step asked for nothing else: a run
                    // cannot both finish and still have work outstanding.
                    if intents.len() == 1
                        && matches!(intents[0].disposition, SandboxToolCallDisposition::Execute)
                    {
                        let intent = intents.remove(0);
                        if intent.name == openwave_core::SANDBOX_DONE_TOOL {
                            let arguments =
                                serde_json::from_value::<openwave_core::SandboxDoneArgs>(
                                    intent.arguments.clone(),
                                )
                                .map_err(|_| {
                                    AgentError::msg("sandbox agent emitted invalid done arguments")
                                })?;
                            return Ok(SandboxStep {
                                narration: String::new(),
                                completion: SandboxCompletion::Done {
                                    provider_id: intent.provider_id,
                                    arguments: intent.arguments,
                                    outputs: arguments.outputs,
                                    summary: arguments.summary,
                                },
                                provider_executed,
                            });
                        }
                        if intent.name == openwave_core::REQUEST_FOLDER_ACCESS_TOOL {
                            let request =
                                serde_json::from_value::<RequestFolderAccessArgs>(intent.arguments)
                                    .map_err(|_| {
                                        AgentError::msg(
                                            "sandbox agent emitted invalid folder-access proposal",
                                        )
                                    })?;
                            return Ok(SandboxStep {
                                narration: String::new(),
                                completion: SandboxCompletion::FolderAccessProposal { request },
                                provider_executed,
                            });
                        }
                        intents.push(intent);
                    } else {
                        // A terminal tool that arrived with company is answered
                        // rather than obeyed. Its siblings are real work the
                        // model asked for and still get run; only the attempt to
                        // finish in the same breath is refused.
                        for intent in &mut intents {
                            if !matches!(intent.disposition, SandboxToolCallDisposition::Execute) {
                                continue;
                            }
                            let terminal = intent.name == openwave_core::SANDBOX_DONE_TOOL
                                || intent.name == openwave_core::REQUEST_FOLDER_ACCESS_TOOL;
                            if terminal {
                                let name = &intent.name;
                                let message = format!(
                                    "{name} must be the only tool call in a step. The other calls \
                                     in this step were run; call {name} alone once you have their \
                                     results."
                                );
                                intent.disposition = SandboxToolCallDisposition::Rejected {
                                    error_code: "must_be_alone",
                                    message,
                                };
                            }
                        }
                    }
                    return Ok(SandboxStep {
                        narration: text,
                        completion: SandboxCompletion::ToolCalls(intents),
                        provider_executed,
                    });
                }
                if !calls.is_empty() {
                    return Err(AgentError::msg(
                        "sandbox agent stopped with an incomplete tool call",
                    ));
                }
                if text.trim().is_empty() {
                    return Err(AgentError::msg(
                        "sandbox agent produced an empty final result",
                    ));
                }
                return Ok(SandboxStep {
                    narration: String::new(),
                    completion: SandboxCompletion::Final(text),
                    provider_executed,
                });
            }
            ProviderEvent::Refusal { details } => {
                let category = details
                    .category()
                    .map_or_else(|| "unspecified".to_owned(), str::to_owned);
                return Err(AgentError::Refusal(format!(
                    "sandbox agent model declined the request (category: {category})"
                )));
            }
            // A search the provider already ran. There is nothing to dispatch
            // and nothing to answer — the results are inside the completion
            // being written — so the step records it and keeps reading. It is
            // not a tool call this loop can consume, so it never competes with
            // the one checkpoint a step may park.
            ProviderEvent::ProviderExecutedToolCall {
                name,
                input,
                is_error,
                ..
            } => {
                if provider_executed.len() < MAX_PROVIDER_EXECUTED_RECORDS {
                    provider_executed.push(ProviderExecutedCall {
                        name,
                        query: input
                            .get("query")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                        is_error,
                    });
                }
            }
            // Reasoning blocks exist for in-turn replay, which this minimal
            // loop does not do; dropping them degrades to pre-replay behavior.
            ProviderEvent::ReasoningDelta { .. }
            | ProviderEvent::ReasoningBlock { .. }
            | ProviderEvent::Usage(_) => {}
            // The stream broke mid-flight, so `text` and `arguments` are both
            // possibly truncated. Fail under the classified provider error
            // instead of treating the fragment as a result.
            ProviderEvent::Failed { error } => return Err(error.into_agent_error()),
            _ => {
                return Err(AgentError::msg(
                    "sandbox agent provider emitted an unsupported event",
                ))
            }
        }
    }
    Err(AgentError::msg(
        "sandbox agent provider stream ended without a stop event",
    ))
}

pub(super) fn chrono_duration(duration: Duration) -> Result<chrono::Duration> {
    chrono::Duration::from_std(duration)
        .map_err(|error| AgentError::msg(format!("invalid sandbox-worker duration: {error}")))
}
