//! The sandbox-resident tool registry.
//!
//! Running the agent loop inside a sandbox is "the same code with a different
//! tool registry and transport" (see
//! [sandbox-providers.md](../../docs/sandbox-providers.md)). This module builds
//! that registry from OpenWave's own [`Tool`] trait, so the tools the sandbox
//! loop invokes are ordinary [`openwave_core`] tools — not a parallel
//! abstraction.
//!
//! The set is deliberately **closed and minimal** for this slice: model
//! inference is dialed back to the host over reverse RPC (the first host-mediated
//! capability), and the only sandbox-local tool is a trivial, dependency-free,
//! read-only computation that demonstrates the loop actually invoking a tool.
//! Widening the sandbox-resident registry is a separate design, gated on the
//! entry conditions the delivery sequence names; nothing here reaches the network
//! or the filesystem, so no credential or egress boundary is engaged yet.

use async_trait::async_trait;
use openwave_core::{ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolSpec};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The name the model uses to invoke the word-count tool.
pub const WORD_COUNT_TOOL: &str = "word_count";

/// Arguments for [`WordCount`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WordCountArgs {
    /// The text whose words to count.
    text: String,
}

/// A trivial, read-only local tool: count the whitespace-separated words in a
/// piece of text.
///
/// It exists to prove the sandbox loop can invoke a real [`Tool`] locally, not to
/// be useful. It touches no filesystem, network, or credential, so it needs no
/// approval beyond the auto-approving [`ApprovalClass::ReadOnly`] class.
pub struct WordCount;

#[async_trait]
impl Tool for WordCount {
    fn spec(&self) -> ToolSpec {
        ToolSpec::for_args::<WordCountArgs>(
            WORD_COUNT_TOOL,
            "Count the whitespace-separated words in a piece of text.",
        )
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        // Arguments are untrusted input; a malformed call is a tool failure the
        // model sees and can correct, not a process error.
        let args: WordCountArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(error) => return Ok(ToolOutput::error(format!("invalid arguments: {error}"))),
        };
        let count = args.text.split_whitespace().count();
        Ok(ToolOutput::text(count.to_string()))
    }
}

/// Build the closed sandbox-resident tool registry for this slice.
#[must_use]
pub fn sandbox_tool_registry() -> openwave_core::ToolRegistry {
    let mut registry = openwave_core::ToolRegistry::new();
    registry.register(Box::new(WordCount));
    registry
}
