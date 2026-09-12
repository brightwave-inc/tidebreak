//! Remote and external sessions: creation, remote turns, and queue promotion.

use super::*;
use tidebreak_core::ExecutionLocation;

/// How long a machine-location message waits for its worker to promote the
/// queue head before the route answers `queued` (decision 0088).
const MACHINE_PROMOTION_POLL: std::time::Duration = std::time::Duration::from_millis(50);
const MACHINE_PROMOTION_POLLS: usize = 40;

/// What get-or-create decided the session acts as, for the adapter to render.
/// `acting_login`, `app_name`, and `connect_url` are known at create when the
/// forge answered; an existing session reports `acts_as` from the row and
/// leaves the rest empty (they are not stored on the session).
#[derive(Debug, Clone)]
pub struct ExternalActsAsView {
    pub acts_as: tidebreak_core::ActsAs,
    pub acting_login: Option<String>,
    pub app_name: Option<String>,
    pub connect_url: Option<String>,
}

impl CodeRuntime {
    /// Resolve the original Slack consent before creating or queueing work.
    async fn validated_external_delegation(
        &self,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
    ) -> Result<Option<Arc<crate::obo_gateway::OboGateway>>, ServerError> {
        let Some(external) = self
            .harness_llm
            .as_ref()
            .and_then(|relay| relay.external_delegations())
        else {
            return Ok(None);
        };
        match external.for_grant(owner, grant_id).await {
            Ok(delegated) => Ok(Some(delegated)),
            Err(
                tidebreak_core::AgentError::SignInRequired(_)
                | tidebreak_core::AgentError::InvalidTarget(_),
            ) => {
                let workspace =
                    tidebreak_core::db::code::get_external_grant(&self.db, owner, grant_id)
                        .await?
                        .is_some_and(|grant| grant.kind.is_workspace());
                Err(ServerError::conflict_kind(
                    "external_reconnect_required",
                    if workspace {
                        "Your Slack workspace connection needs administrator approval again. Ask an administrator to restore the Tidebreak connection for this Slack workspace."
                    } else {
                        "Your Slack connection needs approval again. Send `reconnect` to Tidebreak, then approve the connection."
                    },
                ))
            }
            Err(error) => Err(ServerError::from(error)),
        }
    }

    /// Publish a fixed public failure summary, without transport error details.
    async fn note_remote_startup_failure(
        &self,
        session: &Session,
        attention: Attention,
        message: String,
    ) -> Result<(), ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Ok(());
        };
        if remote.startup_failure_matches(session.id, &message) {
            return Ok(());
        }
        let event = tidebreak_core::Event::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Warning,
            message: message.clone(),
        };
        let seq = tidebreak_core::db::code::append_event(
            &self.db,
            &session.owner,
            session.id,
            session.spawn_epoch,
            &event,
        )
        .await
        .map_err(|error| tidebreak_core::AgentError::Store(error.to_string()))?;
        self.bus
            .publish(session.id, tidebreak_core::SequencedEvent { seq, event });
        remote.record_startup_failure(session.id, message);
        crate::code::attention::apply_attention(
            &self.db,
            &self.bus,
            &session.owner,
            session.id,
            attention,
            false,
        )
        .await?;
        Ok(())
    }

    /// Create a workspace whose checkout lives in a sandbox, not on this
    /// machine. A per-workspace `remote:<id>` worktree marker records that
    /// state ([`CodeWorkspace::is_remote`]); nothing here touches the
    /// filesystem.
    ///
    /// The authenticated remote-workspace route exposes this owner-scoped
    /// runtime path.
    pub(crate) async fn create_remote_workspace(
        &self,
        owner: &OwnerId,
        repo_id: RepoId,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        if repo.origin_host.is_none() || repo.origin_owner.is_none() || repo.origin_name.is_none() {
            return Err(ServerError::conflict_kind(
                "repo_origin_unknown",
                "the repository records no origin, so a sandbox cannot clone it",
            ));
        }
        let workspace = self.build_remote_workspace(owner, &repo, title).await?;
        insert_workspace(&self.db, &workspace).await?;
        Ok(workspace)
    }

    /// Validate and shape a remote workspace value without inserting it, so
    /// a caller can commit it atomically with the rows that depend on it.
    pub(super) async fn build_remote_workspace(
        &self,
        owner: &OwnerId,
        repo: &CodeRepo,
        title: Option<String>,
    ) -> Result<CodeWorkspace, ServerError> {
        let title = title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let id = WorkspaceId::new();
        // A sandbox has no local branch creation to arbitrate collisions. Include
        // the workspace id so repeated prompts and concurrent starts stay distinct.
        let branch = format!(
            "{}-{}",
            branch_name(&repo.branch_prefix, &title, id.as_uuid()),
            id
        );
        Ok(CodeWorkspace {
            id,
            owner: owner.clone(),
            repo_id: repo.id,
            title,
            worktree_path: CodeWorkspace::remote_worktree_marker(id),
            branch_name: branch,
            base_ref: repo.default_base_ref.clone(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
            setup_error: None,
        })
    }

    pub(super) fn validate_remote_execution(&self, session: &Session) -> Result<(), ServerError> {
        if let Some(remote) = self.remote_sessions() {
            remote
                .settings
                .validate_execution(session)
                .map_err(|message| {
                    ServerError::unprocessable_kind("sandbox_settings_unavailable", message)
                })?;
        }
        Ok(())
    }

    pub(super) fn validate_remote_settings_change(
        &self,
        session: &Session,
        next: &SessionExecutionSettings,
    ) -> Result<(), ServerError> {
        if self
            .remote_sessions()
            .is_some_and(|remote| remote.settings.engine.is_some())
            && *next != SessionExecutionSettings::from(session)
        {
            return Err(ServerError::conflict_kind(
                "sandbox_settings_fixed",
                "choose the model and reasoning effort when starting a new sandbox session; this profile cannot change them during a session",
            ));
        }
        Ok(())
    }

    /// Shape a remote session value bound to `workspace`, uninserted.
    pub(super) fn remote_session_value(
        owner: &OwnerId,
        owner_kind: Option<&str>,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Session {
        Session {
            visibility: tidebreak_core::SessionVisibility::Private,
            id: SessionId::new(),
            owner: owner.clone(),
            owner_kind: owner_kind.map(str::to_owned),
            workspace_id: Some(workspace_id),
            kind: SessionKind::Interactive,
            harness_kind: harness,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: settings.permission_mode,
            model: normalize_model(settings.model),
            reasoning_effort: settings.reasoning_effort,
            fast_mode: settings.fast_mode,
            lifecycle: SessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
            spawn_epoch: 1,
            attention: Attention::working(AttentionSource::Lifecycle),
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
            execution_location: tidebreak_core::ExecutionLocation::Sandbox,
            acts_as: settings.acts_as,
        }
    }

    /// Bind an external conversation to a session, creating the remote
    /// workspace, session, and binding together on first contact
    /// (docs/slack-sessions.md, stage 2).
    ///
    /// Idempotent across the channel's retries: a bound conversation
    /// answers with its session, an ended one answers `Ended` rather than
    /// resurrecting, and a binding under another grant refuses. Two racing
    /// creates converge on one session through the binding's unique
    /// conversation key.
    ///
    /// `requested_mode` is the permission mode the channel named, if any. On
    /// the machine's engine it is honored up to the operator's ceiling and
    /// refused by name above it; absent, the session takes the operator's
    /// default (decision 88). A sandbox session is `Allow` because confinement
    /// is its boundary (decision 39), so a request for any other mode there
    /// is refused rather than approximated.
    #[allow(clippy::too_many_arguments)]
    pub async fn external_get_or_create(
        &self,
        owner: &OwnerId,
        owner_kind: Option<&str>,
        grant_id: tidebreak_core::CodeGrantId,
        channel_kind: &str,
        external_key: &str,
        repo_id: impl Into<Option<RepoId>>,
        title: Option<String>,
        harness: HarnessKind,
        settings: NewSessionSettings,
        requested_mode: Option<tidebreak_core::PermissionMode>,
        requested_acts_as: Option<tidebreak_core::ActsAs>,
    ) -> Result<
        (
            tidebreak_core::ExternalSessionResolution,
            ExternalActsAsView,
        ),
        ServerError,
    > {
        self.external_get_or_create_with_channel_context(
            owner,
            owner_kind,
            grant_id,
            channel_kind,
            external_key,
            repo_id,
            title,
            harness,
            settings,
            requested_mode,
            requested_acts_as,
            None,
        )
        .await
    }

    /// Capture channel instructions and provenance before a session or worker
    /// becomes visible to an adapter retry.
    #[allow(clippy::too_many_arguments)]
    pub async fn external_get_or_create_with_channel_context(
        &self,
        owner: &OwnerId,
        owner_kind: Option<&str>,
        grant_id: tidebreak_core::CodeGrantId,
        channel_kind: &str,
        external_key: &str,
        repo_id: impl Into<Option<RepoId>>,
        title: Option<String>,
        harness: HarnessKind,
        settings: NewSessionSettings,
        requested_mode: Option<tidebreak_core::PermissionMode>,
        requested_acts_as: Option<tidebreak_core::ActsAs>,
        channel_context: Option<tidebreak_core::db::code::ExternalSessionChannelContext<'_>>,
    ) -> Result<
        (
            tidebreak_core::ExternalSessionResolution,
            ExternalActsAsView,
        ),
        ServerError,
    > {
        if channel_kind.trim().is_empty() || external_key.trim().is_empty() {
            return Err(ServerError::conflict_kind(
                "binding_key_invalid",
                "a binding needs a channel kind and a conversation key",
            ));
        }
        let repo_id = repo_id.into();
        // The internal coordinator is a machine-side surface that owns the
        // native tools; a configured runtime never silently moves it into a
        // sandbox. Explicit external harnesses (and future conversation
        // harnesses) take the deployment placement. Repository-backed
        // sessions keep their existing rule unchanged.
        let location = if repo_id.is_none() && harness == HarnessKind::Internal {
            ExecutionLocation::Machine
        } else {
            self.external_execution_location()
        };
        let delegated = self.validated_external_delegation(owner, grant_id).await?;
        // The fast path costs one read and builds nothing.
        if let Some(binding) = tidebreak_core::db::code::get_external_binding(
            &self.db,
            owner,
            channel_kind,
            external_key,
        )
        .await?
        {
            if binding.grant_id != grant_id {
                return Ok((
                    tidebreak_core::ExternalSessionResolution::GrantMismatch,
                    ExternalActsAsView {
                        acts_as: tidebreak_core::ActsAs::default_for_owner_kind(owner_kind),
                        acting_login: None,
                        app_name: None,
                        connect_url: None,
                    },
                ));
            }
            let session = self.get_session(owner, binding.session_id).await?;
            let identity = ExternalActsAsView {
                acts_as: session.acts_as(),
                acting_login: None,
                app_name: None,
                connect_url: None,
            };
            if session.lifecycle == SessionLifecycle::Ended {
                return Ok((
                    tidebreak_core::ExternalSessionResolution::Ended {
                        session_id: binding.session_id,
                    },
                    identity,
                ));
            }
            return Ok((
                tidebreak_core::ExternalSessionResolution::Existing(Box::new(binding)),
                identity,
            ));
        }
        let mut settings = settings;
        if location == ExecutionLocation::Sandbox
            && matches!(harness, HarnessKind::ClaudeCode | HarnessKind::Codex)
        {
            let listings = match delegated.as_ref() {
                Some(gateway) => Some(gateway.compat_listings(owner).await?),
                None => match self.harness_llm() {
                    Some(relay) => Some(relay.listings(owner).await?),
                    None => None,
                },
            };
            if let Some((anthropic, openai)) = listings {
                settings.model = Some(select_external_sandbox_model(
                    harness,
                    settings.model.as_deref(),
                    anthropic,
                    openai,
                )?);
            }
        }
        let workspace_grant =
            tidebreak_core::db::code::get_external_grant(&self.db, owner, grant_id)
                .await?
                .is_some_and(|grant| grant.kind.is_workspace());
        let requested_acts_as = if workspace_grant {
            Some(tidebreak_core::ActsAs::Bot)
        } else {
            requested_acts_as
        };
        let lender: Option<&dyn crate::obo_gateway::GitCredentialLender> = delegated
            .as_ref()
            .map(|gateway| gateway.as_ref() as &dyn crate::obo_gateway::GitCredentialLender)
            .or_else(|| self.git_credentials().map(|lender| lender.as_ref()));
        // A conversation needs no forge connection until it chooses a
        // repository. Its identity is still fixed before the first turn.
        let identity = if repo_id.is_none() {
            ExternalActsAsView {
                acts_as: if workspace_grant || owner_kind == Some("service") {
                    tidebreak_core::ActsAs::Bot
                } else {
                    requested_acts_as.unwrap_or(tidebreak_core::ActsAs::Person)
                },
                acting_login: None,
                app_name: None,
                connect_url: None,
            }
        } else {
            self.decide_external_acts_as(owner, owner_kind, requested_acts_as, lender)
                .await?
        };
        let Some(repo_id) = repo_id else {
            return match location {
                ExecutionLocation::Sandbox => {
                    if let Some(mode) = requested_mode.filter(|mode| *mode != PermissionMode::Allow)
                    {
                        return Err(ServerError::conflict_kind(
                            "permission_mode_unsupported",
                            format!(
                                "this deployment runs channel sessions in a sandbox, which is \
                                 always allow; {mode} is not available here"
                            ),
                        ));
                    }
                    let session = self
                        .build_repositoryless_remote_session(
                            owner,
                            owner_kind,
                            harness,
                            NewSessionSettings {
                                permission_mode: PermissionMode::Allow,
                                acts_as: Some(identity.acts_as),
                                ..settings
                            },
                        )
                        .await?;
                    let resolution =
                        tidebreak_core::db::code::resolve_external_session_with_channel_context(
                            &self.db,
                            owner,
                            grant_id,
                            channel_kind,
                            external_key,
                            None,
                            &session,
                            channel_context,
                        )
                        .await?;
                    Ok((resolution, identity))
                }
                ExecutionLocation::Machine => {
                    let policy = self.external_permission;
                    let mode = requested_mode.unwrap_or(policy.default_mode);
                    if mode > policy.ceiling {
                        return Err(ServerError::conflict_kind(
                            "permission_mode_above_ceiling",
                            format!(
                                "This deployment allows channel sessions up to {}. To allow {mode}, raise TIDEBREAK_EXTERNAL_PERMISSION_CEILING.",
                                policy.ceiling
                            ),
                        ));
                    }
                    let session = self
                        .build_repositoryless_session(
                            owner,
                            owner_kind,
                            harness,
                            NewSessionSettings {
                                permission_mode: mode,
                                permission_mode_ceiling: Some(policy.ceiling),
                                acts_as: Some(identity.acts_as),
                                ..settings
                            },
                            Some(grant_id),
                        )
                        .await?;
                    let resolution =
                        tidebreak_core::db::code::resolve_external_session_with_channel_context(
                            &self.db,
                            owner,
                            grant_id,
                            channel_kind,
                            external_key,
                            None,
                            &session,
                            channel_context,
                        )
                        .await?;
                    if matches!(
                        resolution,
                        tidebreak_core::ExternalSessionResolution::Created(_)
                    ) {
                        self.attach_and_spawn_worker(session).await?;
                    }
                    Ok((resolution, identity))
                }
            };
        };
        let repo = self.get_repo(owner, repo_id).await?;
        Self::refuse_removed_repo(&repo)?;
        if workspace_grant {
            let origin = Self::workspace_repository_origin(&repo)?;
            self.require_workspace_repository_access(owner, grant_id, &origin)
                .await?;
        }
        match location {
            ExecutionLocation::Sandbox => {
                if let Some(mode) = requested_mode.filter(|mode| *mode != PermissionMode::Allow) {
                    return Err(ServerError::conflict_kind(
                        "permission_mode_unsupported",
                        format!(
                            "this deployment runs channel sessions in a sandbox, which is \
                             always allow; {mode} is not available here"
                        ),
                    ));
                }
                if repo.origin_host.is_none()
                    || repo.origin_owner.is_none()
                    || repo.origin_name.is_none()
                {
                    return Err(ServerError::conflict_kind(
                        "repo_origin_unknown",
                        "the repository records no origin, so a sandbox cannot clone it",
                    ));
                }
                let settings = NewSessionSettings {
                    acts_as: Some(identity.acts_as),
                    ..settings
                };
                let workspace = self.build_remote_workspace(owner, &repo, title).await?;
                let session =
                    Self::remote_session_value(owner, owner_kind, workspace.id, harness, settings);
                self.validate_remote_execution(&session)?;
                let resolution =
                    tidebreak_core::db::code::resolve_external_session_with_channel_context(
                        &self.db,
                        owner,
                        grant_id,
                        channel_kind,
                        external_key,
                        Some(&workspace),
                        &session,
                        channel_context,
                    )
                    .await?;
                Ok((resolution, identity))
            }
            ExecutionLocation::Machine => {
                // The machine's own engine: the ordinary local workspace and
                // session, then the binding. The channel's `Allow` is a
                // sandbox posture; on the machine the session takes the mode
                // the channel named, up to the operator's ceiling, else the
                // operator's default, and the owner decides approvals from
                // the desktop, the web, or the channel once it can carry
                // them (decision 0088).
                let policy = self.external_permission;
                let mode = requested_mode.unwrap_or(policy.default_mode);
                if mode > policy.ceiling {
                    return Err(ServerError::conflict_kind(
                        "permission_mode_above_ceiling",
                        format!(
                            "this deployment allows channel sessions up to {} on its own \
                             engine; {mode} needs the operator to raise \
                             TIDEBREAK_EXTERNAL_PERMISSION_CEILING",
                            policy.ceiling
                        ),
                    ));
                }
                let settings = NewSessionSettings {
                    permission_mode: mode,
                    acts_as: Some(identity.acts_as),
                    ..settings
                };
                let (workspace, _base_refresh_warning) = self
                    .create_workspace_with_git_credentials(
                        owner,
                        repo_id,
                        title,
                        None,
                        None,
                        lender,
                        identity.acts_as,
                    )
                    .await?;
                let session = self
                    .create_session_of_kind_unattached(
                        owner,
                        owner_kind,
                        workspace.id,
                        SessionKind::Interactive,
                        harness,
                        settings,
                        Some(grant_id),
                    )
                    .await?;
                let resolution =
                    tidebreak_core::db::code::resolve_external_session_with_channel_context(
                        &self.db,
                        owner,
                        grant_id,
                        channel_kind,
                        external_key,
                        None,
                        &session,
                        channel_context,
                    )
                    .await?;
                if matches!(
                    resolution,
                    tidebreak_core::ExternalSessionResolution::Created(_)
                ) {
                    self.attach_and_spawn_worker(session).await?;
                }
                Ok((resolution, identity))
            }
        }
    }

    /// Choose the session's forge identity once, before the workspace is
    /// cloned, so the clone borrows the same identity the session will
    /// (decision 0090 amendment).
    async fn decide_external_acts_as(
        &self,
        owner: &OwnerId,
        owner_kind: Option<&str>,
        requested: Option<tidebreak_core::ActsAs>,
        lender: Option<&dyn crate::obo_gateway::GitCredentialLender>,
    ) -> Result<ExternalActsAsView, ServerError> {
        use crate::obo_gateway::{GitForgeAttribution, GitForgeAttributionRequest, GitForgeError};

        if owner_kind == Some("service") {
            return self.external_bot_identity(owner, lender, None).await;
        }
        let Some(lender) = lender else {
            // Standalone and desktop: local git identity, no probe.
            return Ok(ExternalActsAsView {
                acts_as: requested.unwrap_or(tidebreak_core::ActsAs::Person),
                acting_login: None,
                app_name: None,
                connect_url: None,
            });
        };
        if requested == Some(tidebreak_core::ActsAs::Bot) {
            return self.external_bot_identity(owner, Some(lender), None).await;
        }
        match lender
            .git_forge_identity(owner, GitForgeAttributionRequest::Person)
            .await
        {
            Ok(identity) => match identity.attribution {
                GitForgeAttribution::Person { login, .. } => Ok(ExternalActsAsView {
                    acts_as: tidebreak_core::ActsAs::Person,
                    acting_login: Some(login),
                    app_name: Some(identity.app_name).filter(|name| !name.is_empty()),
                    connect_url: None,
                }),
                GitForgeAttribution::Bot { bot_login } => Ok(ExternalActsAsView {
                    acts_as: tidebreak_core::ActsAs::Bot,
                    acting_login: bot_login,
                    app_name: Some(identity.app_name).filter(|name| !name.is_empty()),
                    connect_url: None,
                }),
            },
            Err(GitForgeError::NotConnected { connect_url }) => {
                self.external_bot_identity(owner, Some(lender), connect_url)
                    .await
            }
            Err(GitForgeError::PersonNotOffered | GitForgeError::NoGitForge) => {
                self.external_bot_identity(owner, Some(lender), None).await
            }
            Err(GitForgeError::Unavailable(detail)) => Err(ServerError::bad_gateway_kind(
                "forge_unavailable",
                format!("the forge is temporarily unavailable; retry ({detail})"),
            )),
            Err(_) => Err(ServerError::bad_gateway_kind(
                "forge_unavailable",
                "the forge could not answer who this session acts as; retry",
            )),
        }
    }

    async fn external_bot_identity(
        &self,
        owner: &OwnerId,
        lender: Option<&dyn crate::obo_gateway::GitCredentialLender>,
        connect_url: Option<String>,
    ) -> Result<ExternalActsAsView, ServerError> {
        use crate::obo_gateway::{GitForgeAttribution, GitForgeAttributionRequest};

        let mut view = ExternalActsAsView {
            acts_as: tidebreak_core::ActsAs::Bot,
            acting_login: None,
            app_name: None,
            connect_url,
        };
        let Some(lender) = lender else {
            return Ok(view);
        };
        if let Ok(identity) = lender
            .git_forge_identity(owner, GitForgeAttributionRequest::Installation)
            .await
        {
            view.app_name = Some(identity.app_name).filter(|name| !name.is_empty());
            if let GitForgeAttribution::Bot { bot_login } = identity.attribution {
                view.acting_login = bot_login;
            }
        }
        Ok(view)
    }

    /// Where an external session runs on this deployment (decision 0088):
    /// a gateway sandbox when a sandbox runtime is configured, else the
    /// machine's own engine. Desktop, mobile, and `agent-mcp` sessions do
    /// not use this default. The machine is the floor, not an interim path.
    #[must_use]
    pub fn external_execution_location(&self) -> ExecutionLocation {
        if self.remote.is_some() {
            ExecutionLocation::Sandbox
        } else {
            ExecutionLocation::Machine
        }
    }

    /// Create a session on a remote workspace: a row and nothing else. No
    /// local harness is probed or spawned — the sandbox carries the engine,
    /// and the first turn provisions it.
    ///
    /// The authenticated remote-session route exposes this owner-scoped
    /// runtime path.
    pub(crate) async fn create_remote_session(
        &self,
        owner: &OwnerId,
        owner_kind: Option<&str>,
        workspace_id: WorkspaceId,
        harness: HarnessKind,
        settings: NewSessionSettings,
    ) -> Result<Session, ServerError> {
        if self.remote.is_none() {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        }
        let lifecycle = self.workspace_lifecycle_lock(workspace_id);
        let _lifecycle_guard = lifecycle.lock().await;
        let workspace = self.get_workspace(owner, workspace_id).await?;
        if !workspace.is_remote() {
            return Err(ServerError::conflict_kind(
                "workspace_not_remote",
                "this workspace has a local checkout; create a local session on it",
            ));
        }
        if workspace.status != CodeWorkspaceStatus::Active {
            return Err(ServerError::conflict_kind(
                "workspace_not_ready",
                format!("workspace is {}", workspace.status.as_str()),
            ));
        }
        let session =
            Self::remote_session_value(owner, owner_kind, workspace_id, harness, settings);
        self.validate_remote_execution(&session)?;
        insert_session(&self.db, &session).await?;
        Ok(session)
    }

    /// Submit one turn to a remote session's sandbox (`docs/slack-sessions.md`).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn submit_remote_turn(
        &self,
        owner: &OwnerId,
        mut session: Session,
        workspace: Option<&CodeWorkspace>,
        message: String,
        model: Option<String>,
        reasoning_effort: Option<Option<ReasoningEffort>>,
        attachments: Vec<tidebreak_core::ImageRef>,
        actor: Option<tidebreak_core::TurnActor>,
        trigger_delivery: Option<TriggerDeliveryClaim>,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        };
        if trigger_delivery.is_some() {
            // Trigger delivery is at-most-once. The runtime's spawn and inbox
            // calls accept no idempotency key and expose no replay result, so
            // retrying an ambiguous response could run one trigger twice.
            return Err(ServerError::conflict_kind(
                "remote_triggers_unsupported",
                "remote trigger turns are disabled because sandbox spawn and inbox calls have no idempotency key; submit the turn manually",
            ));
        }
        if !attachments.is_empty() {
            // Remote messages carry text only. Until the runtime provides a
            // bounded, owner-scoped file transfer, attachment bytes have no
            // safe path into the sandbox.
            return Err(ServerError::conflict_kind(
                "remote_attachments_unsupported",
                "remote sessions cannot stage attachment bytes because the sandbox message contract carries text only; send the turn without attachments",
            ));
        }
        self.validate_remote_execution(&session)?;
        // The declared supervised image reads settings only at spawn. Inbox
        // messages cannot change them, so preserve the session contract.
        let mut next = SessionExecutionSettings::from(&session);
        if let Some(model) = normalize_model(model) {
            next.model = Some(model);
        }
        if let Some(effort) = reasoning_effort {
            next.reasoning_effort = effort;
        }
        self.validate_remote_settings_change(&session, &next)?;
        if next != SessionExecutionSettings::from(&session) {
            session = replace_session_execution_settings(&self.db, owner, &session, &next)
                .await?
                .ok_or_else(|| {
                    ServerError::conflict_kind(
                        "session_settings_changed",
                        "the session settings changed before the turn could reserve them",
                    )
                })?;
        }
        // Queue-default, exactly as the local path: a busy session parks the
        // send as a durable row the remote sweep promotes at the next idle.
        let in_flight = session.lifecycle == SessionLifecycle::Running
            || get_open_turn(&self.db, owner, session.id).await?.is_some();
        let backlog = !tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
            .await?
            .is_empty();
        if in_flight || backlog {
            if !queue_if_busy {
                return Err(ServerError::conflict_kind(
                    "trigger_turn_busy",
                    "the turn was not accepted because the session is busy",
                ));
            }
            return self
                .park_remote_follow_up(owner, &session, message, actor)
                .await;
        }
        let repo = match workspace {
            Some(workspace) => Some(self.get_repo(owner, workspace.repo_id).await?),
            None => None,
        };
        let scratch_branch = workspace
            .is_none()
            .then(|| format!("scratch-{}", session.id));
        let driver = remote.driver(&self.db, self.bus.as_ref());
        let outcome = driver
            .submit_turn(
                &mut session,
                workspace,
                repo.as_ref(),
                scratch_branch.as_deref(),
                &message,
            )
            .await?;
        // A provisioned or delivered turn has events to drain and a parked
        // one has a head to promote; either way the sweep should look now,
        // not at its next floor.
        remote.wake_sweep();
        self.relay_remote_outcome(owner, &session, outcome, message, actor, queue_if_busy)
            .await
    }

    /// Translate a driver outcome into the submit answer the routes speak.
    pub(super) async fn relay_remote_outcome(
        &self,
        owner: &OwnerId,
        session: &Session,
        outcome: crate::code::remote::driver::RemoteTurnOutcome,
        message: String,
        actor: Option<tidebreak_core::TurnActor>,
        queue_if_busy: bool,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        use crate::code::remote::driver::RemoteTurnOutcome as Outcome;
        match outcome {
            Outcome::Delivered { turn } | Outcome::Reincarnated { turn, .. } => {
                Ok(SubmitTurnOutcome::Ran(turn))
            }
            Outcome::TurnInFlight | Outcome::ReincarnationInFlight | Outcome::FlushPending => {
                if !queue_if_busy {
                    return Err(ServerError::conflict_kind(
                        "trigger_turn_busy",
                        "the turn was not accepted because the session is busy",
                    ));
                }
                self.park_remote_follow_up(owner, session, message, actor)
                    .await
            }
            Outcome::CapExhausted { running } => Err(ServerError::conflict_kind(
                "sandbox_cap_exhausted",
                format!(
                    "the sandbox cap is full: {} session(s) hold the slots",
                    running.len()
                ),
            )),
            Outcome::SpendExhausted {
                spent_microusd,
                ceiling_microusd,
            } => Err(ServerError::conflict_kind(
                "session_spend_exhausted",
                format!(
                    "this session has spent {spent_microusd} of its {ceiling_microusd} micro-USD ceiling and takes no more turns"
                ),
            )),
            Outcome::RecoveryBlocked { code, message } => {
                Err(ServerError::conflict_kind(code, message))
            }
            Outcome::SignInRequired => Err(ServerError::conflict_kind(
                "sign_in_required",
                "sign in to the sandbox environment, then retry",
            )),
        }
    }

    /// Park a message for a remote session. The remote sweep promotes the
    /// head at the next idle; there is no worker to nudge.
    pub(super) async fn park_remote_follow_up(
        &self,
        owner: &OwnerId,
        session: &Session,
        message: String,
        actor: Option<tidebreak_core::TurnActor>,
    ) -> Result<SubmitTurnOutcome, ServerError> {
        self.validate_remote_execution(session)?;
        let queued = tidebreak_core::db::code::list_queued_turns(&self.db, owner, session.id)
            .await
            .map_err(ServerError::from)?;
        if queued.len() >= QueuedTurn::MAX_PER_SESSION {
            return Err(ServerError::conflict_kind(
                "queue_full",
                format!(
                    "this session may queue at most {} messages",
                    QueuedTurn::MAX_PER_SESSION
                ),
            ));
        }
        let now = chrono::Utc::now();
        let row = tidebreak_core::db::code::enqueue_queued_turn(
            &self.db,
            owner,
            &QueuedTurn {
                id: TurnId::new(),
                session_id: session.id,
                message,
                actor,
                attachments: Vec::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .map_err(ServerError::from)?;
        if let Some(remote) = self.remote_sessions() {
            remote.wake_sweep();
        }
        Ok(SubmitTurnOutcome::Queued(Box::new(row)))
    }

    /// Promote the queue head of every idle remote session that has one.
    /// Called from the remote sweep; local sessions drain their own queues
    /// through their workers and are skipped here.
    pub(crate) async fn promote_remote_queue_heads(&self) -> Result<(), ServerError> {
        if self.remote.is_none() {
            return Ok(());
        }
        // Only sessions holding a queue can have a head to promote, so the
        // pass reads those rather than every session on the machine.
        for (owner, session_id) in
            tidebreak_core::db::code::sessions_with_queued_turns_all_owners(&self.db).await?
        {
            let Some(session) = get_session(&self.db, &owner, session_id).await? else {
                continue;
            };
            self.try_promote_remote_head(session).await?;
        }
        Ok(())
    }

    /// Promote a freshly recorded external message by the session's
    /// location (decision 0088). A sandbox session hands the head to its
    /// lease; a machine session wakes its worker, which drains the queue the
    /// way it does after any turn, and this waits briefly for the head to
    /// become a turn so the channel hears `new_turn` rather than `queued`
    /// for an idle session.
    pub(crate) async fn promote_external_head(
        &self,
        session: Session,
        turn_id: tidebreak_core::TurnId,
    ) -> Result<(), ServerError> {
        match session.execution_location {
            ExecutionLocation::Sandbox => self.promote_remote_head(session, true).await,
            ExecutionLocation::Machine => {
                let owner = session.owner.clone();
                let session_id = session.id;
                self.wake_session_queue(session_id);
                for _ in 0..MACHINE_PROMOTION_POLLS {
                    let still_queued =
                        tidebreak_core::db::code::list_queued_turns(&self.db, &owner, session_id)
                            .await?
                            .iter()
                            .any(|row| row.id == turn_id);
                    if !still_queued {
                        break;
                    }
                    tokio::time::sleep(MACHINE_PROMOTION_POLL).await;
                }
                Ok(())
            }
        }
    }

    /// Promote one idle remote session's queue head, when it has one and
    /// nothing holds promotion. Shared by the sweep and the external
    /// messages path, which tries the head immediately after enqueueing
    /// rather than waiting out a sweep tick.
    pub(super) async fn try_promote_remote_head(
        &self,
        session: Session,
    ) -> Result<(), ServerError> {
        self.promote_remote_head(session, false).await
    }

    async fn promote_remote_head(
        &self,
        session: Session,
        fresh_input: bool,
    ) -> Result<(), ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Ok(());
        };
        let _recovery_guard = self.session_recovery_lock(session.id).lock_owned().await;
        let lock = remote.promotion_lock(session.id);
        let _guard = lock.lock().await;
        let mut session = self.get_session(&session.owner, session.id).await?;
        if fresh_input {
            remote.retry_startup_failure(session.id);
        }
        if matches!(
            session.lifecycle,
            SessionLifecycle::Ended | SessionLifecycle::Fenced
        ) {
            remote.clear_startup_failure(session.id);
        }
        if session.execution_location != ExecutionLocation::Sandbox
            || session.lifecycle != SessionLifecycle::Idle
        {
            return Ok(());
        }
        let workspace = self.session_workspace(&session).await?;
        if tidebreak_core::db::code::queue_paused(&self.db, &session.owner, session.id).await? {
            return Ok(());
        }
        if remote.promotion_held(session.id) {
            return Ok(());
        }
        let Some(head) = queued_turn_head(&self.db, &session.owner, session.id).await? else {
            remote.clear_startup_failure(session.id);
            return Ok(());
        };
        let repo = match workspace.as_ref() {
            Some(workspace) => {
                let Ok(repo) = self.get_repo(&session.owner, workspace.repo_id).await else {
                    return Ok(());
                };
                Some(repo)
            }
            None => None,
        };
        let scratch_branch = workspace
            .is_none()
            .then(|| format!("scratch-{}", session.id));
        let driver = remote.driver(&self.db, self.bus.as_ref());
        let message = head.message.clone();
        use crate::code::remote::driver::RemoteTurnOutcome as Outcome;
        match driver
            .submit_turn_from(
                &mut session,
                workspace.as_ref(),
                repo.as_ref(),
                scratch_branch.as_deref(),
                &message,
                Some(&head),
            )
            .await
        {
            // The claim was the atomic promotion; nothing to delete. A
            // reincarnation has a fresh sandbox to pump, so the sweep
            // looks again now rather than at its next floor.
            Ok(Outcome::Delivered { .. }) | Ok(Outcome::Reincarnated { .. }) => {
                remote.clear_promotion_hold(session.id);
                remote.clear_startup_failure(session.id);
                remote.wake_sweep();
            }
            // Permanent for this session: nothing exposes a way to raise
            // the ceiling, and every retry would re-journal the refusal
            // and re-cancel the sandbox. Pause the queue so the tray
            // shows why nothing moves; unpausing retries deliberately.
            Ok(Outcome::SpendExhausted { .. } | Outcome::RecoveryBlocked { .. }) => {
                let _ = tidebreak_core::db::code::set_queue_paused(
                    &self.db,
                    &session.owner,
                    session.id,
                    true,
                )
                .await;
            }
            // Transient machine-side refusals: hold retries so the
            // notice and attention do not repeat every sweep tick. The
            // hold expiring retries on its own once the slot may be
            // free or the owner has signed in.
            Ok(Outcome::SignInRequired) => {
                remote.hold_promotion(session.id);
                self.note_remote_startup_failure(
                    &session,
                    Attention::needs_you("sign in to the sandbox environment", AttentionSource::Structured),
                    "Tidebreak cannot access the sandbox environment (sandbox_sign_in_required). Your message stays queued. Renew the connection in Tidebreak or ask your administrator for help, then send another message to retry.".into(),
                ).await?;
            }
            Ok(Outcome::CapExhausted { .. }) => {
                remote.clear_startup_failure(session.id);
                remote.hold_promotion(session.id);
            }
            // Busy shapes: the row stays queued for the next idle.
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    session = %session.id,
                    %error,
                    "promoting a queued remote message failed; the row stays queued"
                );
                remote.hold_promotion(session.id);
                self.note_remote_startup_failure(
                    &session,
                    Attention::needs_you(format!("Sandbox startup failed ({})", error.kind()), AttentionSource::Structured),
                    format!("Tidebreak could not start the sandbox (sandbox_start_failed:{}). Your message stays queued. Tidebreak retries automatically; send another message to retry now.", error.kind()),
                ).await?;
            }
        }
        Ok(())
    }

    /// Take one external message for a bound session
    /// (`docs/slack-sessions.md`, stage 2).
    ///
    /// Idempotent across the channel's retries: the event id commits with
    /// the queue row it causes, and a replay derives its answer from that
    /// row's current state — still queued, promoted into a turn, or
    /// retracted — without writing a second row. An idle session promotes
    /// the head immediately; a busy one queues durably.
    pub async fn external_submit_message(
        &self,
        owner: &OwnerId,
        grant_id: tidebreak_core::CodeGrantId,
        session_id: SessionId,
        message: ExternalMessage,
    ) -> Result<ExternalMessageOutcome, ServerError> {
        let ExternalMessage {
            text,
            event_id,
            channel_ts,
            actor,
            context,
            steer,
            expected_turn_id,
            correlation_uuid,
        } = message;
        if !tidebreak_core::db::code::session_bound_to_grant(&self.db, owner, session_id, grant_id)
            .await?
        {
            return Err(ServerError::conflict_kind(
                "grant_scope",
                "this grant holds no binding to that session",
            ));
        }
        let session = self.get_session(owner, session_id).await?;
        self.validated_external_delegation(owner, grant_id).await?;
        match session.lifecycle {
            SessionLifecycle::Ended => {
                return Err(ServerError::conflict_kind(
                    "session_ended",
                    "the bound session has ended; the conversation is closed",
                ));
            }
            SessionLifecycle::Fenced => {
                return Err(ServerError::conflict_kind(
                    "session_fenced",
                    "the bound session is fenced pending a reap",
                ));
            }
            _ => {}
        }
        // A sandbox admission needs a durable correlation id so its ack can
        // settle without ambiguity; mint one when the caller did not supply it.
        let correlation_uuid = if steer {
            Some(correlation_uuid.unwrap_or_else(uuid::Uuid::new_v4))
        } else {
            correlation_uuid
        };
        let record = tidebreak_core::db::code::record_external_message_with_steer(
            &self.db,
            owner,
            session_id,
            &event_id,
            &channel_ts,
            &text,
            &actor,
            context.as_ref(),
            tidebreak_core::db::code::ExternalSteerAdmissionInput {
                request_steer: steer,
                expected_turn_id,
                correlation_uuid,
            },
        )
        .await
        .map_err(|error| match error {
            tidebreak_core::db::code::ExternalMessageIntakeError::Context { kind, message } => {
                ServerError::bad_request_kind(kind, &message)
            }
            tidebreak_core::db::code::ExternalMessageIntakeError::Store(error) => error.into(),
        })?;
        use tidebreak_core::code::{ExternalSteerAdmission, ExternalSteerQueuedReason};
        let (turn_id, fresh) = match &record {
            tidebreak_core::ExternalMessageRecord::Recorded(row) => (row.id, true),
            tidebreak_core::ExternalMessageRecord::Replay { turn_id, .. } => (*turn_id, false),
        };
        // The first delivery owns the receipt metadata. A replay never steers again.
        if let tidebreak_core::ExternalMessageRecord::Replay {
            steer_requested: Some(true),
            expected_turn_id: Some(target),
            correlation_uuid: stored_correlation,
            admission,
            queued_reason,
            ..
        } = &record
        {
            return Ok(if *admission == Some(ExternalSteerAdmission::Steered) {
                ExternalMessageOutcome::Steered {
                    turn_id,
                    expected_turn_id: *target,
                    correlation_uuid: *stored_correlation,
                }
            } else {
                ExternalMessageOutcome::SteerQueued {
                    turn_id,
                    reason: queued_reason.unwrap_or(ExternalSteerQueuedReason::Unacknowledged),
                }
            });
        }
        if steer && fresh {
            let target = expected_turn_id.expect("steering target validated before recording");
            if session.execution_location == ExecutionLocation::Sandbox {
                return self
                    .steer_remote_external(
                        owner,
                        &session,
                        &event_id,
                        &text,
                        target,
                        turn_id,
                        correlation_uuid
                            .expect("steering correlation is assigned before recording"),
                    )
                    .await;
            }
            let reason = self
                .steer_external(
                    owner,
                    session_id,
                    event_id.clone(),
                    text,
                    target,
                    correlation_uuid,
                )
                .await?;
            return Ok(match reason {
                Some(reason) => ExternalMessageOutcome::SteerQueued { turn_id, reason },
                None => ExternalMessageOutcome::Steered {
                    turn_id,
                    expected_turn_id: target,
                    correlation_uuid,
                },
            });
        }
        if fresh {
            // Best effort: a refusal leaves the row queued for the sweep.
            let session = self.get_session(owner, session_id).await?;
            if let Err(error) = self.promote_external_head(session, turn_id).await {
                tracing::warn!(
                    session = %session_id,
                    ?error,
                    "promoting an external message failed; the row stays queued"
                );
            }
        }
        if let Some(row) = tidebreak_core::db::code::list_queued_turns(&self.db, owner, session_id)
            .await?
            .into_iter()
            .find(|row| row.id == turn_id)
        {
            return Ok(ExternalMessageOutcome::Queued(Box::new(row)));
        }
        if let Some(turn) = tidebreak_core::db::code::get_turn(&self.db, owner, turn_id).await? {
            return Ok(ExternalMessageOutcome::NewTurn(Box::new(turn)));
        }
        // The row the first delivery caused was retracted before it ran.
        Ok(ExternalMessageOutcome::Dropped)
    }

    /// Dispatch to one durable sandbox target. Native acknowledgment settles the receipt.
    #[allow(clippy::too_many_arguments)]
    async fn steer_remote_external(
        &self,
        owner: &OwnerId,
        session: &Session,
        event_id: &str,
        text: &str,
        expected_turn_id: tidebreak_core::TurnId,
        turn_id: tidebreak_core::TurnId,
        correlation_uuid: uuid::Uuid,
    ) -> Result<ExternalMessageOutcome, ServerError> {
        use tidebreak_core::code::supervisor_tools::{encode_steer_frame, SupervisorSteerFrame};
        use tidebreak_core::code::ExternalSteerQueuedReason;
        let queued = |reason| ExternalMessageOutcome::SteerQueued { turn_id, reason };
        if !supports_sandbox_steer(session.harness_kind) {
            self.settle_external_queued(
                owner,
                session.id,
                event_id,
                expected_turn_id,
                ExternalSteerQueuedReason::SteerUnsupported,
            )
            .await?;
            return Ok(queued(ExternalSteerQueuedReason::SteerUnsupported));
        }
        let open = tidebreak_core::db::code::get_open_turn(&self.db, owner, session.id).await?;
        let Some(open) = open.filter(|turn| turn.id == expected_turn_id) else {
            self.settle_external_queued(
                owner,
                session.id,
                event_id,
                expected_turn_id,
                ExternalSteerQueuedReason::StaleTurn,
            )
            .await?;
            return Ok(queued(ExternalSteerQueuedReason::StaleTurn));
        };
        let Some(remote) = self.remote_sessions() else {
            self.settle_external_queued(
                owner,
                session.id,
                event_id,
                expected_turn_id,
                ExternalSteerQueuedReason::SteerUnsupported,
            )
            .await?;
            return Ok(queued(ExternalSteerQueuedReason::SteerUnsupported));
        };
        let Some(runtime_id) = remote
            .driver(&self.db, self.bus.as_ref())
            .steering_runtime(owner, session.id)
            .await?
        else {
            self.settle_external_queued(
                owner,
                session.id,
                event_id,
                expected_turn_id,
                ExternalSteerQueuedReason::SteerUnsupported,
            )
            .await?;
            return Ok(queued(ExternalSteerQueuedReason::SteerUnsupported));
        };
        let incarnation =
            tidebreak_core::db::code::latest_incarnation(&self.db, owner, session.id).await?;
        let Some(incarnation) = incarnation else {
            self.settle_external_queued(
                owner,
                session.id,
                event_id,
                expected_turn_id,
                ExternalSteerQueuedReason::StaleTurn,
            )
            .await?;
            return Ok(queued(ExternalSteerQueuedReason::StaleTurn));
        };
        let target = incarnation
            .sandbox_id
            .as_deref()
            .zip(native_steer_turn(open.ordinal, incarnation.starting_turn).ok());
        let Some((sandbox_id, native_turn)) = target else {
            self.settle_external_queued(
                owner,
                session.id,
                event_id,
                expected_turn_id,
                ExternalSteerQueuedReason::StaleTurn,
            )
            .await?;
            return Ok(queued(ExternalSteerQueuedReason::StaleTurn));
        };
        let claimed = tidebreak_core::db::code::claim_external_steer_target(
            &self.db,
            owner,
            session.id,
            event_id,
            expected_turn_id,
            correlation_uuid,
            sandbox_id,
            native_turn,
            runtime_id,
        )
        .await?;
        if claimed {
            // A network error cannot prove that the inbox rejected this instruction.
            // Preserve the receipt until durable native admission evidence arrives.
            let dispatch = remote
                .driver(&self.db, self.bus.as_ref())
                .send_steer_frame(
                    owner,
                    session.id,
                    encode_steer_frame(&SupervisorSteerFrame {
                        expected_turn_id: expected_turn_id.to_string(),
                        sandbox_id: sandbox_id.to_owned(),
                        runtime_id: runtime_id.to_string(),
                        native_turn,
                        correlation_uuid: correlation_uuid.to_string(),
                        body: text.to_owned(),
                    }),
                )
                .await;
            if matches!(
                dispatch,
                Err(crate::code::remote::driver::SteerDispatchError::NotSent(_))
            ) {
                self.settle_external_queued(
                    owner,
                    session.id,
                    event_id,
                    expected_turn_id,
                    ExternalSteerQueuedReason::StaleTurn,
                )
                .await?;
                return Ok(queued(ExternalSteerQueuedReason::StaleTurn));
            }
        }
        Ok(queued(ExternalSteerQueuedReason::Unacknowledged))
    }

    pub(super) async fn interrupt_remote(&self, session: &Session) -> Result<(), ServerError> {
        let Some(remote) = self.remote_sessions() else {
            return Err(ServerError::conflict_kind(
                "remote_disabled",
                "this deployment has no sandbox runtime configured",
            ));
        };
        let Some(row) =
            tidebreak_core::db::code::latest_incarnation(&self.db, &session.owner, session.id)
                .await?
        else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        };
        if row.state != tidebreak_core::IncarnationState::Active {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        }
        let Some(sandbox_id) = row.sandbox_id.as_deref() else {
            return Err(ServerError::conflict_kind(
                "no_active_turn",
                "there is no active turn to interrupt",
            ));
        };
        let message = crate::code::remote::wire::SandboxMessage {
            body: crate::code::remote::wire::SupervisorMessageBody::Input("stop".to_owned()),
            interrupt: true,
        };
        match remote
            .provisioner
            .send(&session.owner, session.id, sandbox_id, &message)
            .await
        {
            Ok(_) => Ok(()),
            Err(crate::code::remote::RemoteSandboxError::SignInRequired(_)) => {
                Err(ServerError::conflict_kind(
                    "sign_in_required",
                    "sign in to the sandbox environment, then retry",
                ))
            }
            Err(error) => Err(ServerError::internal(error.to_string())),
        }
    }

    /// Best-effort stop of a remote session's sandbox. Used when the session
    /// row is ending, so the environment does not keep spending.
    pub(super) async fn cancel_remote_sandbox(&self, session: &Session) {
        let Some(remote) = self.remote_sessions() else {
            return;
        };
        let Ok(Some(row)) =
            tidebreak_core::db::code::latest_incarnation(&self.db, &session.owner, session.id)
                .await
        else {
            return;
        };
        if let Some(sandbox_id) = row.sandbox_id.as_deref() {
            if let Err(error) = remote
                .provisioner
                .cancel(&session.owner, session.id, sandbox_id)
                .await
            {
                tracing::warn!(
                    session = %session.id,
                    %error,
                    "could not cancel a remote sandbox while ending the session"
                );
            }
        }
        if row.state != tidebreak_core::IncarnationState::Stopped {
            let _ = tidebreak_core::db::code::stop_incarnation(
                &self.db,
                &session.owner,
                row.id,
                Some("session_ended"),
            )
            .await;
        }
    }
}

/// Whether a sandbox harness has a verified acknowledged mid-turn steering
/// channel. Codex's `turn/steer` is verified; Claude reports Unknown and
/// must never be claimed steered on a stdin write.
fn supports_sandbox_steer(harness: tidebreak_core::HarnessKind) -> bool {
    matches!(harness, tidebreak_core::HarnessKind::Codex)
}

fn native_steer_turn(ordinal: i64, starting_turn: i32) -> Result<u32, ServerError> {
    ordinal
        .checked_sub(i64::from(starting_turn))
        .and_then(|offset| offset.checked_add(1))
        .filter(|turn| *turn > 0)
        .and_then(|turn| u32::try_from(turn).ok())
        .ok_or_else(|| {
            ServerError::conflict_kind(
                "stale_turn",
                "The turn does not belong to this sandbox incarnation.",
            )
        })
}

#[cfg(test)]
mod steer_target_tests {
    use super::native_steer_turn;
    #[test]
    fn maps_session_ordinals_into_each_incarnation() {
        assert_eq!(native_steer_turn(1, 1).unwrap(), 1);
        assert_eq!(native_steer_turn(4, 4).unwrap(), 1);
        assert_eq!(native_steer_turn(5, 4).unwrap(), 2);
        assert!(native_steer_turn(3, 4).is_err());
    }
}

/// Match Gateway's sandbox bootstrap: keep an explicit granted model, otherwise
/// select the first granted route for the engine's protocol before starting work.
fn select_external_sandbox_model(
    harness: HarnessKind,
    selection: Option<&str>,
    anthropic: tidebreak_core::Result<Vec<crate::obo_gateway::GatewayCompatModel>>,
    openai: tidebreak_core::Result<Vec<crate::obo_gateway::GatewayCompatModel>>,
) -> Result<String, ServerError> {
    let models: Vec<String> = match harness {
        HarnessKind::ClaudeCode => anthropic?.into_iter().map(|row| row.id).collect(),
        HarnessKind::Codex => openai?.into_iter().map(|row| row.id).collect(),
        HarnessKind::Internal | HarnessKind::Opencode | HarnessKind::Grok => Vec::new(),
    };
    match selection {
        Some(selected) => models.into_iter().find(|model| model == selected),
        None => models.into_iter().next(),
    }
    .ok_or_else(|| {
        ServerError::conflict_kind(
            "model_provider_unavailable",
            "This Slack connection cannot use the selected harness model. Choose an available model in channel settings.",
        )
    })
}

#[cfg(test)]
mod external_sandbox_model_tests {
    use super::*;
    use crate::obo_gateway::GatewayCompatModel;

    fn models(ids: &[&str]) -> tidebreak_core::Result<Vec<GatewayCompatModel>> {
        Ok(ids
            .iter()
            .map(|id| GatewayCompatModel {
                id: (*id).to_owned(),
                display_name: None,
                family_default: false,
            })
            .collect())
    }

    #[test]
    fn defaults_and_explicit_models_use_the_engines_granted_protocol() {
        for (harness, expected) in [
            (HarnessKind::ClaudeCode, "claude-granted"),
            (HarnessKind::Codex, "gpt-granted"),
        ] {
            assert_eq!(
                select_external_sandbox_model(
                    harness,
                    None,
                    models(&["claude-granted"]),
                    models(&["gpt-granted", "gpt-selected"])
                )
                .unwrap(),
                expected
            );
        }
        assert_eq!(
            select_external_sandbox_model(
                HarnessKind::Codex,
                Some("gpt-selected"),
                Err(tidebreak_core::AgentError::msg(
                    "unused protocol is unavailable"
                )),
                models(&["gpt-granted", "gpt-selected"])
            )
            .unwrap(),
            "gpt-selected"
        );
        assert!(select_external_sandbox_model(
            HarnessKind::Codex,
            Some("native-ungranted"),
            models(&[]),
            models(&["gpt-granted"])
        )
        .is_err());
        assert!(select_external_sandbox_model(
            HarnessKind::Codex,
            None,
            models(&["claude-granted"]),
            models(&[])
        )
        .is_err());
    }
}
