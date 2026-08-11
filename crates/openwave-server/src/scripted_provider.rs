//! A model provider that replays a script, for driving the real engine from a
//! test in another crate.
//!
//! Every stub provider in this crate is private to a `#[cfg(test)]` module, so
//! nothing outside can run a turn end to end: `openwave-cli`'s process-level
//! tests spawn the `openwave` binary, and no in-process handle reaches into it.
//! This module is the one seam that does, and it stays deliberately small — a
//! list of steps in, provider events out.
//!
//! It is compiled only under the `scripted-provider` feature, which
//! `openwave-cli` enables from its dev-dependencies. A released binary never
//! contains it, so the environment variable below cannot divert a real
//! installation's model traffic.
//!
//! `OPENWAVE_SCRIPTED_PROVIDER` holds the script as a JSON array, one entry per
//! model step:
//!
//! ```json
//! [{"tool": "update_task_plan", "input": {"steps": []}}, {"text": "done"}]
//! ```
//!
//! A `tool` step streams that call and stops with `tool_use`, so the host runs
//! the tool for real and comes back for the next step. A `text` step streams
//! the text and ends the turn. Steps past the end of the script end the turn
//! too, so a script that mispredicts how many completions a turn needs cannot
//! loop forever.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt as _};

use openwave_core::{
    AgentError, ChatRequest, ModelProvider, ProviderEvent, ProviderId, Result, StopReason,
};

use crate::resolver::ProviderResolver;

/// The environment variable carrying the script.
const SCRIPT_VAR: &str = "OPENWAVE_SCRIPTED_PROVIDER";

/// One model step.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum Step {
    /// Call `tool` with `input`, then wait for its result.
    Tool {
        tool: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    /// Answer with `text` and end the turn.
    Text { text: String },
}

/// Replays [`Step`]s, one per completion.
struct ScriptedProvider {
    steps: Vec<Step>,
    calls: AtomicUsize,
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("scripted")
    }

    async fn stream(&self, _request: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let step = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = match self.steps.get(step) {
            Some(Step::Tool { tool, input }) => vec![
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: format!("scripted_{step}"),
                    name: tool.clone(),
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: input.to_string(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse,
                },
            ],
            Some(Step::Text { text }) => vec![
                ProviderEvent::TextDelta { text: text.clone() },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
            None => vec![
                ProviderEvent::TextDelta {
                    text: "the script ended".to_owned(),
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ],
        };
        Ok(stream::iter(events).boxed())
    }
}

/// Hands the scripted provider to every turn.
struct ScriptedResolver(Arc<ScriptedProvider>);

#[async_trait]
impl ProviderResolver for ScriptedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.0.clone()
    }
}

/// The resolver [`SCRIPT_VAR`] asks for, or `None` when it is unset.
///
/// A malformed script is an error rather than a silent fall-through to
/// configured routing: a test whose script did not parse would otherwise run
/// against whatever credentials the host happens to have.
pub(crate) fn resolver_from_env() -> Result<Option<Arc<dyn ProviderResolver>>> {
    let Ok(script) = std::env::var(SCRIPT_VAR) else {
        return Ok(None);
    };
    let steps: Vec<Step> = serde_json::from_str(&script).map_err(|error| {
        AgentError::config(format!("{SCRIPT_VAR} is not a valid script: {error}"))
    })?;
    Ok(Some(Arc::new(ScriptedResolver(Arc::new(
        ScriptedProvider {
            steps,
            calls: AtomicUsize::new(0),
        },
    )))))
}
