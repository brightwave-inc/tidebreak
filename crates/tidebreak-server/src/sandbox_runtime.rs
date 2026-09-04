//! Server adapters for the extracted sandbox runtime.

use std::sync::Arc;

use async_trait::async_trait;
use tidebreak_code_execution::{PluginPackage, SkillPackage};
use tidebreak_core::{
    AgentConfig, Chat, ModelProvider, ReasoningEffort, Result, SecretProvider, SequencedAgentEvent,
    SessionId, Store, TurnWebSearch,
};
use tidebreak_sandbox_runtime::{ResolvedSandboxModel, SandboxHost, SandboxModelUse};

use crate::bus::EventBus;
use crate::code_execution::ConfiguredExecProvider;
use crate::resolver::ProviderResolver;

pub struct ServerSandboxHost {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    resolver: Arc<dyn ProviderResolver>,
    events: Arc<EventBus>,
    code_execution: Option<Arc<ConfiguredExecProvider>>,
}

impl ServerSandboxHost {
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        resolver: Arc<dyn ProviderResolver>,
        events: Arc<EventBus>,
        code_execution: Option<Arc<ConfiguredExecProvider>>,
    ) -> Self {
        Self {
            store,
            secrets,
            resolver,
            events,
            code_execution,
        }
    }
}

#[async_trait]
impl SandboxHost for ServerSandboxHost {
    async fn resolve_provider(&self) -> Arc<dyn ModelProvider> {
        self.resolver.resolve().await
    }

    async fn resolve_model(
        &self,
        use_case: SandboxModelUse,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
        mut base: AgentConfig,
    ) -> Result<ResolvedSandboxModel> {
        let capabilities = if self.resolver.enforces_model_registry() {
            let Some(policy) =
                crate::providers::resolve_model_policy(&*self.store, &model, true, None).await?
            else {
                let message = match use_case {
                    SandboxModelUse::InProcess => {
                        "sandbox model is not registered for its provider"
                    }
                    SandboxModelUse::Container => {
                        "container sandbox model is not registered for its provider"
                    }
                };
                return Err(tidebreak_core::AgentError::config(message));
            };
            if !crate::providers::is_valid_execution_policy(&policy) {
                return Err(tidebreak_core::AgentError::config(
                    "managed gateway execution requires a frozen model identity",
                ));
            }
            let capabilities = (
                policy.supports_vendor_web_search,
                policy.supports_search_subrequest,
            );
            crate::providers::apply_model_policy(&mut base, &policy, reasoning_effort)?;
            capabilities
        } else {
            crate::providers::apply_free_form_model(&mut base, model, reasoning_effort)?;
            (false, false)
        };
        Ok(ResolvedSandboxModel {
            config: base,
            supports_vendor_web_search: capabilities.0,
            supports_search_subrequest: capabilities.1,
        })
    }

    async fn resolve_chat_model(&self, chat: &Chat, boot_default: &str) -> Result<String> {
        crate::runtime_settings::resolve_chat_model(&*self.store, chat, boot_default).await
    }

    async fn checkin_steps_override(&self) -> Result<Option<u32>> {
        crate::runtime_settings::read_sandbox_agent_checkin_steps_override(&*self.store).await
    }

    async fn error_checkin_threshold(&self) -> Result<u32> {
        crate::runtime_settings::read_sandbox_agent_error_checkin(&*self.store).await
    }

    async fn resolve_web_search(
        &self,
        supports_vendor: bool,
        supports_subrequest: bool,
    ) -> Result<TurnWebSearch> {
        crate::web_search::resolve_turn_web_search(
            &*self.store,
            &*self.secrets,
            supports_vendor,
            supports_subrequest,
        )
        .await
    }

    async fn skill_catalog(&self) -> Vec<SkillPackage> {
        match &self.code_execution {
            Some(provider) => provider.skill_catalog().await,
            None => Vec::new(),
        }
    }

    async fn plugin_catalog(&self) -> Vec<PluginPackage> {
        match &self.code_execution {
            Some(provider) => provider.plugin_catalog().await,
            None => Vec::new(),
        }
    }

    fn publish_event(&self, session_id: SessionId, event: SequencedAgentEvent) {
        let _ = self.events.sender(session_id).send(event);
    }
}
