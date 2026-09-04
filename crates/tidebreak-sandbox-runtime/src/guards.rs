//! Process-local cancellation and steering handles for sandbox work.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tidebreak_core::{AgentRunId, CallId, CancelToken};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Exact local cancellation handles for sandbox model and checkpoint attempts.
///
/// Every executor lane shares the checkpoint map because a cancellation
/// reaches each lane through the exact durable call, run, and executor lease
/// identity. Registration is RAII, so a stale permit cannot remove a newer
/// lease for the same durable identity.
#[derive(Default)]
pub struct SandboxAttemptGuard {
    models: Mutex<HashMap<(AgentRunId, Uuid), CancelToken>>,
    checkpoints: Mutex<HashMap<(CallId, AgentRunId, Uuid), CancelToken>>,
}

impl SandboxAttemptGuard {
    pub fn register_model(
        self: &Arc<Self>,
        agent_run_id: AgentRunId,
        lease_token: Uuid,
    ) -> Option<ActiveSandboxModelAttempt> {
        let mut models = self.models.lock().unwrap();
        let identity = (agent_run_id, lease_token);
        if models.contains_key(&identity) {
            return None;
        }
        let cancel = CancelToken::new();
        models.insert(identity, cancel.clone());
        Some(ActiveSandboxModelAttempt {
            guard: Arc::clone(self),
            agent_run_id,
            lease_token,
            cancel,
        })
    }

    pub fn register_checkpoint(
        self: &Arc<Self>,
        call_id: CallId,
        agent_run_id: AgentRunId,
        executor_lease_token: Uuid,
    ) -> Option<ActiveSandboxCheckpointAttempt> {
        let mut checkpoints = self.checkpoints.lock().unwrap();
        let identity = (call_id, agent_run_id, executor_lease_token);
        if checkpoints.contains_key(&identity) {
            return None;
        }
        let cancel = CancelToken::new();
        checkpoints.insert(identity, cancel.clone());
        Some(ActiveSandboxCheckpointAttempt {
            guard: Arc::clone(self),
            call_id,
            agent_run_id,
            executor_lease_token,
            cancel,
        })
    }

    /// Signal only a model handle owned by the exact durable run lease.
    pub fn cancel_model(&self, agent_run_id: AgentRunId, lease_token: Uuid) -> bool {
        if let Some(cancel) = self
            .models
            .lock()
            .unwrap()
            .get(&(agent_run_id, lease_token))
        {
            cancel.cancel();
            return true;
        }
        false
    }

    /// Signal only the exact sandbox call, run, and executor lease identity.
    pub fn cancel_checkpoint(
        &self,
        call_id: CallId,
        agent_run_id: AgentRunId,
        executor_lease_token: Uuid,
    ) -> bool {
        if let Some(cancel) =
            self.checkpoints
                .lock()
                .unwrap()
                .get(&(call_id, agent_run_id, executor_lease_token))
        {
            cancel.cancel();
            return true;
        }
        false
    }
}

/// Process-local handles for container-resident runs this process is driving.
///
/// Steering lasts for one attached connection. Cancellation lasts for the
/// whole exact claimed drive, including provisioning and reconnects. Durable
/// run state remains authoritative; these handles only close latency gaps.
#[derive(Default)]
pub struct SandboxSteerGuard {
    attached: Mutex<HashMap<(AgentRunId, Uuid), mpsc::Sender<String>>>,
    container_drives: Mutex<HashMap<(AgentRunId, Uuid), CancelToken>>,
    #[cfg(test)]
    steer_delivered: tokio::sync::Notify,
}

/// Why one steering instruction could not reach a live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxSteerRefusal {
    /// No connection to the run is live in this process.
    NotAttached,
    /// The run has not consumed its existing steering backlog.
    Backlogged,
}

impl SandboxSteerGuard {
    /// Register one exact claimed container drive for prompt cancellation.
    pub fn register_container_drive(
        self: &Arc<Self>,
        agent_run_id: AgentRunId,
        lease_token: Uuid,
    ) -> Option<ActiveSandboxContainerDrive> {
        let mut drives = self.container_drives.lock().unwrap();
        let identity = (agent_run_id, lease_token);
        if drives.contains_key(&identity) {
            return None;
        }
        let cancel = CancelToken::new();
        drives.insert(identity, cancel.clone());
        Some(ActiveSandboxContainerDrive {
            guard: Arc::clone(self),
            agent_run_id,
            lease_token,
            cancel,
        })
    }

    /// Signal only the container drive owned by the exact durable run lease.
    pub fn cancel_container_drive(&self, agent_run_id: AgentRunId, lease_token: Uuid) -> bool {
        if let Some(cancel) = self
            .container_drives
            .lock()
            .unwrap()
            .get(&(agent_run_id, lease_token))
        {
            cancel.cancel();
            return true;
        }
        false
    }

    /// Register the live connection's steering sink for one exact run and lease.
    pub fn register(
        self: &Arc<Self>,
        agent_run_id: AgentRunId,
        lease_token: Uuid,
        sink: mpsc::Sender<String>,
    ) -> Option<AttachedSandboxRun> {
        let mut attached = self.attached.lock().unwrap();
        let identity = (agent_run_id, lease_token);
        if attached.contains_key(&identity) {
            return None;
        }
        attached.insert(identity, sink);
        Some(AttachedSandboxRun {
            guard: Arc::clone(self),
            agent_run_id,
            lease_token,
        })
    }

    /// Hand `text` to the run's live connection.
    pub fn steer(
        &self,
        agent_run_id: AgentRunId,
        text: String,
    ) -> std::result::Result<(), SandboxSteerRefusal> {
        let attached = self.attached.lock().unwrap();
        let sink = attached
            .iter()
            .find(|((run, _lease), _sink)| *run == agent_run_id)
            .map(|(_identity, sink)| sink.clone())
            .ok_or(SandboxSteerRefusal::NotAttached)?;
        drop(attached);
        sink.try_send(text).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => SandboxSteerRefusal::Backlogged,
            mpsc::error::TrySendError::Closed(_) => SandboxSteerRefusal::NotAttached,
        })
    }

    #[cfg(test)]
    pub async fn wait_for_steer_delivery(&self) {
        self.steer_delivered.notified().await;
    }

    #[cfg(test)]
    pub fn record_steer_delivery(&self) {
        self.steer_delivered.notify_one();
    }
}

/// One exact claimed container drive, deregistered when the drive finishes.
pub struct ActiveSandboxContainerDrive {
    guard: Arc<SandboxSteerGuard>,
    agent_run_id: AgentRunId,
    lease_token: Uuid,
    cancel: CancelToken,
}

impl ActiveSandboxContainerDrive {
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Drop for ActiveSandboxContainerDrive {
    fn drop(&mut self) {
        self.guard
            .container_drives
            .lock()
            .unwrap()
            .remove(&(self.agent_run_id, self.lease_token));
    }
}

/// One attached sandbox run, deregistered when its connection ends.
pub struct AttachedSandboxRun {
    guard: Arc<SandboxSteerGuard>,
    agent_run_id: AgentRunId,
    lease_token: Uuid,
}

impl Drop for AttachedSandboxRun {
    fn drop(&mut self) {
        self.guard
            .attached
            .lock()
            .unwrap()
            .remove(&(self.agent_run_id, self.lease_token));
    }
}

/// One active in-process model attempt.
pub struct ActiveSandboxModelAttempt {
    guard: Arc<SandboxAttemptGuard>,
    agent_run_id: AgentRunId,
    lease_token: Uuid,
    cancel: CancelToken,
}

impl ActiveSandboxModelAttempt {
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Drop for ActiveSandboxModelAttempt {
    fn drop(&mut self) {
        self.guard
            .models
            .lock()
            .unwrap()
            .remove(&(self.agent_run_id, self.lease_token));
    }
}

/// One active sandbox checkpoint attempt.
pub struct ActiveSandboxCheckpointAttempt {
    guard: Arc<SandboxAttemptGuard>,
    call_id: CallId,
    agent_run_id: AgentRunId,
    executor_lease_token: Uuid,
    cancel: CancelToken,
}

impl ActiveSandboxCheckpointAttempt {
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl Drop for ActiveSandboxCheckpointAttempt {
    fn drop(&mut self) {
        self.guard.checkpoints.lock().unwrap().remove(&(
            self.call_id,
            self.agent_run_id,
            self.executor_lease_token,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_signals_require_the_exact_run_lease() {
        let guard = Arc::new(SandboxAttemptGuard::default());
        let run = AgentRunId::sandbox_for_spawn_call(CallId::new());
        let lease = Uuid::new_v4();
        let held = guard.register_model(run, lease).expect("register model");

        assert!(!guard.cancel_model(run, Uuid::new_v4()));
        assert!(!held.cancel_token().is_cancelled());
        assert!(!guard.cancel_model(AgentRunId::sandbox_for_spawn_call(CallId::new()), lease));
        assert!(guard.cancel_model(run, lease));
        assert!(held.cancel_token().is_cancelled());
        assert!(
            guard.cancel_model(run, lease),
            "an exact retry stays signalled"
        );
    }

    #[test]
    fn checkpoint_signals_require_the_exact_call_run_and_lease() {
        let guard = Arc::new(SandboxAttemptGuard::default());
        let call = CallId::new();
        let run = AgentRunId::sandbox_for_spawn_call(CallId::new());
        let lease = Uuid::new_v4();
        let held = guard
            .register_checkpoint(call, run, lease)
            .expect("register checkpoint");

        assert!(!guard.cancel_checkpoint(CallId::new(), run, lease));
        assert!(!guard.cancel_checkpoint(
            call,
            AgentRunId::sandbox_for_spawn_call(CallId::new()),
            lease
        ));
        assert!(!guard.cancel_checkpoint(call, run, Uuid::new_v4()));
        assert!(!held.cancel_token().is_cancelled());
        assert!(guard.cancel_checkpoint(call, run, lease));
        assert!(held.cancel_token().is_cancelled());
    }

    #[test]
    fn container_drive_signals_require_the_exact_run_lease() {
        let guard = Arc::new(SandboxSteerGuard::default());
        let run = AgentRunId::sandbox_for_spawn_call(CallId::new());
        let lease = Uuid::new_v4();
        let held = guard
            .register_container_drive(run, lease)
            .expect("register container drive");

        assert!(!guard.cancel_container_drive(run, Uuid::new_v4()));
        assert!(
            !guard.cancel_container_drive(AgentRunId::sandbox_for_spawn_call(CallId::new()), lease)
        );
        assert!(!held.cancel_token().is_cancelled());
        assert!(guard.cancel_container_drive(run, lease));
        assert!(held.cancel_token().is_cancelled());
        assert!(
            guard.cancel_container_drive(run, lease),
            "an exact retry stays signalled"
        );
        drop(held);
        assert!(!guard.cancel_container_drive(run, lease));
    }

    #[test]
    fn stale_and_current_attempts_are_isolated() {
        let guard = Arc::new(SandboxAttemptGuard::default());
        let run = AgentRunId::sandbox_for_spawn_call(CallId::new());
        let old_lease = Uuid::new_v4();
        let stale_model = guard
            .register_model(run, old_lease)
            .expect("register old model");
        let new_lease = Uuid::new_v4();
        let current_model = guard
            .register_model(run, new_lease)
            .expect("a distinct model lease may coexist until its heartbeat fences it");
        assert!(!stale_model.cancel_token().is_cancelled());
        assert!(!current_model.cancel_token().is_cancelled());
        assert!(guard.register_model(run, new_lease).is_none());
        drop(stale_model);
        assert!(!current_model.cancel_token().is_cancelled());
        assert!(guard.cancel_model(run, new_lease));
        assert!(current_model.cancel_token().is_cancelled());

        let call = CallId::new();
        let old_checkpoint_lease = Uuid::new_v4();
        let stale_checkpoint = guard
            .register_checkpoint(call, run, old_checkpoint_lease)
            .expect("register old checkpoint");
        let new_checkpoint_lease = Uuid::new_v4();
        let current_checkpoint = guard
            .register_checkpoint(call, run, new_checkpoint_lease)
            .expect("a distinct checkpoint lease may coexist until its heartbeat fences it");
        assert!(!stale_checkpoint.cancel_token().is_cancelled());
        assert!(!current_checkpoint.cancel_token().is_cancelled());
        assert!(guard
            .register_checkpoint(call, run, new_checkpoint_lease)
            .is_none());
        drop(stale_checkpoint);
        assert!(!current_checkpoint.cancel_token().is_cancelled());
        assert!(guard.cancel_checkpoint(call, run, new_checkpoint_lease));
        assert!(current_checkpoint.cancel_token().is_cancelled());
    }
}
