//! Desktop implementation of the server-owned browser runtime boundary.
//!
//! The HTTP channel authenticates a code session and passes its exact
//! `{owner, workspace, session}` scope here. This adapter maps that scope onto
//! a short-lived native [`BrowserRegistry`] capability. Revocation leaves an
//! enduring tombstone: an ended session id can never lazily mint a replacement
//! capability on a later request.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use tauri::AppHandle;
use tidebreak_core::{
    BrowserListResult, BrowserNavigateArgs, BrowserNavigateResult, BrowserPageSnapshot,
    BrowserSnapshotArgs, CodeSessionId, OwnerId, WorkspaceId,
};
use tidebreak_server::{BrowserRuntime, BrowserRuntimeError, BrowserRuntimeScope};
use uuid::Uuid;

use crate::browser_control::BrowserRegistry;

#[derive(Clone)]
pub(crate) struct DesktopBrowserRuntime {
    app: AppHandle,
    registry: BrowserRegistry,
    sessions: SessionCapabilities,
}

impl DesktopBrowserRuntime {
    pub(crate) fn new(app: AppHandle, registry: BrowserRegistry) -> Self {
        Self {
            app,
            registry,
            sessions: SessionCapabilities::default(),
        }
    }
}

#[async_trait]
impl BrowserRuntime for DesktopBrowserRuntime {
    async fn list(
        &self,
        scope: &BrowserRuntimeScope,
    ) -> Result<BrowserListResult, BrowserRuntimeError> {
        let capability_id = self.sessions.capability_for(&self.registry, scope)?;
        self.registry
            .list_for_capability(capability_id)
            .map(|sessions| BrowserListResult { sessions })
            .map_err(|error| map_native_error(None, error))
    }

    async fn navigate(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserNavigateArgs,
    ) -> Result<BrowserNavigateResult, BrowserRuntimeError> {
        let capability_id = self.sessions.capability_for(&self.registry, scope)?;
        crate::code_browser::navigate_browser_for_agent(
            &self.app,
            &self.registry,
            capability_id,
            args,
        )
        .await
        .map_err(|error| map_native_error(Some(&args.browser_id), error))
    }

    async fn snapshot(
        &self,
        scope: &BrowserRuntimeScope,
        args: &BrowserSnapshotArgs,
    ) -> Result<BrowserPageSnapshot, BrowserRuntimeError> {
        let capability_id = self.sessions.capability_for(&self.registry, scope)?;
        crate::browser_semantics::browser_semantic_snapshot(
            &self.app,
            &self.registry,
            capability_id,
            args.clone(),
        )
        .await
        .map_err(|error| map_native_error(Some(&args.browser_id), error))
    }

    fn revoke_session(&self, scope: &BrowserRuntimeScope) {
        self.sessions.revoke(&self.registry, scope);
    }
}

#[derive(Clone, Default)]
struct SessionCapabilities {
    states: std::sync::Arc<Mutex<HashMap<CodeSessionId, SessionCapabilityState>>>,
}

enum SessionCapabilityState {
    Active(ActiveSessionCapability),
    Revoked,
}

struct ActiveSessionCapability {
    owner: OwnerId,
    workspace: WorkspaceId,
    capability_id: Uuid,
}

impl SessionCapabilities {
    fn capability_for(
        &self,
        registry: &BrowserRegistry,
        scope: &BrowserRuntimeScope,
    ) -> Result<Uuid, BrowserRuntimeError> {
        use std::collections::hash_map::Entry;

        let mut states = self.lock();
        match states.entry(scope.session) {
            Entry::Occupied(mut entry) => {
                if matches!(entry.get(), SessionCapabilityState::Revoked) {
                    return Err(BrowserRuntimeError::SessionEnded);
                }

                let scope_matches = match entry.get() {
                    SessionCapabilityState::Active(active) => {
                        active.owner == scope.owner && active.workspace == scope.workspace
                    }
                    SessionCapabilityState::Revoked => false,
                };
                if !scope_matches {
                    let previous = match std::mem::replace(
                        entry.get_mut(),
                        SessionCapabilityState::Revoked,
                    ) {
                        SessionCapabilityState::Active(active) => active.capability_id,
                        SessionCapabilityState::Revoked => unreachable!("checked above"),
                    };
                    registry.revoke_agent_capability(previous);
                    return Err(BrowserRuntimeError::SessionEnded);
                }

                let active = match entry.get_mut() {
                    SessionCapabilityState::Active(active) => active,
                    SessionCapabilityState::Revoked => unreachable!("checked above"),
                };
                let workspace = scope.workspace.to_string();
                if registry
                    .heartbeat_agent_capability(active.capability_id, &workspace)
                    .is_ok()
                {
                    return Ok(active.capability_id);
                }

                // A live code session may outlast the native capability's
                // short TTL. Rotate it under the session-state lock so a
                // concurrent revoke cannot resurrect the session.
                let previous = active.capability_id;
                let replacement = registry.issue_agent_capability(&workspace, "Code agent");
                active.capability_id = replacement;
                registry.revoke_agent_capability(previous);
                Ok(replacement)
            }
            Entry::Vacant(entry) => {
                let capability_id =
                    registry.issue_agent_capability(&scope.workspace.to_string(), "Code agent");
                entry.insert(SessionCapabilityState::Active(ActiveSessionCapability {
                    owner: scope.owner.clone(),
                    workspace: scope.workspace,
                    capability_id,
                }));
                Ok(capability_id)
            }
        }
    }

    fn revoke(&self, registry: &BrowserRegistry, scope: &BrowserRuntimeScope) {
        use std::collections::hash_map::Entry;

        let capability_id = {
            let mut states = self.lock();
            match states.entry(scope.session) {
                Entry::Occupied(mut entry) => {
                    match std::mem::replace(entry.get_mut(), SessionCapabilityState::Revoked) {
                        SessionCapabilityState::Active(active) => Some(active.capability_id),
                        SessionCapabilityState::Revoked => None,
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(SessionCapabilityState::Revoked);
                    None
                }
            }
        };
        if let Some(capability_id) = capability_id {
            registry.revoke_agent_capability(capability_id);
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<CodeSessionId, SessionCapabilityState>> {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn map_native_error(browser_id: Option<&str>, error: String) -> BrowserRuntimeError {
    if error.contains("browser capability is unavailable") {
        return BrowserRuntimeError::SessionEnded;
    }
    if let Some(browser_id) = browser_id {
        if error.contains("browser session is not registered")
            || error.contains("belongs to a different workspace")
            || error.contains("browser session is not open")
        {
            return BrowserRuntimeError::UnknownBrowserId(browser_id.to_owned());
        }
    }
    if error.contains("snapshot is stale")
        || error.contains("stale target")
        || error.contains("document changed")
    {
        return BrowserRuntimeError::StaleTarget;
    }
    if error.contains("not available on this platform")
        || error.contains("does not support semantic")
    {
        return BrowserRuntimeError::Unsupported("semantic snapshots".to_owned());
    }
    BrowserRuntimeError::Failed(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(session: CodeSessionId, workspace: WorkspaceId) -> BrowserRuntimeScope {
        BrowserRuntimeScope {
            owner: OwnerId::local(),
            workspace,
            session,
        }
    }

    #[test]
    fn revoke_before_first_use_is_an_enduring_tombstone() {
        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let scope = scope(CodeSessionId::new(), WorkspaceId::new());

        sessions.revoke(&registry, &scope);

        assert_eq!(
            sessions.capability_for(&registry, &scope),
            Err(BrowserRuntimeError::SessionEnded)
        );
    }

    #[test]
    fn revoked_session_never_reissues_a_native_capability() {
        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let scope = scope(CodeSessionId::new(), WorkspaceId::new());
        let capability_id = sessions.capability_for(&registry, &scope).unwrap();

        sessions.revoke(&registry, &scope);

        assert!(registry
            .heartbeat_agent_capability(capability_id, &scope.workspace.to_string())
            .is_err());
        assert_eq!(
            sessions.capability_for(&registry, &scope),
            Err(BrowserRuntimeError::SessionEnded)
        );
    }

    #[test]
    fn reused_session_id_with_another_scope_fails_closed() {
        let registry = BrowserRegistry::default();
        let sessions = SessionCapabilities::default();
        let session = CodeSessionId::new();
        let original = scope(session, WorkspaceId::new());
        let capability_id = sessions.capability_for(&registry, &original).unwrap();
        let replacement_scope = scope(session, WorkspaceId::new());

        assert_eq!(
            sessions.capability_for(&registry, &replacement_scope),
            Err(BrowserRuntimeError::SessionEnded)
        );
        assert!(registry
            .heartbeat_agent_capability(capability_id, &original.workspace.to_string())
            .is_err());
        assert_eq!(
            sessions.capability_for(&registry, &original),
            Err(BrowserRuntimeError::SessionEnded)
        );
    }
}
