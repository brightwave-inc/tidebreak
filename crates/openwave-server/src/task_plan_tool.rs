//! The foreground agent's durable task plan tool.
//!
//! Unlike the other foreground-only tools, this one neither parks the turn nor
//! asks anyone for anything: it validates, writes the chat's plan row, and
//! returns. It needs the store, so it lives here rather than in the core tool
//! module — the execution context carries the conversation and the call, not a
//! store handle.

use std::sync::Arc;

use async_trait::async_trait;
use openwave_core::{
    parse_update_task_plan_arguments, task_plan_summary, update_task_plan_tool_spec, ApprovalClass,
    Result, Store, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec,
};
use serde_json::Value;

/// Replace the conversation's task plan.
pub(crate) struct UpdateTaskPlanTool {
    store: Arc<dyn Store>,
}

impl UpdateTaskPlanTool {
    pub(crate) fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for UpdateTaskPlanTool {
    fn spec(&self) -> ToolSpec {
        update_task_plan_tool_spec()
    }

    /// The plan is the agent's own display state. It touches no file, no host
    /// path, and nothing outside the conversation it already belongs to, so
    /// there is nothing here for a reader to consent to — and gating it would
    /// make keeping the user informed the most interrupting thing the agent
    /// does.
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
        let plan = self
            .store
            .update_task_plan(ctx.chat_id, call_id, &arguments.steps, chrono::Utc::now())
            .await?;
        Ok(ToolOutput::text(task_plan_summary(&plan.steps)))
    }
}
