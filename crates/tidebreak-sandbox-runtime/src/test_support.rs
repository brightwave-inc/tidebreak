use std::sync::Arc;

use async_trait::async_trait;
use tidebreak_core::{
    AgentConfig, ModelProvider, ReasoningEffort, Result, SecretProvider, Store, TurnWebSearch,
    VendorWebSearch,
};

use crate::{ResolvedSandboxModel, SandboxHost, SandboxModelUse};

pub(crate) mod resolver {
    use std::sync::Arc;

    use async_trait::async_trait;
    use tidebreak_core::ModelProvider;

    #[async_trait]
    pub(crate) trait ProviderResolver: Send + Sync {
        async fn resolve(&self) -> Arc<dyn ModelProvider>;

        fn enforces_model_registry(&self) -> bool {
            false
        }
    }
}

pub(crate) mod bus {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use tidebreak_core::{SequencedAgentEvent, SessionId};
    use tokio::sync::broadcast;

    const LIVE_BUFFER: usize = 256;

    #[derive(Default)]
    pub(crate) struct EventBus {
        channels: Mutex<HashMap<SessionId, broadcast::Sender<SequencedAgentEvent>>>,
    }

    impl EventBus {
        fn channel(&self, chat: SessionId) -> broadcast::Sender<SequencedAgentEvent> {
            self.channels
                .lock()
                .expect("test event bus lock")
                .entry(chat)
                .or_insert_with(|| broadcast::channel(LIVE_BUFFER).0)
                .clone()
        }

        pub(crate) fn publish(&self, chat: SessionId, event: SequencedAgentEvent) {
            let _ = self.channel(chat).send(event);
        }

        pub(crate) fn subscribe(
            &self,
            chat: SessionId,
        ) -> broadcast::Receiver<SequencedAgentEvent> {
            self.channel(chat).subscribe()
        }
    }
}

pub(crate) struct TestSandboxHost {
    store: Arc<dyn Store>,
    resolver: Arc<dyn resolver::ProviderResolver>,
    events: Arc<bus::EventBus>,
}

impl TestSandboxHost {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        _secrets: Arc<dyn SecretProvider>,
        resolver: Arc<dyn resolver::ProviderResolver>,
        events: Arc<bus::EventBus>,
    ) -> Self {
        Self {
            store,
            resolver,
            events,
        }
    }
}

#[async_trait]
impl SandboxHost for TestSandboxHost {
    async fn resolve_provider(&self) -> Arc<dyn ModelProvider> {
        self.resolver.resolve().await
    }

    async fn resolve_model(
        &self,
        _use_case: SandboxModelUse,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
        mut base: AgentConfig,
    ) -> Result<ResolvedSandboxModel> {
        let registered = self.resolver.enforces_model_registry();
        if registered {
            if let Some((provider, raw_model)) = model.split_once("::") {
                base.provider = Some(tidebreak_core::ProviderId::new(provider));
                base.model = raw_model.to_owned();
            } else {
                base.model = model;
            }
            base.reasoning_model = true;
        } else {
            base.model = model;
        }
        base.reasoning_effort = reasoning_effort;
        Ok(ResolvedSandboxModel {
            config: base,
            supports_vendor_web_search: registered,
            supports_search_subrequest: false,
        })
    }

    async fn resolve_web_search(
        &self,
        supports_vendor: bool,
        supports_subrequest: bool,
    ) -> Result<TurnWebSearch> {
        let mode = self
            .store
            .get_setting("web_search")
            .await?
            .and_then(|value| {
                value
                    .get("mode")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "automatic".to_owned());
        Ok(match mode.as_str() {
            "off" => TurnWebSearch::Off,
            "host" => TurnWebSearch::Host,
            "vendor" if supports_vendor => TurnWebSearch::Vendor(VendorWebSearch {
                max_uses: VendorWebSearch::DEFAULT_MAX_USES,
            }),
            "vendor" if supports_subrequest => TurnWebSearch::Host,
            "vendor" => TurnWebSearch::Off,
            _ if supports_vendor => TurnWebSearch::Vendor(VendorWebSearch {
                max_uses: VendorWebSearch::DEFAULT_MAX_USES,
            }),
            _ => TurnWebSearch::Host,
        })
    }

    fn publish_event(
        &self,
        session_id: tidebreak_core::SessionId,
        event: tidebreak_core::SequencedAgentEvent,
    ) {
        self.events.publish(session_id, event);
    }
}

#[async_trait]
impl<T> SandboxHost for T
where
    T: resolver::ProviderResolver + Send + Sync,
{
    async fn resolve_provider(&self) -> Arc<dyn ModelProvider> {
        self.resolve().await
    }

    async fn resolve_model(
        &self,
        _use_case: SandboxModelUse,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
        mut base: AgentConfig,
    ) -> Result<ResolvedSandboxModel> {
        if self.enforces_model_registry() {
            if let Some((provider, raw_model)) = model.split_once("::") {
                base.provider = Some(tidebreak_core::ProviderId::new(provider));
                base.model = raw_model.to_owned();
            } else {
                base.model = model;
            }
            base.reasoning_model = true;
        } else {
            base.model = model;
        }
        base.reasoning_effort = reasoning_effort;
        Ok(ResolvedSandboxModel {
            config: base,
            supports_vendor_web_search: self.enforces_model_registry(),
            supports_search_subrequest: false,
        })
    }
}
