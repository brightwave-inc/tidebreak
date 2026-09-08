//! One coding-session channel for native apps and host-approved Chrome.
//!
//! The server derives the subject from a live capability token. Native scope
//! validation also binds its owner/workspace, while this adapter retains Chrome
//! request identities after large results expire so retries cannot repeat input.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tauri::{AppHandle, Manager};
use tidebreak_core::computer_session::{is_chrome_session_tool, ComputerUseCall, ComputerUseOutcome, ComputerUseResult};
use tidebreak_core::{CancelToken, DocumentBlob, SessionId};
use tidebreak_server::chrome::ChromeScope;
use tidebreak_server::{NativeRuntime, NativeRuntimeError, NativeRuntimeScope};
use uuid::Uuid;

use crate::chrome_runtime_adapter::ChromeRuntimeAdapter;
use crate::host_access::HostAccess;
use crate::native_runtime_adapter::DesktopNativeRuntime;

const MAX_REQUESTS: usize = 4096;
const MAX_RESULTS: usize = 16;

pub(crate) struct DesktopComputerRuntime {
    app: AppHandle,
    native: DesktopNativeRuntime,
    chrome: Arc<ChromeRuntimeAdapter>,
    sessions: Mutex<HashMap<SessionId, Arc<ChromeSession>>>,
    shutting_down: AtomicBool,
}

struct ChromeSession {
    scope: NativeRuntimeScope,
    gate: tokio::sync::Mutex<()>,
    stop: Mutex<CancelToken>,
    revoked: AtomicBool,
    journal: Mutex<ChromeJournal>,
}

#[derive(Default)]
struct ChromeJournal {
    fingerprints: HashMap<Uuid, String>,
    results: HashMap<Uuid, ComputerUseResult>,
    order: VecDeque<Uuid>,
}

impl ChromeJournal {
    fn recall(&self, call: &ComputerUseCall) -> Result<Option<ComputerUseResult>, NativeRuntimeError> {
        let Some(prior) = self.fingerprints.get(&call.request_id) else { return Ok(None) };
        if prior != &fingerprint(call) { return Err(NativeRuntimeError::RequestConflict) }
        Ok(Some(self.results.get(&call.request_id).cloned().unwrap_or_else(|| unknown(call, "The stored result expired. Inspect the target before issuing a new action."))))
    }

    fn begin(&mut self, call: &ComputerUseCall) -> Result<(), NativeRuntimeError> {
        if self.fingerprints.len() >= MAX_REQUESTS {
            return Err(NativeRuntimeError::Unsupported("more requests in this session; start a new coding session".into()));
        }
        self.fingerprints.insert(call.request_id, fingerprint(call));
        self.finish(call, unknown(call, "The operation may have started. Inspect the target before issuing a new action."));
        Ok(())
    }

    fn finish(&mut self, call: &ComputerUseCall, result: ComputerUseResult) {
        if !self.results.contains_key(&call.request_id) { self.order.push_back(call.request_id); }
        self.results.insert(call.request_id, result);
        while self.order.len() > MAX_RESULTS {
            if let Some(id) = self.order.pop_front() { self.results.remove(&id); }
        }
    }
}

fn fingerprint(call: &ComputerUseCall) -> String {
    DocumentBlob::from_bytes(&serde_json::to_vec(call).expect("computer call is JSON")).id.to_string()
}

fn unknown(call: &ComputerUseCall, message: &str) -> ComputerUseResult {
    ComputerUseResult {
        request_id: call.request_id,
        outcome: ComputerUseOutcome::Unknown,
        text: message.into(),
        data: serde_json::json!({}),
        error_code: Some("unknown_outcome".into()),
        images: vec![],
    }
}

fn chrome_scope(scope: &NativeRuntimeScope, cancel: CancelToken) -> ChromeScope {
    ChromeScope { owner: scope.owner.clone(), workspace: scope.workspace, session: scope.session, cancel }
}

impl DesktopComputerRuntime {
    pub(crate) fn new(app: AppHandle, cache: std::path::PathBuf, home: std::path::PathBuf) -> Self {
        Self {
            native: DesktopNativeRuntime::new(app.clone()),
            chrome: Arc::new(ChromeRuntimeAdapter::new(app.clone(), cache, home)),
            app,
            sessions: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
        }
    }

    fn session(&self, scope: &NativeRuntimeScope) -> Result<Arc<ChromeSession>, NativeRuntimeError> {
        if self.shutting_down.load(Ordering::Acquire) { return Err(NativeRuntimeError::SessionEnded) }
        let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let session = sessions.entry(scope.session).or_insert_with(|| Arc::new(ChromeSession {
            scope: scope.clone(), gate: tokio::sync::Mutex::new(()), stop: Mutex::new(CancelToken::new()),
            revoked: AtomicBool::new(false), journal: Mutex::new(ChromeJournal::default()),
        }));
        if session.scope != *scope { return Err(NativeRuntimeError::NotAuthorized("This session belongs to a different owner or workspace.".into())) }
        if session.revoked.load(Ordering::Acquire) { return Err(NativeRuntimeError::SessionEnded) }
        Ok(session.clone())
    }

    /// Stop is synchronous, so queued input sees cancellation before any drain.
    pub(crate) fn stop_all_chrome(&self) {
        for session in self.sessions.lock().unwrap_or_else(|p| p.into_inner()).values() {
            let stop = session.stop.lock().unwrap_or_else(|p| p.into_inner());
            stop.cancel();
            self.chrome.stop_session(&chrome_scope(&session.scope, stop.clone()));
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.stop_all_chrome();
        self.chrome.shutdown().await;
    }
}

#[async_trait]
impl NativeRuntime for DesktopComputerRuntime {
    fn is_available(&self) -> bool { self.native.is_available() }

    async fn execute(&self, scope: &NativeRuntimeScope, call: &ComputerUseCall) -> Result<ComputerUseResult, NativeRuntimeError> {
        if !is_chrome_session_tool(&call.name) { return self.native.execute(scope, call).await }
        // The native adapter validates the same host-derived subject before a
        // Chrome request can create state or display its native consent prompt.
        self.native.result_for_call(scope, call).await?;
        self.app.state::<HostAccess>().require_local(crate::host_authority::Authority::ComputerUse)
            .await.map_err(NativeRuntimeError::NotAuthorized)?;
        let session = self.session(scope)?;
        let queued_stop = session.stop.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let _gate = session.gate.lock().await;
        if session.revoked.load(Ordering::Acquire) || self.shutting_down.load(Ordering::Acquire) { return Err(NativeRuntimeError::SessionEnded) }
        if let Some(result) = session.journal.lock().unwrap_or_else(|p| p.into_inner()).recall(call)? { return Ok(result) }
        let host = self.app.state::<HostAccess>();
        if host.computer_use.is_halted() { return Err(NativeRuntimeError::NotAuthorized("Computer control is stopped. Resume it in Tidebreak before requesting Chrome control.".into())) }
        let reconnect = call.name == tidebreak_core::chrome_connection::CHROME_CONNECT_TOOL;
        if queued_stop.is_cancelled() && !reconnect { return Err(NativeRuntimeError::NotAuthorized("Chrome is stopped. Request chrome_connect to obtain native approval to resume.".into())) }
        let stop = {
            let mut stop = session.stop.lock().unwrap_or_else(|p| p.into_inner());
            if stop.is_cancelled() && reconnect { *stop = CancelToken::new(); }
            stop.clone()
        };
        session.journal.lock().unwrap_or_else(|p| p.into_inner()).begin(call)?;
        let active_scope = chrome_scope(scope, stop.clone());
        let result = tokio::select! {
            biased;
            _ = stop.cancelled() => unknown(call, "Chrome was stopped. Inspect the target before issuing a new action."),
            _ = host.computer_use.wait_for_halt() => {
                stop.cancel();
                self.chrome.stop_session(&active_scope);
                unknown(call, "Computer control was stopped. Inspect the target before issuing a new action.")
            },
            result = self.chrome.execute(&active_scope, call) => result,
        };
        session.journal.lock().unwrap_or_else(|p| p.into_inner()).finish(call, result.clone());
        Ok(result)
    }

    async fn result_for_call(&self, scope: &NativeRuntimeScope, call: &ComputerUseCall) -> Result<Option<ComputerUseResult>, NativeRuntimeError> {
        if !is_chrome_session_tool(&call.name) { return self.native.result_for_call(scope, call).await }
        self.native.result_for_call(scope, call).await?;
        let session = self.session(scope)?;
        let result = session.journal.lock().unwrap_or_else(|p| p.into_inner()).recall(call);
        result
    }

    async fn cancel_session(&self, scope: &NativeRuntimeScope) -> Result<(), String> {
        if let Ok(session) = self.session(scope) {
            let stop = session.stop.lock().unwrap_or_else(|p| p.into_inner());
            stop.cancel();
            self.chrome.stop_session(&chrome_scope(scope, stop.clone()));
        }
        self.native.cancel_session(scope).await
    }

    fn revoke_session(&self, scope: &NativeRuntimeScope) {
        self.native.revoke_session(scope);
        if let Ok(session) = self.session(scope) {
            session.revoked.store(true, Ordering::Release);
            let stop = session.stop.lock().unwrap_or_else(|p| p.into_inner()).clone();
            stop.cancel();
            let scope = chrome_scope(scope, stop);
            self.chrome.stop_session(&scope);
            let chrome = self.chrome.clone();
            tauri::async_runtime::spawn(async move { chrome.revoke_session(&scope).await; });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call() -> ComputerUseCall { ComputerUseCall { request_id: Uuid::new_v4(), name: "chrome_act".into(), arguments: serde_json::json!({"target_ref":"tab-1"}) } }
    #[test]
    fn expired_results_never_repeat_an_action() {
        let mut journal = ChromeJournal::default();
        let first = call();
        journal.begin(&first).unwrap();
        for _ in 0..MAX_RESULTS { journal.begin(&call()).unwrap(); }
        let recovered = journal.recall(&first).unwrap().unwrap();
        assert_eq!(recovered.outcome, ComputerUseOutcome::Unknown);
        assert_eq!(journal.results.len(), MAX_RESULTS);
        let changed = ComputerUseCall { arguments: serde_json::json!({"target_ref":"tab-2"}), ..first };
        assert!(matches!(journal.recall(&changed), Err(NativeRuntimeError::RequestConflict)));
    }
    #[test]
    fn recovery_reads_do_not_create_new_requests() {
        let journal = ChromeJournal::default();
        assert!(journal.recall(&call()).unwrap().is_none());
        assert!(journal.fingerprints.is_empty());
    }
}
