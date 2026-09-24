//! Triggers and the background sweeps the runtime starts on demand.

use super::*;

impl CodeRuntime {
    /// Stop every background sweep for good, for a server that is stopping
    /// before its data is deleted. Each is marked started first, so nothing
    /// starts it again, and dropping its guard aborts its task.
    pub(crate) fn stop_sweeps(&self) {
        for started in [
            &self.recovery_started,
            &self.stall_started,
            &self.watch_started,
            &self.trigger_started,
            &self.reconcile_started,
            &self.pr_refresh_started,
            &self.remote_started,
        ] {
            started.store(true, Ordering::SeqCst);
        }
        drop(self.recovery_sweep.lock().expect("recovery sweep").take());
        drop(self.stall_sweep.lock().expect("stall sweep").take());
        drop(self.watch_sweep.lock().expect("watch sweep").take());
        drop(self.trigger_sweep.lock().expect("trigger sweep").take());
        drop(self.reconcile_sweep.lock().expect("reconcile sweep").take());
        drop(
            self.pr_refresh_sweep
                .lock()
                .expect("pr refresh sweep")
                .take(),
        );
        drop(self.remote_sweep.lock().expect("remote sweep").take());
    }

    pub(super) fn ensure_stall_sweep(&self) {
        if self.stall_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard =
            crate::code::attention::StallSweepGuard::spawn(self.db.clone(), self.bus.clone());
        *self.stall_sweep.lock().expect("stall sweep") = Some(guard);
    }

    /// Start the watch sweep once. The guard holds a weak runtime handle so
    /// this field never keeps its own runtime alive.
    pub(in crate::code) fn ensure_watch_sweep(self: &Arc<Self>) {
        if self.watch_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = crate::code::watch::WatchSweepGuard::spawn(Arc::downgrade(self));
        *self.watch_sweep.lock().expect("watch sweep") = Some(guard);
    }

    /// Triggers armed on one repository.
    pub(crate) async fn list_triggers(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
    ) -> Result<Vec<CodeTrigger>, ServerError> {
        // Refuses an unknown repository rather than returning an empty list,
        // so a stale id reads as an error and not as "none armed".
        self.get_repo(owner, repo_id).await?;
        Ok(list_triggers_for_repo(&self.db, owner, repo_id).await?)
    }

    /// Arm a trigger on a repository.
    ///
    /// One row per `(repository, condition)`. A later arm sets its action and
    /// enables it in one upsert.
    pub(crate) async fn create_trigger(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        condition: CodeTriggerCondition,
        action: CodeTriggerAction,
    ) -> Result<CodeTrigger, ServerError> {
        self.get_repo(owner, repo_id).await?;
        let now = Utc::now();
        let trigger = CodeTrigger {
            id: CodeTriggerId::new(),
            owner: owner.clone(),
            repo_id,
            condition,
            action,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        arm_trigger(&self.db, owner, &trigger).await?;
        // Read back rather than returning what we just built. Two arms of the
        // same condition racing each other both see no row and both mint an
        // id; only one is stored, and the loser would otherwise answer 201
        // with an id that GET, PATCH and DELETE cannot find.
        list_triggers_for_repo(&self.db, owner, repo_id)
            .await?
            .into_iter()
            .find(|trigger| trigger.condition == condition)
            .ok_or_else(|| ServerError::internal("the trigger vanished after it was saved"))
    }

    /// Switch a trigger on or off, keeping the row so the scoping survives.
    pub(crate) async fn set_trigger_enabled(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        id: CodeTriggerId,
        enabled: bool,
    ) -> Result<CodeTrigger, ServerError> {
        if !update_trigger_enabled(&self.db, owner, repo_id, id, enabled, Utc::now()).await? {
            return Err(ServerError::not_found("trigger not found"));
        }
        list_triggers_for_repo(&self.db, owner, repo_id)
            .await?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| ServerError::not_found("trigger not found"))
    }

    /// Remove a repository-scoped trigger and its recorded fire fingerprints.
    pub(crate) async fn delete_trigger(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        id: CodeTriggerId,
    ) -> Result<(), ServerError> {
        if delete_trigger(&self.db, owner, repo_id, id).await? {
            Ok(())
        } else {
            Err(ServerError::not_found("trigger not found"))
        }
    }

    /// Start the trigger sweep once. Same weak-handle shape as the watch
    /// sweep, on its own interval so the two do not read GitHub together.
    pub(in crate::code) fn ensure_trigger_sweep(self: &Arc<Self>) {
        if self.trigger_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = crate::code::trigger::TriggerSweepGuard::spawn(Arc::downgrade(self));
        *self.trigger_sweep.lock().expect("trigger sweep") = Some(guard);
    }

    /// Start the pull-request reconcile sweep once (decision 77). Same
    /// weak-handle shape, on a third coprime interval.
    pub(in crate::code) fn ensure_reconcile_sweep(self: &Arc<Self>) {
        if self.reconcile_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = crate::code::reconcile::ReconcileSweepGuard::spawn(Arc::downgrade(self));
        *self.reconcile_sweep.lock().expect("reconcile sweep") = Some(guard);
    }

    /// Start the remote-session sweep once, on deployments that configured
    /// remote execution. A no-op everywhere else.
    pub(in crate::code) fn ensure_remote_sweep(self: &Arc<Self>) {
        if self.remote.is_none() || self.remote_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let guard = crate::code::remote::service::RemoteSweepGuard::spawn(Arc::downgrade(self));
        *self.remote_sweep.lock().expect("remote sweep") = Some(guard);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Delete all data stops the server before it removes the folders the
    /// sweeps write into, so a stopped sweep must stay stopped: a watch or a
    /// trigger armed afterwards cannot start it again.
    #[tokio::test]
    async fn stopped_sweeps_do_not_start_again() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            tidebreak_core::DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("code.db").display()
            ))
            .await
            .unwrap(),
        );
        let runtime = Arc::new(CodeRuntime::new(
            db,
            dir.path().into(),
            None,
            None,
            None,
            None,
            None,
            None,
        ));
        runtime.ensure_watch_sweep();
        assert!(runtime.watch_sweep.lock().unwrap().is_some());

        runtime.stop_sweeps();
        assert!(runtime.watch_sweep.lock().unwrap().is_none());
        runtime.ensure_watch_sweep();
        runtime.ensure_trigger_sweep();
        runtime.ensure_reconcile_sweep();
        assert!(runtime.watch_sweep.lock().unwrap().is_none());
        assert!(runtime.trigger_sweep.lock().unwrap().is_none());
        assert!(runtime.reconcile_sweep.lock().unwrap().is_none());
    }
}
