//! The foreground agent's durable task plan tool.
//!
//! Unlike the other foreground-only tools, this one neither parks the turn nor
//! asks anyone for anything: it validates, writes the chat's plan row, and
//! returns. It needs the store, so it lives here rather than in the core tool
//! module — the execution context carries the conversation and the call, not a
//! store handle.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tidebreak_core::{
    parse_update_task_plan_arguments, task_plan_summary, update_task_plan_tool_spec, ApprovalClass,
    Result, Store, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec,
};

/// Replace the conversation's task plan.
pub struct UpdateTaskPlanTool {
    store: Arc<dyn Store>,
}

impl UpdateTaskPlanTool {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for UpdateTaskPlanTool {
    fn spec(&self) -> ToolSpec {
        update_task_plan_tool_spec()
    }

    /// `ReadOnly` even though the call writes a row.
    ///
    /// The class governs consent, and the question it answers is what a call
    /// can reach: this one reaches the conversation's own display state and
    /// nothing else — no file, no host path, no network, no state another
    /// conversation or the host can observe. There is nothing here for a
    /// reader to decide, and gating it would make keeping the user informed
    /// the most interrupting thing the agent does. `Workspace` would be a
    /// worse fit rather than a more honest one: it means the call changed
    /// something the user owns and may want undone, which a checklist the
    /// agent keeps about its own progress is not.
    ///
    /// The class doubles as the plan-mode visibility filter, and that reading
    /// would be wrong here — a turn drafting a proposal must not also be
    /// committing plan rows. The registry excludes this tool from the
    /// plan-mode surface by name for exactly that reason.
    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let arguments = match parse_update_task_plan_arguments(&args) {
            Ok(arguments) => arguments,
            Err(correction) => {
                return Ok(ToolOutput::failed(
                    ToolErrorCategory::InvalidArguments,
                    correction,
                ));
            }
        };
        // The plan is scoped to the turn that wrote it, which only an agent
        // turn has. A direct or MCP invocation has no call to attribute it to.
        let Some(call_id) = ctx.call_id else {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::ToolFailed,
                "a task plan can only be recorded from a conversation turn",
            ));
        };
        // A store failure here is one unrecorded checklist, not a reason to
        // lose the work the turn is doing. It comes back as a tool failure the
        // model can read and carry on from, like the correction above.
        match self
            .store
            .update_task_plan(ctx.chat_id, call_id, &arguments.steps, chrono::Utc::now())
            .await
        {
            Ok(Some(plan)) => Ok(ToolOutput::text(task_plan_summary(&plan.steps))),
            // The attempt that made this call no longer owns the turn, so its
            // plan is not the one anybody should see. Nothing went wrong and
            // nothing should fail; say so plainly and let the turn end.
            Ok(None) => Ok(ToolOutput::text(
                "Plan not recorded: a newer attempt owns this turn.",
            )),
            Err(error) => Ok(ToolOutput::failed(
                ToolErrorCategory::ToolFailed,
                format!("the task plan could not be recorded: {error}"),
            )),
        }
    }
}
