//! The in-container agent loop.
//!
//! This is the sandbox-resident consumer of the transport: on attach it drives a
//! bounded agent loop that reuses OpenWave's [`Tool`](openwave_core::Tool)
//! registry, dials each model step back to the host over reverse RPC, emits the
//! event stream as it goes, and submits a final result.
//!
//! # Scope: the loop's shape, not the host-side `Agent`
//!
//! OpenWave's full host-side [`Agent`](openwave_core::Agent) drives its turn from
//! a durable [`Store`](openwave_core::storage) — messages, turns, checkpoints,
//! approval brokering — which is host-side persistence machinery, not something
//! the container stands up. This slice therefore reuses the *plain-Rust seams*
//! the design says the sandbox loop shares — the `Tool` trait and registry, the
//! `ToolCtx`/`ToolOutput` vocabulary, and host-proxied model inference — and runs
//! a minimal loop over them to demonstrate the sandbox-resident path end to end.
//! Swapping this minimal driver for `Agent::run_turn` behind a sandbox-resident
//! `Store` is the productionization step, and it plugs in exactly here.
//!
//! # The demo tool protocol
//!
//! To keep the loop model-driven without a full tool-calling transcript, a model
//! completion is read as a directive: a completion of the form
//! `use-tool:<name>:<json-args>` runs that tool locally and feeds its result
//! back for the next step; any other completion is the final answer. The host
//! (or, in tests, a mock host model) is what decides the directive, so the loop
//! is genuinely driven from the model over the reverse channel.

use openwave_core::{ChatId, ToolCtx, ToolRegistry};
use openwave_sandbox_protocol::{ids::OperationId, SandboxRun};

use crate::model::{HostModel, ModelError};
use crate::tools::sandbox_tool_registry;

/// The prefix a model completion uses to request a local tool call.
const TOOL_DIRECTIVE: &str = "use-tool:";

/// Bound on model steps, so a model that never finishes cannot loop forever.
const MAX_STEPS: usize = 8;

/// Why an in-container agent run did not complete.
#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    /// A model step failed (the host refused it, or the connection dropped).
    #[error("model step failed: {0}")]
    Model(#[from] ModelError),
    /// The loop hit its step bound without the model submitting a final answer.
    #[error("the agent loop exceeded {MAX_STEPS} model steps without a result")]
    StepLimit,
}

/// Run the sandbox-resident agent loop for one `task`, driving it through `run`.
///
/// Emits progress events as it works and submits the final answer as the run's
/// terminal [`Result`](openwave_sandbox_protocol::events::EventPayload::Result)
/// event. Returns the final answer text.
///
/// # Errors
/// [`AgentRunError::Model`] if a model step fails, or [`AgentRunError::StepLimit`]
/// if the model never submits a final answer within the step bound.
pub async fn run_agent(run: SandboxRun, task: impl Into<String>) -> Result<String, AgentRunError> {
    let outcome = run_loop(run.clone(), task.into()).await;
    // Every exit from the loop must put a terminal event on the stream. The
    // supervisor keeps serving the connection after this returns, so a host that
    // saw neither a result nor a failure would wait on an open socket forever
    // and leak the sandbox. `run_loop` emits the result on the success path; a
    // failure is signalled here, once, on every other path.
    if let Err(error) = &outcome {
        let _ = run.emit_failed(error.to_string()).await;
    }
    outcome
}

async fn run_loop(run: SandboxRun, task: String) -> Result<String, AgentRunError> {
    let tools = sandbox_tool_registry();
    let model = HostModel::new(run.clone());
    // The sandbox loop has no host chat identity or filesystem scratch; a fresh
    // chat id and no private scratch keep the tool context self-contained.
    let ctx = ToolCtx::without_private_scratch(ChatId::new(), None);

    let _ = run
        .emit_progress("sandbox agent attached; starting run")
        .await;

    let mut transcript = format!("Task: {task}\n");
    for _step in 0..MAX_STEPS {
        // Each model step is its own durable operation, so a re-issue after a
        // reconnect is answered from the host's recorded outcome.
        let completion = model
            .complete(OperationId::new(), transcript.clone())
            .await?;

        let Some(directive) = parse_tool_directive(&completion) else {
            // Any non-directive completion is the final answer.
            let _ = run.emit_result(completion.clone()).await;
            return Ok(completion);
        };

        let _ = run
            .emit_progress(format!("calling tool {}", directive.name))
            .await;
        let output = run_tool(&tools, &ctx, directive.name, directive.args).await;
        let _ = run
            .emit_progress(format!("tool {} -> {}", directive.name, output))
            .await;
        transcript.push_str(&format!("Tool {} result: {output}\n", directive.name));
    }

    Err(AgentRunError::StepLimit)
}

/// A parsed `use-tool:<name>:<json-args>` directive.
struct ToolDirective<'a> {
    name: &'a str,
    args: &'a str,
}

/// Read a completion as a tool directive, or `None` if it is a final answer.
fn parse_tool_directive(completion: &str) -> Option<ToolDirective<'_>> {
    let rest = completion.strip_prefix(TOOL_DIRECTIVE)?;
    let (name, args) = rest.split_once(':')?;
    (!name.is_empty()).then_some(ToolDirective { name, args })
}

/// Execute one local tool by name, returning its model-readable result text.
///
/// A missing tool or a malformed argument string is reported back to the model
/// as text rather than aborting the run — the loop treats them the way the model
/// would treat any tool failure.
async fn run_tool(tools: &ToolRegistry, ctx: &ToolCtx, name: &str, args: &str) -> String {
    let Some(tool) = tools.get(name) else {
        return format!("error: no tool named {name}");
    };
    let args = match serde_json::from_str(args) {
        Ok(args) => args,
        Err(error) => return format!("error: tool arguments were not valid JSON: {error}"),
    };
    match tool.execute(ctx, args).await {
        Ok(output) => output.content,
        Err(error) => format!("error: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_tool_directive_and_ignores_a_final_answer() {
        let directive = parse_tool_directive("use-tool:word_count:{\"text\":\"a b\"}").unwrap();
        assert_eq!(directive.name, "word_count");
        assert_eq!(directive.args, "{\"text\":\"a b\"}");

        assert!(parse_tool_directive("the final answer is 2").is_none());
        // An empty tool name is not a directive.
        assert!(parse_tool_directive("use-tool::{}").is_none());
    }
}
