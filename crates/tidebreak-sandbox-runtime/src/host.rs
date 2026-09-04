//! Host-owned model, setting, catalog, and event seams.

use std::sync::Arc;

use async_trait::async_trait;
use tidebreak_code_execution::{PluginPackage, SkillPackage};
use tidebreak_core::{
    AgentConfig, Chat, ModelProvider, ReasoningEffort, Result, SequencedAgentEvent, SessionId,
    TurnWebSearch,
};

/// One model selection resolved through the embedding host's policy.
pub struct ResolvedSandboxModel {
    pub config: AgentConfig,
    pub supports_vendor_web_search: bool,
    pub supports_search_subrequest: bool,
}

/// The sandbox surface that is resolving a model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxModelUse {
    /// An in-process background agent run.
    InProcess,
    /// A container-resident background agent run.
    Container,
}

/// Supplies the server-owned facts that sandbox execution consumes.
#[async_trait]
pub trait SandboxHost: Send + Sync {
    /// Resolve the provider that executes the next model request.
    async fn resolve_provider(&self) -> Arc<dyn ModelProvider>;

    /// Resolve one durable model selection through host policy.
    async fn resolve_model(
        &self,
        use_case: SandboxModelUse,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
        base: AgentConfig,
    ) -> Result<ResolvedSandboxModel>;

    /// Resolve the model inherited by a legacy run without a frozen selection.
    async fn resolve_chat_model(&self, chat: &Chat, boot_default: &str) -> Result<String> {
        Ok(chat
            .model
            .clone()
            .unwrap_or_else(|| boot_default.to_owned()))
    }

    /// Return the live check-in step override, if one is stored.
    async fn checkin_steps_override(&self) -> Result<Option<u32>> {
        Ok(None)
    }

    /// Return the consecutive tool-error count that triggers a check-in.
    async fn error_checkin_threshold(&self) -> Result<u32> {
        Ok(tidebreak_core::DEFAULT_SANDBOX_AGENT_ERROR_CHECKIN as u32)
    }

    /// Resolve the host's search choice for the selected model.
    async fn resolve_web_search(
        &self,
        supports_vendor: bool,
        supports_subrequest: bool,
    ) -> Result<TurnWebSearch> {
        let _ = (supports_vendor, supports_subrequest);
        Ok(TurnWebSearch::Off)
    }

    /// Read the enabled skill catalog exposed to a tool-capable run.
    async fn skill_catalog(&self) -> Vec<SkillPackage> {
        Vec::new()
    }

    /// Read the enabled plugin catalog exposed to a tool-capable run.
    async fn plugin_catalog(&self) -> Vec<PluginPackage> {
        Vec::new()
    }

    /// Publish one exact journaled event to live subscribers.
    fn publish_event(&self, session_id: SessionId, event: SequencedAgentEvent) {
        let _ = (session_id, event);
    }
}
