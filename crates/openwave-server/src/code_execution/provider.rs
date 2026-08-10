//! Configured code-execution provider and workspace lifecycle handle.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use openwave_code_execution::{
    sync, CodeExecutionError, CodeExecutionProvider, CodeExecutionProviderKind,
    CodeExecutionRequest, CodeExecutionResponse, DaytonaCredential, E2BCredential,
    ExecFolderAccess, ExecFolderGrant, ExecutionId, ExecutionWorkspaceId, LocalExecutionProvider,
    MaterializationPrecondition, MaterializedChangeKind, OutputArtifactEntry, OutputArtifactScan,
    OutputArtifactStatus, PreviewScan, RejectedChangeReason, RemoteSessionPool, SharedPackageCache,
    StagedUpload, WorkspaceFilePath, WorkspaceLifecycle, WorkspaceListing, WriteOverlay,
    WriteSnapshotSink, PACKAGE_CACHE_DIR,
};
use openwave_core::{
    BlobStore, CallId, Chat, ChatId, ExecFileRejectionReason, ExecFileRejectionRecord, HostRootId,
    NetworkPolicy, RevisionProducer, SecretProvider, Store, TurnId,
};

use crate::exec_write_snapshot::TurnSnapshotSink;
use crate::state::BlobWriteGuard;

use super::config::*;
use super::staging::{
    implicit_staged_paths, materialize_chat_attachments, prepare_execution_directories,
    required_host_deps, staged_set_note, StagedFolders, StagedTurn,
};

pub struct ConfiguredCodeExecutionProvider {
    pub(super) store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    blobs: Option<Arc<dyn BlobStore>>,
    scratch_root: PathBuf,
    document_scripts_source: Option<PathBuf>,
    /// Built-in skills validated once at configuration and staged into every
    /// exec workspace; the prompt catalog is derived from the same load.
    skills: Arc<Vec<openwave_code_execution::LoadedSkill>>,
    /// Per-install directory of user-authored skill packages, re-read at each
    /// staging so an added or edited skill is picked up on the next turn
    /// without a restart. `None` disables user skills entirely.
    user_skills_dir: Option<PathBuf>,
    /// Built-in reusable prompts validated once at configuration. Unlike
    /// skills, nothing here reaches the model or a sandbox: a prompt is text
    /// the user inserts, so this exists only to be listed and fetched.
    prompts: Arc<Vec<openwave_code_execution::LoadedPrompt>>,
    /// Per-install directory of user-authored prompt packages, re-read on each
    /// listing so an added or edited prompt appears without a restart.
    user_prompts_dir: Option<PathBuf>,
    /// Built-in plugins validated once at configuration against the built-in
    /// skills, grouping them in the prompt catalog. Empty when no plugin tree
    /// is configured, which leaves every skill standalone.
    plugins: Arc<Vec<openwave_code_execution::LoadedPlugin>>,
    /// Per-install directory of user-authored plugin packages, re-read
    /// alongside the user skills and prompts they group so an added or edited
    /// bundle appears without a restart. `None` disables user plugins.
    user_plugins_dir: Option<PathBuf>,
    /// Where the built-in plugin tree was loaded from, retained so a bundled
    /// MCP server can be launched with its package root as `PLUGIN_ROOT`. The
    /// packages themselves are already loaded; this is only the path they came
    /// from.
    plugins_dir: Option<PathBuf>,
    /// Public-HTTPS archive fetcher used only by explicit plugin installs.
    /// Injectable in route tests so the contract is driven without live
    /// internet access.
    plugin_archive_fetcher: Arc<dyn crate::plugin_install::PluginArchiveFetcher>,
    /// Serializes filesystem publication and the merged-catalog conflict
    /// check, so two installs cannot both pass the same preflight.
    plugin_install_lock: tokio::sync::Mutex<()>,
    folder_grant_resolver: Option<Arc<dyn ExecFolderGrantResolver>>,
    /// Host-provided office-to-PDF converter feeding the model's visual QA
    /// loop. `None` (headless embeddings) degrades to an honest sync note.
    office_converter: Option<Arc<dyn openwave_code_execution::HostOfficeConverter>>,
    /// Broker that provides skill-declared host tools (the managed
    /// LibreOffice install, on the desktop). `None` reports every tool
    /// unavailable and warms nothing.
    host_tool_broker: Option<Arc<dyn openwave_code_execution::HostToolBroker>>,
    /// Cross-process exclusion for the blobs a write-back snapshot publishes.
    blob_writes: Option<Arc<BlobWriteGuard>>,
    remote_sessions: RemoteSessionPool,
    /// The write overlay each chat's current turn is staging into.
    ///
    /// A turn opens one entry when it resolves its folder grants and closes it
    /// when the turn ends; every `exec` in between finds it here and points the
    /// sandbox at the staged copy instead of the user's folder.
    write_overlays: Mutex<HashMap<ChatId, StagedTurn>>,
    /// The shared package cache's runtime key, probed from the sandbox
    /// interpreter once per process. `None` disables the cache.
    package_cache_runtime: tokio::sync::OnceCell<Option<String>>,
    /// Whether a host-side cache population pass is running or has succeeded;
    /// cleared again on failure so a later exec can retry.
    package_cache_population: Arc<std::sync::atomic::AtomicBool>,
    /// Serializes cache population itself, so the exec-time pass and a
    /// provisioning pass triggered by boot or a plugin enable cannot drive pip
    /// against the same cache at once.
    package_cache_lock: Arc<tokio::sync::Mutex<()>>,
    /// Where live sandbox-preparation progress is announced. Installed after
    /// the app's state is assembled, because the bus is built there; execution
    /// works exactly as before until it is.
    events: OnceLock<Arc<crate::bus::EventBus>>,
    /// Chats already told that execution is running degraded.
    ///
    /// The providers latch the degradation per instance, and an instance is
    /// built per execution — so without this the same warning would land on
    /// every card that recreates a sandbox. Warning once per chat is what a
    /// reader needs: the second card says nothing new.
    degradation_reported: Mutex<HashSet<ChatId>>,
}

/// Publishes a provider's first-run image preparation to the chat that is
/// waiting on it.
struct SandboxPreparationNotices {
    events: Arc<crate::bus::EventBus>,
}

impl openwave_code_execution::SandboxPreparationSink for SandboxPreparationNotices {
    fn report(&self, workspace_id: &str, stage: openwave_code_execution::SandboxPreparation) {
        // The workspace identity is the chat's; a workspace that does not name
        // one has no window to tell, and the execution itself proceeds.
        let Ok(chat) = workspace_id.parse::<ChatId>() else {
            return;
        };
        self.events.publish_metadata(
            chat,
            crate::bus::ChatMetadataNotice::SandboxPreparing {
                preparing: matches!(stage, openwave_code_execution::SandboxPreparation::Started),
            },
        );
    }
}

#[async_trait]
impl StagedFolders for ConfiguredCodeExecutionProvider {
    fn staged_root(&self, chat: ChatId, root_id: HostRootId) -> Option<PathBuf> {
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .get(&chat)?
            .staged_roots
            .get(&root_id)
            .cloned()
    }

    async fn materialize_connected_file(
        &self,
        chat: ChatId,
        turn: TurnId,
        root_id: HostRootId,
        relative: &str,
        content: &[u8],
        expected: MaterializationPrecondition,
    ) -> std::result::Result<MaterializedChangeKind, RejectedChangeReason> {
        let folder = self.writable_connected_root(chat, root_id).await?;
        let snapshots =
            self.blobs
                .as_ref()
                .zip(self.blob_writes.as_ref())
                .map(|(blobs, blob_writes)| {
                    TurnSnapshotSink::new(self.store.clone(), blobs.clone(), blob_writes.clone())
                });
        let result = openwave_code_execution::materialize_file(
            &folder,
            relative,
            content,
            expected,
            snapshots
                .as_ref()
                .map(|sink| sink as &dyn WriteSnapshotSink),
        )
        .await;
        if result.is_ok() {
            if let Some(sink) = snapshots {
                if let Err(error) = sink.commit(chat, turn).await {
                    tracing::error!(
                        chat = %chat,
                        turn = %turn,
                        %error,
                        "could not journal a connected-folder publication; undo is unavailable"
                    );
                }
            }
        }
        result
    }

    async fn connected_file_matches(
        &self,
        chat: ChatId,
        root_id: HostRootId,
        relative: &str,
        byte_len: u64,
        sha256: [u8; 32],
    ) -> bool {
        let Ok(folder) = self.writable_connected_root(chat, root_id).await else {
            return false;
        };
        openwave_code_execution::materialized_file_matches(&folder, relative, byte_len, sha256)
            .await
    }
}

impl ConfiguredCodeExecutionProvider {
    #[must_use]
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        scratch_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            store,
            secrets,
            blobs: None,
            scratch_root: scratch_root.into(),
            document_scripts_source: None,
            skills: Arc::new(Vec::new()),
            user_skills_dir: None,
            prompts: Arc::new(Vec::new()),
            user_prompts_dir: None,
            plugins: Arc::new(Vec::new()),
            user_plugins_dir: None,
            plugins_dir: None,
            plugin_archive_fetcher: crate::plugin_install::default_fetcher(),
            plugin_install_lock: tokio::sync::Mutex::new(()),
            folder_grant_resolver: None,
            office_converter: None,
            host_tool_broker: None,
            blob_writes: None,
            remote_sessions: RemoteSessionPool::default(),
            write_overlays: Mutex::new(HashMap::new()),
            package_cache_runtime: tokio::sync::OnceCell::new(),
            package_cache_population: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            package_cache_lock: Arc::new(tokio::sync::Mutex::new(())),
            events: OnceLock::new(),
            degradation_reported: Mutex::new(HashSet::new()),
        }
    }

    /// Install the chat event bus this provider announces sandbox preparation
    /// on. Called once, after the app state that owns the bus is assembled.
    pub fn attach_event_bus(&self, events: Arc<crate::bus::EventBus>) {
        let _ = self.events.set(events);
    }

    fn preparation_sink(&self) -> Option<Arc<dyn openwave_code_execution::SandboxPreparationSink>> {
        let events = self.events.get()?.clone();
        Some(Arc::new(SandboxPreparationNotices { events }))
    }

    /// Install the blob lifecycle lock the write-back snapshot publishes under.
    ///
    /// Without it — and without a blob store — staged writes are applied with no
    /// snapshot, which is the behavior granted folders had before this existed.
    #[must_use]
    pub(crate) fn with_blob_write_locks(mut self, blob_writes: Arc<BlobWriteGuard>) -> Self {
        self.blob_writes = Some(blob_writes);
        self
    }

    /// Install the blob store used to backfill attached documents before exec.
    #[must_use]
    pub fn with_blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// Install a trusted bundled helper directory into every exec workspace.
    #[must_use]
    pub fn with_document_scripts(mut self, source: Option<PathBuf>) -> Self {
        self.document_scripts_source = source;
        self
    }

    /// Load and install the built-in skill packages staged into every exec
    /// workspace. Malformed packages are skipped (with a warning) at this one
    /// load, so staging and the prompt catalog always agree. Headless
    /// embeddings leave the source absent.
    #[must_use]
    pub fn with_skills(mut self, source: Option<PathBuf>) -> Self {
        self.skills = Arc::new(
            source
                .as_deref()
                .map(|source| {
                    openwave_code_execution::load_skills(
                        source,
                        openwave_code_execution::SkillOrigin::Builtin,
                    )
                })
                .unwrap_or_default(),
        );
        self
    }

    /// Load and install the built-in reusable prompt packages. Call before
    /// [`Self::with_plugins`]: a bundle's `prompts:` members are resolved
    /// against the prompts already loaded.
    #[must_use]
    pub fn with_prompts(mut self, source: Option<PathBuf>) -> Self {
        self.prompts = Arc::new(
            source
                .as_deref()
                .map(|source| {
                    openwave_code_execution::load_prompts(
                        source,
                        openwave_code_execution::PromptOrigin::Builtin,
                    )
                })
                .unwrap_or_default(),
        );
        self
    }

    /// Install the per-install directory user-authored prompt packages are
    /// loaded from. The directory is created here (best effort) so the user
    /// has a place to drop a prompt; its contents are re-read on each listing.
    #[must_use]
    pub fn with_user_prompts(mut self, source: Option<PathBuf>) -> Self {
        if let Some(source) = source.as_deref() {
            if let Err(error) = std::fs::create_dir_all(source) {
                tracing::warn!(
                    "user prompts directory {} could not be created: {error}",
                    source.display()
                );
            }
        }
        self.user_prompts_dir = source;
        self
    }

    /// Load the built-in plugins that group the built-in skills in the prompt
    /// catalog. Call after [`Self::with_skills`]: membership is resolved
    /// against the skills already loaded, and a plugin naming one that is not
    /// there is skipped (with a warning) rather than grouping nothing.
    #[must_use]
    pub fn with_plugins(mut self, source: Option<PathBuf>) -> Self {
        self.plugins = Arc::new(
            source
                .as_deref()
                .map(|source| {
                    openwave_code_execution::load_plugins(
                        source,
                        &self.skills,
                        &self.prompts,
                        openwave_code_execution::PluginOrigin::Builtin,
                    )
                })
                .unwrap_or_default(),
        );
        self.plugins_dir = source;
        self
    }

    /// Install the per-install directory user-authored plugin packages are
    /// loaded from. The directory is created here (best effort) so the user has
    /// a place to drop a bundle; its contents are re-read on each listing,
    /// against the user skills and prompts a bundle may claim.
    #[must_use]
    pub fn with_user_plugins(mut self, source: Option<PathBuf>) -> Self {
        if let Some(source) = source.as_deref() {
            if let Err(error) = std::fs::create_dir_all(source) {
                tracing::warn!(
                    "user plugins directory {} could not be created: {error}",
                    source.display()
                );
            }
        }
        self.user_plugins_dir = source;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_plugin_archive_fetcher(
        mut self,
        fetcher: Arc<dyn crate::plugin_install::PluginArchiveFetcher>,
    ) -> Self {
        self.plugin_archive_fetcher = fetcher;
        self
    }

    /// Install the per-install directory user-authored skill packages are
    /// loaded from. The directory is created here (best effort) so the user
    /// has a place to drop a skill; its contents are re-read at each staging,
    /// so a new or edited skill takes effect on the next turn.
    #[must_use]
    pub fn with_user_skills(mut self, source: Option<PathBuf>) -> Self {
        if let Some(source) = source.as_deref() {
            if let Err(error) = std::fs::create_dir_all(source) {
                tracing::warn!(
                    "user skills directory {} could not be created: {error}",
                    source.display()
                );
            }
        }
        self.user_skills_dir = source;
        self
    }

    /// Every installed skill, before the install's enable flags apply: the
    /// built-in packages merged with a fresh read of the user skills
    /// directory. Built-ins were validated once at configuration; user
    /// packages go through the same strict loader here. The read is a handful
    /// of small files at most.
    ///
    /// This is what a management surface lists — it has to show a disabled
    /// component in order to offer turning it back on. Everything the model
    /// or a sandbox sees goes through [`Self::current_skills`] instead.
    pub(crate) fn installed_skills(&self) -> Vec<openwave_code_execution::LoadedSkill> {
        openwave_code_execution::merged_skills(&self.skills, self.user_skills_dir.as_deref())
    }

    /// Every installed bundle, before the install's enable flags apply: the
    /// built-in bundles merged with a fresh read of the user plugins
    /// directory, exactly as [`Self::installed_skills`] does.
    pub(crate) fn installed_plugins(&self) -> Vec<openwave_code_execution::PluginPackage> {
        self.merged_plugins(&self.installed_skills(), &self.installed_prompts())
    }

    /// Every installed bundle that ships a bundled MCP configuration, paired
    /// with the absolute package root its servers are launched from.
    ///
    /// The root is resolved from the tree the package was loaded from and then
    /// canonicalized, because `PLUGIN_ROOT` is specified as the *resolved*
    /// plugin root and every containment check downstream compares against it.
    /// A bundle whose root will not canonicalize (removed between the listing
    /// and this call) is dropped rather than launched from a path that no
    /// longer means what the loader read.
    pub(crate) fn installed_plugin_mcp(
        &self,
    ) -> Vec<(
        openwave_code_execution::PluginPackage,
        PathBuf,
        openwave_code_execution::PluginMcpConfig,
    )> {
        self.installed_plugins()
            .into_iter()
            .filter(|package| package.mcp_servers > 0)
            .filter_map(|package| {
                let tree = match package.origin {
                    openwave_code_execution::PluginOrigin::Builtin => self.plugins_dir.as_deref(),
                    openwave_code_execution::PluginOrigin::User => self.user_plugins_dir.as_deref(),
                }?;
                let root = std::fs::canonicalize(tree.join(&package.name)).ok()?;
                let config = openwave_code_execution::load_plugin_mcp_config(&root)?;
                Some((package, root, config))
            })
            .collect()
    }

    /// Fetch, validate, and publish one instruction-only plugin, then ask the
    /// ordinary merged loaders whether it survived the existing reserved-name
    /// and ownership rules. A conflict rolls back only the directories this
    /// call just created.
    pub(crate) async fn install_plugin(
        &self,
        request: &crate::plugin_install::PluginInstallRequest,
    ) -> std::result::Result<
        crate::plugin_install::PluginInstallOutcome,
        crate::plugin_install::PluginInstallError,
    > {
        let _guard = self.plugin_install_lock.lock().await;
        let plugins_root = self.user_plugins_dir.as_deref().ok_or_else(|| {
            crate::plugin_install::PluginInstallError::Conflict(
                "plugin installation is not configured".to_owned(),
            )
        })?;
        let skills_root = self.user_skills_dir.as_deref().ok_or_else(|| {
            crate::plugin_install::PluginInstallError::Conflict(
                "plugin skill installation is not configured".to_owned(),
            )
        })?;
        let source = crate::plugin_install::resolve_source(&request.source)?;
        let archive = self
            .plugin_archive_fetcher
            .fetch(&source.archive_url)
            .await?;
        let prepared = crate::plugin_install::prepare_plugin(&archive, &source)?;
        let before = self
            .installed_plugins()
            .into_iter()
            .map(|plugin| plugin.name)
            .collect::<HashSet<_>>();
        let files = crate::plugin_install::install_prepared(&prepared, plugins_root, skills_root)?;
        let after = self.installed_plugins();
        let accepted = after.iter().any(|plugin| {
            plugin.name == prepared.package.name
                && plugin.origin == openwave_code_execution::PluginOrigin::User
                && plugin.compatibility == prepared.stamp.compatibility
        });
        let after_names = after
            .iter()
            .map(|plugin| plugin.name.as_str())
            .collect::<HashSet<_>>();
        if !accepted
            || before
                .iter()
                .any(|existing| !after_names.contains(existing.as_str()))
        {
            crate::plugin_install::rollback_install(&files);
            return Err(crate::plugin_install::PluginInstallError::Conflict(
                "a built-in or installed plugin owns this name or one of its members".to_owned(),
            ));
        }
        Ok(crate::plugin_install::PluginInstallOutcome {
            plugin: prepared.package.name,
            revision: source.revision,
            compatibility: prepared.stamp.compatibility,
            skipped: prepared.skipped,
        })
    }

    /// The same merge against sets the caller has already read.
    ///
    /// Membership is resolved against the *installed* skills and prompts, not
    /// the enabled ones: a bundle whose member the user switched off is still
    /// the bundle they installed, and dropping it would take the switch that
    /// turns the member back on off the surface with it.
    fn merged_plugins(
        &self,
        skills: &[openwave_code_execution::LoadedSkill],
        prompts: &[openwave_code_execution::LoadedPrompt],
    ) -> Vec<openwave_code_execution::PluginPackage> {
        openwave_code_execution::merged_plugins(
            &self.plugins,
            self.user_plugins_dir.as_deref(),
            skills,
            prompts,
        )
        .into_iter()
        .map(|plugin| plugin.package)
        .collect()
    }

    /// Every installed prompt, before the install's enable flags apply: the
    /// built-in packages merged with a fresh read of the user prompts
    /// directory, exactly as [`Self::installed_skills`] does.
    ///
    /// There is no filtered counterpart. A prompt is never staged and never
    /// advertised, so this one listing is the whole consumer surface.
    pub(crate) fn installed_prompts(&self) -> Vec<openwave_code_execution::LoadedPrompt> {
        openwave_code_execution::merged_prompts(&self.prompts, self.user_prompts_dir.as_deref())
    }

    /// The bundle that claims `skill`, if any.
    fn owning_plugin<'a>(
        plugins: &'a [openwave_code_execution::PluginPackage],
        skill: &str,
    ) -> Option<&'a str> {
        plugins
            .iter()
            .find(|plugin| plugin.skills.iter().any(|member| member == skill))
            .map(|plugin| plugin.name.as_str())
    }

    /// The install's plugin and skill enable flags.
    pub(crate) async fn enable_state(&self) -> crate::plugin_state::PluginEnableState {
        crate::plugin_state::read_plugin_enable_state(&*self.store).await
    }

    /// The skills this install actually runs: installed, minus anything the
    /// user switched off — either directly or by disabling the bundle that
    /// claims it.
    ///
    /// One filtered read backs both consumers, so a disabled skill is absent
    /// from the staged workspace *and* from the prompt catalog; the model is
    /// never told about instructions the sandbox will not have.
    async fn current_skills(&self) -> Vec<openwave_code_execution::LoadedSkill> {
        self.enabled_skills(self.installed_skills()).await
    }

    /// Narrow an installed set to what the install's flags leave on.
    ///
    /// Ownership is resolved against the merged bundles, so a user bundle
    /// gates the user skills it claims exactly as a built-in one does.
    async fn enabled_skills(
        &self,
        installed: Vec<openwave_code_execution::LoadedSkill>,
    ) -> Vec<openwave_code_execution::LoadedSkill> {
        let state = self.enable_state().await;
        let plugins = self.merged_plugins(&installed, &self.installed_prompts());
        installed
            .into_iter()
            .filter(|skill| {
                state.skill_enabled(
                    &skill.package.name,
                    Self::owning_plugin(&plugins, &skill.package.name),
                )
            })
            .collect()
    }

    /// The host-derived (name, description) catalog for prompt composition.
    pub(crate) async fn skill_catalog(&self) -> Vec<openwave_code_execution::SkillPackage> {
        self.current_skills()
            .await
            .into_iter()
            .map(|skill| skill.package)
            .collect()
    }

    /// The bundles the catalog groups skills under, built-in and user-authored
    /// alike. A skill no bundle claims stays standalone.
    pub(crate) async fn plugin_catalog(&self) -> Vec<openwave_code_execution::PluginPackage> {
        let state = self.enable_state().await;
        self.installed_plugins()
            .into_iter()
            .filter(|plugin| state.plugin_enabled(&plugin.name))
            .collect()
    }

    /// Stage the chat's workspace at turn start, before the model runs.
    ///
    /// The operating prompt tells the model to `read_file` a skill's
    /// `SKILL.md` *before* producing that kind of document, so the staging
    /// that `execute` performs on the first command comes strictly too late:
    /// the read races ahead of any exec and finds nothing. This runs the same
    /// idempotent preparation when the turn surface is composed. Best-effort
    /// on purpose — prompt enrichment is not an authority boundary, and
    /// `execute` re-prepares (with the provider-correct mirroring flag)
    /// before any command runs.
    pub(crate) async fn stage_turn_workspace(&self, chat_id: ChatId) {
        // A configuration with no skills at all — a headless embedding — has
        // no workspace to prepare. An install that has *disabled* every skill
        // is a different case: a workspace may already hold staged copies, and
        // the pass below is what takes them back out.
        let installed = self.installed_skills();
        if installed.is_empty() {
            return;
        }
        let skills = self.enabled_skills(installed).await;
        // Warm the staged skills' host tools while the turn runs: `ensure` is
        // fire-and-forget with the broker's own discipline (serialized
        // installs, remembered failures), so a staged manifest is all it takes
        // to start the managed install long before the QA loop needs it.
        if let Some(broker) = self.host_tool_broker.as_deref() {
            for dep in required_host_deps(&skills) {
                broker.ensure(dep);
            }
        }
        let host_dir = self.scratch_root.join(chat_id.to_string());
        if let Err(error) = prepare_execution_directories(
            &host_dir,
            false,
            self.document_scripts_source.as_deref(),
            &skills,
        )
        .await
        {
            tracing::warn!("turn-start workspace staging failed for chat {chat_id}: {error}");
        }
    }

    /// Whether host-side office rendering is real for this turn, for the
    /// operating prompt's capability line.
    ///
    /// `None` when no staged skill declares a LibreOffice dependency — the
    /// line would steer nothing and is omitted. Otherwise the broker's status
    /// is the truth: only a tool that resolves right now counts, so a prompt
    /// never promises a converter that is mid-download or failed to install.
    pub(crate) async fn office_rendering_available(&self) -> Option<bool> {
        let declared = self.current_skills().await.iter().any(|skill| {
            skill
                .package
                .host_deps
                .contains(&openwave_code_execution::HostDep::LibreOffice)
        });
        if !declared {
            return None;
        }
        let Some(broker) = self.host_tool_broker.as_deref() else {
            return Some(false);
        };
        Some(matches!(
            broker
                .status(openwave_code_execution::HostDep::LibreOffice)
                .await,
            openwave_code_execution::HostToolStatus::Available
        ))
    }

    /// Whether a Node runtime is real for this turn, for the operating
    /// prompt's capability line.
    ///
    /// `None` when no staged skill declares npm packages — nothing this turn
    /// would run Node, so the line would steer nothing. Otherwise the status
    /// is stated exactly, including [`HostToolStatus::Installing`]: a runtime
    /// that is still downloading is worth waiting a step for, and a model told
    /// only "unavailable" would abandon the npm path for the rest of the turn.
    ///
    /// Every managed backend bakes Node into its image, so only the local
    /// backend has anything to resolve; there, a host with no broker at all
    /// (a headless embedding) reports the honest absence rather than letting
    /// the model discover it one failed command at a time.
    pub(crate) async fn node_runtime_status(
        &self,
    ) -> Option<openwave_code_execution::HostToolStatus> {
        let needed = self
            .current_skills()
            .await
            .iter()
            .any(|skill| !skill.package.npm_deps.is_empty());
        if !needed {
            return None;
        }
        let provider = read_config(&*self.store)
            .await
            .ok()
            .and_then(|c| c.provider);
        if !matches!(provider, None | Some(CodeExecutionProviderKind::Local)) {
            return Some(openwave_code_execution::HostToolStatus::Available);
        }
        let Some(broker) = self.host_tool_broker.as_deref() else {
            return Some(openwave_code_execution::HostToolStatus::Unavailable(
                "this host provides no managed Node runtime".into(),
            ));
        };
        Some(broker.status(openwave_code_execution::HostDep::Node).await)
    }

    /// The managed Node runtime's directory, for the local sandbox to expose
    /// read-only. `None` whenever one does not resolve right now, which is
    /// simply a sandbox without `node` on its `PATH`.
    async fn managed_node_dir(&self) -> Option<std::path::PathBuf> {
        self.host_tool_broker
            .as_deref()?
            .managed_root(openwave_code_execution::HostDep::Node)
            .await
    }

    /// The shared package cache keyspace for the local sandbox interpreter.
    ///
    /// The wheel-compatibility runtime key is probed from the same interpreter
    /// the sandbox runs, once per process; `None` (an unusable interpreter, a
    /// non-macOS host, or an unopenable cache directory) disables the cache
    /// without affecting execution.
    async fn shared_package_cache(&self) -> Option<SharedPackageCache> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        let key = self
            .package_cache_runtime
            .get_or_init(|| async {
                SharedPackageCache::runtime_key(std::path::Path::new(SANDBOX_PYTHON)).await
            })
            .await
            .clone()?;
        SharedPackageCache::open(&self.scratch_root.join(PACKAGE_CACHE_DIR), &key).ok()
    }

    /// Whether verified offline package installs are currently possible on the
    /// selected provider, for truthful operating-prompt steering.
    pub(crate) async fn offline_package_cache_ready(&self) -> bool {
        let Ok(config) = read_config(&*self.store).await else {
            return false;
        };
        if config.provider != Some(CodeExecutionProviderKind::Local) {
            return false;
        }
        match self.shared_package_cache().await {
            Some(cache) => cache.is_ready(),
            None => false,
        }
    }

    /// The host-configured per-command time limit, for truthful
    /// operating-prompt steering. Execution re-reads the setting per
    /// invocation; this is the same value rendered ahead of time so the model
    /// can plan long-running commands around it.
    pub(crate) async fn current_timeout_ms(&self) -> u64 {
        match read_config(&*self.store).await {
            Ok(config) => config.timeout_ms,
            Err(_) => DEFAULT_TIMEOUT_MS,
        }
    }

    /// Best-effort host-side acquisition of the baseline set and the built-in
    /// skills' pinned dependencies, spawned once per process when a networked
    /// local exec shows the cache could be used. Failure clears the latch so a
    /// later exec retries; conversations keep their network install path
    /// either way. User-authored skills are deliberately excluded: the pass
    /// runs once, user pins change under it, and their installs use the
    /// ordinary networked path like any other package.
    fn spawn_package_cache_population(&self, cache: SharedPackageCache) {
        use std::sync::atomic::Ordering;
        let pin_sets = package_cache_pin_sets(self.skills.iter());
        if pin_sets.is_empty() {
            return;
        }
        if self.package_cache_population.swap(true, Ordering::SeqCst) {
            return;
        }
        let latch = self.package_cache_population.clone();
        let lock = self.package_cache_lock.clone();
        tokio::spawn(async move {
            if !populate_package_cache(&lock, &cache, pin_sets).await {
                latch.store(false, Ordering::SeqCst);
            }
        });
    }

    /// Start making the install's enabled dependencies real, in the
    /// background.
    ///
    /// Enabling a plugin is the user saying they want what it does; the host
    /// tools and pinned packages its skills need should start arriving then,
    /// not on the first turn that reaches for them. The pass is deterministic
    /// and host-side — no model drives it — and it returns immediately: every
    /// piece of work behind it is already fire-and-forget or bounded by the
    /// broker's own discipline (serialized installs, a remembered failure only
    /// an explicit retry clears).
    pub(crate) fn spawn_dependency_provisioning(self: &Arc<Self>) {
        let provider = self.clone();
        tokio::spawn(async move { provider.provision_dependencies().await });
    }

    async fn provision_dependencies(&self) {
        let installed = self.installed_skills();
        if installed.is_empty() {
            return;
        }
        let skills = self.enabled_skills(installed).await;
        if let Some(broker) = self.host_tool_broker.as_deref() {
            for dep in required_host_deps(&skills) {
                broker.ensure(dep);
            }
        }
        // Unlike the exec-time pass, no conversation is involved, so there is
        // no network policy to read: this is a host-side acquisition like the
        // managed LibreOffice download, wanted by the same enablement that
        // asked for the skills. Enabled user packages are included for the
        // same reason — the pins the user switched on are the pins to warm.
        let Ok(config) = read_config(&*self.store).await else {
            return;
        };
        if config.provider != Some(CodeExecutionProviderKind::Local) {
            return;
        }
        let Some(cache) = self.shared_package_cache().await else {
            return;
        };
        let pin_sets = package_cache_pin_sets(skills.iter());
        if pin_sets.is_empty() {
            return;
        }
        populate_package_cache(&self.package_cache_lock, &cache, pin_sets).await;
    }

    /// The skills this install would run right now under `state`: installed,
    /// minus anything that flag set switches off directly or through the
    /// bundle that claims it.
    ///
    /// Exposed for the enable route, which compares this across the write it
    /// just made: enabling a *bundle* moves no skill flag, so only liveness
    /// shows what came on.
    pub(crate) fn live_skill_names(
        &self,
        state: &crate::plugin_state::PluginEnableState,
    ) -> HashSet<String> {
        let installed = self.installed_skills();
        let plugins = self.merged_plugins(&installed, &self.installed_prompts());
        installed
            .into_iter()
            .filter(|skill| {
                state.skill_enabled(
                    &skill.package.name,
                    Self::owning_plugin(&plugins, &skill.package.name),
                )
            })
            .map(|skill| skill.package.name)
            .collect()
    }

    /// Install the native bridge that resolves product root IDs through the
    /// live host broker. Non-desktop embeddings leave this absent.
    #[must_use]
    pub fn with_folder_grant_resolver(
        mut self,
        resolver: Option<Arc<dyn ExecFolderGrantResolver>>,
    ) -> Self {
        self.folder_grant_resolver = resolver;
        self
    }

    /// Install the host office-to-PDF converter that renders office outputs
    /// for the model's visual QA loop. Non-desktop embeddings leave this
    /// absent and the render pass degrades to an honest note.
    #[must_use]
    pub fn with_office_converter(
        mut self,
        converter: Option<Arc<dyn openwave_code_execution::HostOfficeConverter>>,
    ) -> Self {
        self.office_converter = converter;
        self
    }

    /// Install the broker that provides skill-declared host tools. Turn
    /// staging warms declared dependencies through it, and the operating
    /// prompt's office-rendering capability line reads its status. Non-desktop
    /// embeddings leave this absent.
    #[must_use]
    pub fn with_host_tool_broker(
        mut self,
        broker: Option<Arc<dyn openwave_code_execution::HostToolBroker>>,
    ) -> Self {
        self.host_tool_broker = broker;
        self
    }

    /// Resolve the local-exec roots visible in one turn's operating prompt.
    ///
    /// Managed providers cannot mount host folders, so they deliberately
    /// receive an empty list. The execution boundary resolves again on every
    /// invocation so a revocation after the prompt snapshot still fails closed.
    pub(crate) async fn folder_grants_for_chat(
        &self,
        chat: &Chat,
        turn: TurnId,
    ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, CodeExecutionError> {
        let config = read_config(&*self.store).await.map_err(|_| {
            CodeExecutionError::Unavailable("configuration storage is unavailable".into())
        })?;
        if config.provider != Some(CodeExecutionProviderKind::Local) || !cfg!(target_os = "macos") {
            return Ok(Vec::new());
        }
        let mut grants = self.resolve_chat_folder_grants(chat).await?;
        self.open_write_overlay(chat.id, turn, &mut grants).await;
        Ok(grants)
    }

    /// Stage this turn's writes for every writable granted folder.
    ///
    /// Called once, when the turn resolves the grants it will show the model.
    /// A folder that cannot be staged is downgraded to read-only for this turn:
    /// silently restoring direct writes would remove the overlay precisely for
    /// the largest or most unusual folders.
    pub(super) async fn open_write_overlay(
        &self,
        chat: ChatId,
        turn: TurnId,
        grants: &mut [ResolvedExecFolderGrant],
    ) {
        let _ = self.close_write_overlay(chat).await;
        let writable = grants
            .iter()
            .filter(|grant| grant.writable)
            .map(|grant| grant.path.clone())
            .collect::<Vec<_>>();
        let scope = chat.to_string();
        let Some(overlay) = WriteOverlay::prepare(&self.scratch_root, &scope, &writable).await
        else {
            for grant in grants.iter_mut().filter(|grant| grant.writable) {
                grant.writable = false;
                grant.staging_unavailable = true;
            }
            return;
        };
        let mut staged_roots = HashMap::new();
        for slot in overlay.slots() {
            for grant in grants
                .iter_mut()
                .filter(|grant| grant.path == slot.source())
            {
                grant.overlay = Some(slot.overlay().to_path_buf());
                staged_roots.insert(grant.root_id, slot.overlay().to_path_buf());
            }
        }
        for grant in grants
            .iter_mut()
            .filter(|grant| grant.writable && grant.overlay.is_none())
        {
            grant.writable = false;
            grant.staging_unavailable = true;
        }
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .insert(
                chat,
                StagedTurn {
                    turn,
                    overlay,
                    staged_roots,
                },
            );
    }

    /// Apply this turn's staged writes to the user's folders and end staging.
    ///
    /// Every file the write-back replaces has its prior bytes retained first
    /// and journaled against `turn`, so the change summary and undo have
    /// something to work from. A turn that never staged anything finds nothing
    /// to do. A turn that is abandoned rather than finished never reaches here,
    /// and its staged writes are discarded when the next turn sweeps them:
    /// applying them later would write a folder that has since moved on.
    pub(crate) async fn close_write_overlay(&self, chat: ChatId) -> Option<TurnId> {
        let staged = self
            .write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .remove(&chat);
        let StagedTurn { turn, overlay, .. } = staged?;
        let snapshots =
            self.blobs
                .as_ref()
                .zip(self.blob_writes.as_ref())
                .map(|(blobs, blob_writes)| {
                    TurnSnapshotSink::new(self.store.clone(), blobs.clone(), blob_writes.clone())
                });
        let outcome = overlay
            .materialize(
                snapshots
                    .as_ref()
                    .map(|sink| sink as &dyn WriteSnapshotSink),
            )
            .await;
        let has_changes = !outcome.written.is_empty() || !outcome.rejected.is_empty();
        // The journal commits after the folders are written, not before: the
        // bytes it points at are already published, and a row for a write that
        // was refused would offer an undo for a change that never happened.
        if let Some(sink) = snapshots {
            if let Err(error) = sink.commit(chat, turn).await {
                tracing::error!(
                    chat = %chat,
                    turn = %turn,
                    %error,
                    "could not journal this turn's changes to granted folders; undo is unavailable for them"
                );
            }
        }
        let rejected = outcome
            .rejected
            .iter()
            .map(|file| ExecFileRejectionRecord {
                folder_path: file.folder.display().to_string(),
                relative_path: file.relative.clone(),
                reason: match file.reason {
                    RejectedChangeReason::Stale => ExecFileRejectionReason::Stale,
                    RejectedChangeReason::SnapshotUnavailable => {
                        ExecFileRejectionReason::SnapshotUnavailable
                    }
                    RejectedChangeReason::StagedFileTooLarge => {
                        ExecFileRejectionReason::StagedFileTooLarge
                    }
                    RejectedChangeReason::TrashUnavailable => {
                        ExecFileRejectionReason::TrashUnavailable
                    }
                    RejectedChangeReason::Unavailable => ExecFileRejectionReason::Unavailable,
                },
            })
            .collect::<Vec<_>>();
        if let Err(error) = self
            .store
            .record_exec_file_rejections(chat, turn, &rejected)
            .await
        {
            tracing::error!(
                chat = %chat,
                turn = %turn,
                %error,
                "could not journal this turn's rejected staged files"
            );
        }
        if !outcome.written.is_empty() || !outcome.rejected.is_empty() {
            let deleted = outcome
                .written
                .iter()
                .filter(|file| {
                    file.change == openwave_code_execution::MaterializedChangeKind::Deleted
                })
                .count();
            tracing::info!(
                chat = %chat,
                turn = %turn,
                written = outcome.written.len().saturating_sub(deleted),
                deleted,
                rejected = outcome.rejected.len(),
                "applied staged exec writes to granted folders"
            );
        }
        has_changes.then_some(turn)
    }

    /// The folder-to-overlay pairs this chat's current turn is staging.
    ///
    /// Only the paths leave the lock. The overlay itself is owned by the
    /// registry for exactly the length of the turn, so nothing an in-flight
    /// execution holds can keep it alive past the point where its writes are
    /// applied.
    fn staged_folders(&self, chat: ChatId) -> HashMap<PathBuf, PathBuf> {
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .get(&chat)
            .map(|staged| {
                staged
                    .overlay
                    .slots()
                    .iter()
                    .map(|slot| (slot.source().to_path_buf(), slot.overlay().to_path_buf()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn overlay_inspector(&self, chat: ChatId) -> Option<openwave_code_execution::OverlayInspector> {
        self.write_overlays
            .lock()
            .expect("write overlay registry is not poisoned")
            .get(&chat)
            .map(|staged| staged.overlay.inspector())
    }

    pub(super) async fn resolve_chat_folder_grants(
        &self,
        chat: &Chat,
    ) -> std::result::Result<Vec<ResolvedExecFolderGrant>, CodeExecutionError> {
        let Some(resolver) = self.folder_grant_resolver.as_ref() else {
            return Ok(Vec::new());
        };
        let root_ids = chat
            .root_attachments
            .iter()
            .map(|attachment| attachment.root_id)
            .collect::<Vec<_>>();
        if root_ids.is_empty() {
            return Ok(Vec::new());
        }
        let allowed = root_ids.iter().copied().collect::<HashSet<_>>();
        let resolved = resolver
            .resolve(ExecFolderGrantQuery {
                chat_id: chat.id,
                project_id: chat.project_id,
                root_ids: root_ids.clone(),
            })
            .await
            .map_err(CodeExecutionError::Sandbox)?;
        if resolved.len() > root_ids.len() {
            return Err(CodeExecutionError::Sandbox(
                "host returned too many execution folder grants".into(),
            ));
        }
        let mut by_id = HashMap::new();
        for grant in resolved {
            if !allowed.contains(&grant.root_id) || by_id.insert(grant.root_id, grant).is_some() {
                return Err(CodeExecutionError::Sandbox(
                    "host returned an invalid execution folder grant".into(),
                ));
            }
        }
        let mut ordered = Vec::new();
        for root_id in root_ids {
            if let Some(grant) = by_id.remove(&root_id) {
                ExecFolderGrant::new(
                    &grant.path,
                    if grant.writable {
                        ExecFolderAccess::ReadWrite
                    } else {
                        ExecFolderAccess::ReadOnly
                    },
                )?;
                ordered.push(grant);
            }
        }
        Ok(ordered)
    }

    async fn writable_connected_root(
        &self,
        chat_id: ChatId,
        root_id: HostRootId,
    ) -> std::result::Result<PathBuf, RejectedChangeReason> {
        let chat = self
            .store
            .get_chat(chat_id)
            .await
            .map_err(|_| RejectedChangeReason::Unavailable)?
            .ok_or(RejectedChangeReason::Unavailable)?;
        self.resolve_chat_folder_grants(&chat)
            .await
            .map_err(|_| RejectedChangeReason::Unavailable)?
            .into_iter()
            .find(|grant| grant.root_id == root_id && grant.writable)
            .map(|grant| grant.path)
            .ok_or(RejectedChangeReason::Unavailable)
    }

    /// Resolve the currently selected adapter at the last boundary before use.
    async fn resolve(
        &self,
        network_policy: Option<&NetworkPolicy>,
    ) -> std::result::Result<
        (CodeExecutionProviderKind, Box<dyn CodeExecutionProvider>),
        CodeExecutionError,
    > {
        let config = read_config(&*self.store).await.map_err(|_| {
            CodeExecutionError::Unavailable("configuration storage is unavailable".into())
        })?;
        let Some(provider) = config.provider else {
            return Err(CodeExecutionError::NotConfigured);
        };
        let resolved: Box<dyn CodeExecutionProvider> = match provider {
            CodeExecutionProviderKind::Local => {
                // Mounted only once verified artifacts exist; an empty or
                // unusable cache leaves execution exactly as it was.
                let package_cache = match self.shared_package_cache().await {
                    Some(cache) if cache.is_ready() => Some(cache.wheels_dir()),
                    _ => None,
                };
                Box::new(
                    LocalExecutionProvider::new(
                        &self.scratch_root,
                        Duration::from_millis(config.timeout_ms),
                    )?
                    .with_network_policy(network_policy.cloned().unwrap_or_default())
                    .with_document_scripts(self.document_scripts_source.clone())
                    .with_shared_package_cache(package_cache)
                    .with_managed_node(self.managed_node_dir().await),
                )
            }
            CodeExecutionProviderKind::E2b => {
                let credential = E2BCredential::load(&*self.secrets)
                    .await?
                    .ok_or(CodeExecutionError::NotConfigured)?;
                let egress = network_policy
                    .map(network_egress_config)
                    .unwrap_or_else(|| config.egress.clone());
                Box::new(configured_e2b(
                    credential,
                    Duration::from_millis(config.timeout_ms),
                    self.remote_sessions.clone(),
                    &egress,
                    config.e2b_template.as_deref(),
                )?)
            }
            CodeExecutionProviderKind::Daytona => {
                let credential = DaytonaCredential::load(&*self.secrets)
                    .await?
                    .ok_or(CodeExecutionError::NotConfigured)?;
                let egress = network_policy
                    .map(network_egress_config)
                    .unwrap_or_else(|| config.egress.clone());
                Box::new(configured_daytona(
                    credential,
                    Duration::from_millis(config.timeout_ms),
                    self.remote_sessions.clone(),
                    &egress,
                    config.daytona_snapshot.as_deref(),
                    self.preparation_sink(),
                )?)
            }
            CodeExecutionProviderKind::Docker => {
                // The chat's policy reaches container creation, but only its
                // strictest class is enforced there: "no network" creates the
                // container with no network at all, and an allowlist runs on
                // the runtime's default network, which the settings surface
                // discloses rather than implying the policy reaches it.
                let egress = network_policy
                    .map(network_egress_config)
                    .unwrap_or_else(|| config.egress.clone());
                Box::new(configured_docker(
                    Duration::from_millis(config.timeout_ms),
                    self.remote_sessions.clone(),
                    &egress,
                )?)
            }
            _ => {
                return Err(CodeExecutionError::Unavailable(
                    "selected provider is not supported by this build".into(),
                ))
            }
        };
        Ok((provider, resolved))
    }

    /// The configured provider's optional durable-workspace surface.
    ///
    /// Returns `Ok(None)` when execution is disabled, no provider is fully
    /// configured, or the selected backend has no workspace lifecycle, so host
    /// callers degrade instead of failing. This is a host-internal API; no
    /// model-facing tool is registered over it.
    pub async fn workspace(
        &self,
    ) -> std::result::Result<Option<ConfiguredWorkspace>, CodeExecutionError> {
        let provider = match self.resolve(None).await {
            Ok((_, provider)) => provider,
            Err(CodeExecutionError::NotConfigured) => return Ok(None),
            Err(error) => return Err(error),
        };
        if provider.workspace_lifecycle().is_none() {
            return Ok(None);
        }
        Ok(Some(ConfiguredWorkspace { provider }))
    }

    /// Run one command for a background agent run in its own workspace.
    ///
    /// A background run is confined to the workspace named by its own identity:
    /// no folder authority, no conversation attachments, and no write overlay,
    /// so the only files it can read are the ones its own earlier commands
    /// wrote. Delegation already bypasses the conversation's approval gate, so
    /// this path must never be the one that hands a background agent host
    /// paths. The parent conversation contributes exactly one thing — its
    /// network policy — because the user chose that policy for this work.
    pub async fn execute_for_agent_run(
        &self,
        chat_id: ChatId,
        request: CodeExecutionRequest,
    ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
        if !request.folder_grants.is_empty() {
            return Err(CodeExecutionError::InvalidRequest(
                "a background run's execution carries no folder authority".into(),
            ));
        }
        let chat = self
            .store
            .get_chat(chat_id)
            .await
            .map_err(|_| {
                CodeExecutionError::Unavailable("conversation storage is unavailable".into())
            })?
            .ok_or_else(|| {
                CodeExecutionError::InvalidRequest("execution conversation does not exist".into())
            })?;
        let (kind, provider) = self.resolve(Some(&chat.network_policy)).await?;
        self.execute_prepared(kind, provider, request, None, chat_id)
            .await
    }

    /// Prepare the workspace, run one command, and reconcile its files.
    ///
    /// Everything above this point differs between a foreground turn and a
    /// background run — whose authority the request carries, and whose
    /// attachments belong in the workspace. From here down the two are the same
    /// operation against a private workspace. `chat` is the conversation whose
    /// attachments are materialized and whose write overlay this command joins;
    /// a background run passes `None` for both. `degradation_chat` is only the
    /// conversation the one-shot sandbox-degradation notice is deduplicated
    /// against.
    async fn execute_prepared(
        &self,
        kind: CodeExecutionProviderKind,
        provider: Box<dyn CodeExecutionProvider>,
        request: CodeExecutionRequest,
        chat: Option<ChatId>,
        degradation_chat: ChatId,
    ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
        let host_dir = self.scratch_root.join(request.workspace_id.as_str());
        let skills = self.current_skills().await;
        prepare_execution_directories(
            &host_dir,
            kind != CodeExecutionProviderKind::Local,
            self.document_scripts_source.as_deref(),
            &skills,
        )
        .await?;
        if let (Some(blobs), Some(chat_id)) = (self.blobs.as_deref(), chat) {
            materialize_chat_attachments(&*self.store, blobs, chat_id, &host_dir).await?;
        }
        // A remote sandbox has its own filesystem, but the model is shown one
        // path vocabulary across the file tools and exec. Stage exactly the
        // paths the model listed on this call into the workspace before the
        // command, and pull only output/ and preview/ back out afterwards —
        // the two directories the host output and preview scans read. The
        // local provider already runs inside scratch, so nothing is staged
        // there, but the listed paths are validated identically so a bad path
        // fails the same way on every provider.
        let lifecycle = match kind {
            CodeExecutionProviderKind::Local => None,
            _ => provider.workspace_lifecycle(),
        };
        let Some(lifecycle) = lifecycle else {
            sync::validate_staged_paths(&host_dir, &request.files).await?;
            let inspector = chat.and_then(|chat| self.overlay_inspector(chat));
            let mut response = provider.execute(request).await?;
            if let Some(inspector) = inspector {
                response.sync_notes.extend(inspector.notes().await);
            }
            // Local exec writes output/ directly into scratch; convert any
            // office files there so the skill's visual QA loop has a PDF to
            // render, exactly like the remote pull path below.
            if response.exit_code == Some(0) && !response.timed_out {
                response.sync_notes.extend(
                    openwave_code_execution::render_office_outputs(
                        self.office_converter.as_deref(),
                        &host_dir,
                    )
                    .await,
                );
            }
            return Ok(response);
        };
        // A staging that fails outright fails the execution: a listed path
        // that does not exist, an over-bound expansion, or an unreachable
        // workspace would otherwise surface as a baffling not-found inside the
        // sandbox. Entries a listed directory had to leave behind individually
        // ride along as notes instead.
        let mut staged_paths =
            implicit_staged_paths(self.document_scripts_source.is_some(), !skills.is_empty());
        staged_paths.extend(request.files.iter().cloned());
        let mut notes =
            sync::stage_listed_paths(lifecycle, &request.workspace_id, &host_dir, &staged_paths)
                .await?
                .notes;
        let mut response = provider.execute(request.clone()).await?;
        // A failed pull keeps the execution's output — the command did run —
        // and says the host copies are stale instead of failing the call.
        match sync::pull_result_dirs(lifecycle, &request.workspace_id, &host_dir).await {
            Ok(pulled) => notes.extend(pulled.notes),
            Err(error) => notes.push(format!(
                "output files were not copied back to private scratch: {error}"
            )),
        }
        // The pull just landed any office outputs in host scratch; convert
        // them there so the model can render page images next call. The
        // sandbox itself has no LibreOffice — the host is where the converter
        // lives, for every provider.
        if response.exit_code == Some(0) && !response.timed_out {
            notes.extend(
                openwave_code_execution::render_office_outputs(
                    self.office_converter.as_deref(),
                    &host_dir,
                )
                .await,
            );
        }
        // A failed command plus an empty or thin staged set usually means the
        // command's inputs were never listed; one bounded line points there.
        if response.timed_out || response.exit_code != Some(0) {
            notes.push(staged_set_note(&request.files));
        }
        response.sync_notes.extend(notes);
        // Degrading is news once. The provider reports it on the execution that
        // discovered it; a later sandbox rebuild rediscovers the same thing, and
        // repeating it on every card would be noise rather than information.
        if response.degraded.is_some()
            && !self
                .degradation_reported
                .lock()
                .map(|mut reported| reported.insert(degradation_chat))
                .unwrap_or(false)
        {
            response.degraded = None;
        }
        Ok(response)
    }

    /// Publish a background run's `output/` files into its parent conversation.
    ///
    /// The run wrote the files and named them; this only records what is there.
    /// Revisions are attributed to the run rather than to a turn, so a file two
    /// runs both wrote becomes successive versions of one output, exactly as it
    /// does when a turn overwrites its own.
    pub async fn collect_agent_run_outputs(
        &self,
        workspace: &ExecutionWorkspaceId,
        chat_id: ChatId,
        call_id: CallId,
        run_id: openwave_core::AgentRunId,
    ) -> std::result::Result<OutputArtifactScan, CodeExecutionError> {
        self.publish_output_directory(workspace, chat_id, call_id, RevisionProducer::Run(run_id))
            .await
    }

    /// Open one private scratch directory under the scratch root, creating it
    /// if a conversation has not needed one yet — a background run can publish
    /// into a conversation that has never executed anything itself.
    async fn open_scratch_directory(
        &self,
        name: &str,
    ) -> std::result::Result<cap_std::fs::Dir, CodeExecutionError> {
        let path = self.scratch_root.join(name);
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&path)?;
            cap_std::fs::Dir::open_ambient_dir(&path, cap_std::ambient_authority())
        })
        .await
        .map_err(|_| CodeExecutionError::Sandbox("output scan task failed".into()))?
        .map_err(|_| CodeExecutionError::Sandbox("the private workspace is unavailable".into()))
    }

    /// Scan one workspace's `output/` and record what changed as revisions.
    ///
    /// Files are read out of the workspace the command ran in and their bytes
    /// are published into the owning conversation's own scratch, which is where
    /// every reader resolves a revision. For a foreground turn those are the
    /// same directory; a background run's workspace is its own, so the two
    /// differ and only the publication side follows the conversation.
    async fn publish_output_directory(
        &self,
        workspace: &ExecutionWorkspaceId,
        chat_id: ChatId,
        call_id: CallId,
        producer: RevisionProducer,
    ) -> std::result::Result<OutputArtifactScan, CodeExecutionError> {
        let workspace_dir = self.open_scratch_directory(workspace.as_str()).await?;
        let publication_dir = if workspace.as_str() == chat_id.to_string() {
            workspace_dir.try_clone().map_err(|_| {
                CodeExecutionError::Sandbox("the private workspace is unavailable".into())
            })?
        } else {
            self.open_scratch_directory(&chat_id.to_string()).await?
        };

        let sync = openwave_core::sync_output_directory(
            &*self.store,
            &workspace_dir,
            &publication_dir,
            chat_id,
            call_id,
            producer,
            Utc::now(),
        )
        .await
        .map_err(|error| {
            CodeExecutionError::Unavailable(format!("outputs could not be recorded: {error}"))
        })?;
        Ok(OutputArtifactScan {
            entries: sync
                .entries
                .into_iter()
                .map(|entry| OutputArtifactEntry {
                    filename: entry.filename,
                    output_id: entry.output_id.to_string(),
                    ordinal: entry.ordinal,
                    status: match entry.status {
                        openwave_core::OutputSyncStatus::Created => OutputArtifactStatus::Created,
                        openwave_core::OutputSyncStatus::Updated => OutputArtifactStatus::Updated,
                        openwave_core::OutputSyncStatus::Unchanged => {
                            OutputArtifactStatus::Unchanged
                        }
                    },
                })
                .collect(),
            notes: sync.notes,
        })
    }
}

/// The pin sets one cache-population pass acquires: the baseline set, plus
/// each skill's own pinned requirements.
///
/// The baseline set is preinstalled in the managed sandbox image; on the local
/// backend the cache is what makes it available offline, so it is acquired
/// regardless of which skills are loaded. Each skill stays its own set because
/// a set's pins resolve as one consistent closure, and one unresolvable set
/// must not sink the others' artifacts.
fn package_cache_pin_sets<'a>(
    skills: impl Iterator<Item = &'a openwave_code_execution::LoadedSkill>,
) -> Vec<Vec<String>> {
    let mut pin_sets = Vec::new();
    let baseline = openwave_code_execution::baseline_python_deps()
        .iter()
        .map(|pin| (*pin).to_owned())
        .collect::<Vec<_>>();
    if !baseline.is_empty() {
        pin_sets.push(baseline);
    }
    pin_sets.extend(
        skills
            .map(|skill| skill.package.python_deps.clone())
            .filter(|pins| !pins.is_empty()),
    );
    pin_sets
}

/// Acquire `pin_sets` into `cache`, one set at a time, and report whether
/// every set succeeded.
///
/// The lock is what keeps two triggers — a boot or plugin-enable provisioning
/// pass and the exec-time one — from running pip against the same cache
/// concurrently. Failures are logged and not propagated: a later exec or a
/// later enable retries, and conversations keep their networked install path
/// meanwhile.
async fn populate_package_cache(
    lock: &tokio::sync::Mutex<()>,
    cache: &SharedPackageCache,
    pin_sets: Vec<Vec<String>>,
) -> bool {
    let _guard = lock.lock().await;
    let mut succeeded = true;
    for pins in pin_sets {
        match cache
            .populate_with_pip(std::path::Path::new(SANDBOX_PYTHON), &pins)
            .await
        {
            Ok(report) => tracing::info!(
                promoted = report.promoted,
                refused = report.refused,
                invalidated = report.invalidated,
                evicted = report.evicted,
                "shared package cache population pass finished"
            ),
            Err(error) => {
                succeeded = false;
                tracing::warn!(%error, "shared package cache population failed");
            }
        }
    }
    succeeded
}

pub(super) fn exec_folder_grant_for_turn(
    grant: ResolvedExecFolderGrant,
    staged: &HashMap<PathBuf, PathBuf>,
) -> std::result::Result<ExecFolderGrant, CodeExecutionError> {
    let overlay = grant
        .writable
        .then(|| staged.get(&grant.path))
        .flatten()
        .cloned();
    // A live broker write grant is necessary but no longer sufficient: this
    // turn must also have staged the folder. Missing staging fails closed
    // instead of quietly restoring unrestricted writes to the real root.
    let writable = grant.writable && overlay.is_some();
    let resolved = ExecFolderGrant::new(
        grant.path,
        if writable {
            ExecFolderAccess::ReadWrite
        } else {
            ExecFolderAccess::ReadOnly
        },
    )?;
    match overlay {
        Some(overlay) => resolved.staged_at(overlay),
        None => Ok(resolved),
    }
}

#[async_trait]
impl CodeExecutionProvider for ConfiguredCodeExecutionProvider {
    async fn execute(
        &self,
        mut request: CodeExecutionRequest,
    ) -> std::result::Result<CodeExecutionResponse, CodeExecutionError> {
        if !request.folder_grants.is_empty() {
            return Err(CodeExecutionError::InvalidRequest(
                "execution folder grants are host-resolved state".into(),
            ));
        }
        let chat_id = request
            .workspace_id
            .as_str()
            .parse::<ChatId>()
            .map_err(|_| {
                CodeExecutionError::InvalidRequest(
                    "execution workspace does not identify a conversation".into(),
                )
            })?;
        let chat = self
            .store
            .get_chat(chat_id)
            .await
            .map_err(|_| {
                CodeExecutionError::Unavailable("conversation storage is unavailable".into())
            })?
            .ok_or_else(|| {
                CodeExecutionError::InvalidRequest("execution conversation does not exist".into())
            })?;
        let (kind, provider) = self.resolve(Some(&chat.network_policy)).await?;
        if kind == CodeExecutionProviderKind::Local
            && permits_package_installs(&chat.network_policy)
        {
            // A networked local exec is the signal that installs are wanted:
            // the same pins a conversation installs under its per-chat HOME
            // are acquired host-side into the shared cache, so a later
            // conversation can install them with the network off.
            if let Some(cache) = self.shared_package_cache().await {
                self.spawn_package_cache_population(cache);
            }
        }
        if kind == CodeExecutionProviderKind::Local && cfg!(target_os = "macos") {
            // Authority is resolved again here rather than reused from the
            // turn's prompt snapshot, so a revocation mid-turn fails closed.
            // Staging is looked up rather than re-established: the overlay
            // belongs to the turn, and every command in it writes to the same
            // staged tree.
            let staged = self.staged_folders(chat_id);
            let grants = self
                .resolve_chat_folder_grants(&chat)
                .await?
                .into_iter()
                .map(|grant| exec_folder_grant_for_turn(grant, &staged))
                .collect::<std::result::Result<Vec<_>, _>>()?;
            request = request.with_folder_grants(grants)?;
        }
        self.execute_prepared(kind, provider, request, Some(chat_id), chat_id)
            .await
    }

    async fn collect_preview_images(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<PreviewScan, CodeExecutionError> {
        let preview_dir = self.scratch_root.join(workspace.as_str()).join("preview");
        tokio::task::spawn_blocking(move || {
            openwave_code_execution::scan_preview_directory(&preview_dir)
        })
        .await
        .map_err(|_| CodeExecutionError::Sandbox("preview scan task failed".into()))
    }

    async fn collect_output_artifacts(
        &self,
        workspace: &ExecutionWorkspaceId,
        execution: &ExecutionId,
    ) -> std::result::Result<OutputArtifactScan, CodeExecutionError> {
        let chat_id = workspace.as_str().parse::<ChatId>().map_err(|_| {
            CodeExecutionError::InvalidRequest(
                "execution workspace does not identify a conversation".into(),
            )
        })?;
        let call_id = execution.as_str().parse::<CallId>().map_err(|_| {
            CodeExecutionError::InvalidRequest(
                "execution does not carry a canonical tool-call identity".into(),
            )
        })?;
        // The revision's producer is the turn that owns this exec call, read
        // from the durable call record rather than anything the model asserts.
        let calls = self.store.list_tool_calls(chat_id).await.map_err(|_| {
            CodeExecutionError::Unavailable("tool-call storage is unavailable".into())
        })?;
        let turn_id = calls
            .into_iter()
            .find(|call| call.id == call_id)
            .map(|call| call.turn_id)
            .ok_or_else(|| {
                CodeExecutionError::InvalidRequest(
                    "execution identity is not owned by this conversation".into(),
                )
            })?;
        self.publish_output_directory(workspace, chat_id, call_id, RevisionProducer::Turn(turn_id))
            .await
    }

    // `workspace_lifecycle` stays `None` here on purpose: the capability of
    // this late-binding wrapper depends on the configuration read at call
    // time, which the synchronous trait flag cannot express. Host callers use
    // [`ConfiguredCodeExecutionProvider::workspace`] instead.
}

/// A resolved workspace-lifecycle handle over the currently selected provider.
pub struct ConfiguredWorkspace {
    provider: Box<dyn CodeExecutionProvider>,
}

impl ConfiguredWorkspace {
    fn lifecycle(&self) -> std::result::Result<&dyn WorkspaceLifecycle, CodeExecutionError> {
        // Checked when this handle was constructed; re-checked instead of
        // unwrapped so a defect degrades into an error, not a panic.
        self.provider.workspace_lifecycle().ok_or_else(|| {
            CodeExecutionError::Unavailable("selected provider lost its workspace surface".into())
        })
    }
}

#[async_trait]
impl WorkspaceLifecycle for ConfiguredWorkspace {
    async fn create_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<(), CodeExecutionError> {
        self.lifecycle()?.create_workspace(workspace).await
    }

    async fn connect_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<bool, CodeExecutionError> {
        self.lifecycle()?.connect_workspace(workspace).await
    }

    async fn destroy_workspace(
        &self,
        workspace: &ExecutionWorkspaceId,
    ) -> std::result::Result<(), CodeExecutionError> {
        self.lifecycle()?.destroy_workspace(workspace).await
    }

    async fn put_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> std::result::Result<(), CodeExecutionError> {
        self.lifecycle()?
            .put_workspace_file(workspace, path, content)
            .await
    }

    async fn stage_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
        content: &[u8],
    ) -> std::result::Result<StagedUpload, CodeExecutionError> {
        // Delegated rather than left to the trait default so the selected
        // backend's session memory is not bypassed by the wrapper.
        self.lifecycle()?
            .stage_workspace_file(workspace, path, content)
            .await
    }

    async fn get_workspace_file(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: &WorkspaceFilePath,
    ) -> std::result::Result<Vec<u8>, CodeExecutionError> {
        self.lifecycle()?.get_workspace_file(workspace, path).await
    }

    async fn list_workspace_files(
        &self,
        workspace: &ExecutionWorkspaceId,
        path: Option<&WorkspaceFilePath>,
    ) -> std::result::Result<WorkspaceListing, CodeExecutionError> {
        self.lifecycle()?
            .list_workspace_files(workspace, path)
            .await
    }
}
