//! The registration lifecycle of a local app at the model gateway.
//!
//! A local app's gateway bindings are relayed to a *shared app* the gateway
//! holds, so before anything can be relayed the app has to exist there. This
//! module owns that: it projects the local manifest into the gateway's own
//! manifest vocabulary, registers the app the first time one of its gateway
//! bindings is consented to or invoked, appends a revision whenever the local
//! app has moved on, and relays the author's consent so the gateway's own
//! consent gate does not stand between an author and the app they just
//! granted.
//!
//! Two seams keep this drivable in tests. [`GatewayDraftClient`] is the
//! network half — the same rationale as
//! [`crate::connected_apps::RestOperationDispatcher`]: the registration
//! ladder is worth driving end to end, and no test can stand up an OAuth
//! session against a fake deployment to do it. [`GatewayDraftRegistry`] is
//! the durable half, and is what production wires behind
//! [`crate::connected_apps::GatewayDraftSource`].
//!
//! The mapping is stored per `(app, deployment)`, so a profile re-paired to a
//! different gateway holds no registration there and registers afresh; the
//! rows the old pairing left are orphaned rather than misread, and they die
//! with the app row.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;

use tidebreak_core::id::AppId;
use tidebreak_core::local_app::{
    app_revision_relative_path, AppBinding, AppGatewayDraft, AppManifest, AppRecord, AppRevision,
    MAX_APP_NAME_CHARS,
};
use tidebreak_core::{AgentError, OwnerId, Result, Store};

use crate::connected_apps::{
    GatewayConsentRelay, GatewayDispatchError, GatewayDraftSource, GatewayRegistration,
};
use crate::connectors::{GatewayConsentOutcome, GatewayInvokeOutcome, GatewayRegistrationOutcome};
use crate::gateway_runtime::GatewayRuntime;

/// The client name every registration this host makes is stamped with, so a
/// gateway operator can tell where a shared app came from.
const CLIENT_NAME: &str = "Tidebreak";

/// The longest title the gateway's shared-app manifest accepts.
///
/// Every name a local manifest can carry has to survive the projection
/// unchanged: a title the gateway would truncate or refuse silently renames
/// the app there, or fails a registration for a name the author was allowed
/// to choose. The two bounds are compatible today, and this build fails the
/// day either one moves so that they are not.
const MAX_GATEWAY_TITLE_CHARS: usize = 120;
const _: () = assert!(MAX_APP_NAME_CHARS <= MAX_GATEWAY_TITLE_CHARS);

/// One local revision, projected into what a shared-app create or revision
/// append carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedAppProjection {
    /// Display name — the gateway manifest's `title` and the create body's
    /// `name`.
    pub(crate) name: String,
    /// The gateway's manifest vocabulary: `{title, bindings, parameters}`.
    pub(crate) manifest: serde_json::Value,
    /// The revision's bundle bytes, base64 as the gateway takes them.
    pub(crate) bundle_base64: String,
}

/// The network half of registration, so the whole ladder is drivable against
/// a fake instead of a deployment.
///
/// Every method answers `Ok(None)` for "nothing at this deployment can hold
/// this app" — the route is absent (a gateway that predates shared-app
/// registration), the app is not this user's, or there is no session to ask
/// with. That is the same `Ok(None)`-on-404 reading the rest of the gateway
/// client uses, and it is a degradation rather than a fault.
#[async_trait]
pub(crate) trait GatewayDraftClient: Send + Sync {
    /// Register a new shared app. `slug` is only sent when the caller is
    /// asking for a specific one; otherwise the gateway derives it.
    async fn create(
        &self,
        gateway_base_url: &str,
        slug: Option<&str>,
        projection: &SharedAppProjection,
    ) -> Result<Option<GatewayRegistrationOutcome>>;

    /// Append a revision to an already-registered shared app.
    async fn append(
        &self,
        gateway_base_url: &str,
        shared_app_id: &str,
        projection: &SharedAppProjection,
    ) -> Result<Option<GatewayRegistrationOutcome>>;

    /// Record consent for a registered shared app, pinned to `revision_id`.
    async fn consent(
        &self,
        gateway_base_url: &str,
        shared_app_id: &str,
        revision_id: &str,
    ) -> Result<Option<GatewayConsentOutcome>>;
}

/// The production client: the three registration calls over the one gateway
/// runtime's connection.
pub(crate) struct GatewayConnectorDraftClient {
    runtime: Arc<GatewayRuntime>,
}

impl GatewayConnectorDraftClient {
    pub(crate) fn new(runtime: Arc<GatewayRuntime>) -> Self {
        Self { runtime }
    }

    /// The connection for `gateway_base_url`, and only for it.
    ///
    /// The runtime resolves its connection from policy, which can move
    /// between the moment a caller read the deployment and the moment the
    /// call goes out — a re-pairing is exactly that. Registering against a
    /// deployment other than the one the mapping is keyed to would mint a
    /// shared app nothing ever reads again, so a mismatch refuses instead:
    /// the caller's ladder reports it as unreachable, and the next attempt
    /// resolves the new deployment from the start.
    async fn connection_at(
        &self,
        gateway_base_url: &str,
    ) -> Result<Option<Arc<crate::connectors::GatewayConnection>>> {
        let Some(resolved) = registration_base_url(&self.runtime).await else {
            return Ok(None);
        };
        if normalized_gateway_base_url(&resolved) != gateway_base_url {
            return Err(AgentError::Store(
                "this profile's gateway moved while the app was being registered".into(),
            ));
        }
        self.runtime.connection().await
    }
}

#[async_trait]
impl GatewayDraftClient for GatewayConnectorDraftClient {
    async fn create(
        &self,
        gateway_base_url: &str,
        slug: Option<&str>,
        projection: &SharedAppProjection,
    ) -> Result<Option<GatewayRegistrationOutcome>> {
        let Some(connection) = self.connection_at(gateway_base_url).await? else {
            return Ok(None);
        };
        let mut body = serde_json::Map::new();
        body.insert(
            "name".into(),
            serde_json::Value::String(projection.name.clone()),
        );
        if let Some(slug) = slug {
            body.insert("slug".into(), serde_json::Value::String(slug.to_owned()));
        }
        body.insert("manifest".into(), projection.manifest.clone());
        body.insert(
            "bundle_base64".into(),
            serde_json::Value::String(projection.bundle_base64.clone()),
        );
        body.insert(
            "client_name".into(),
            serde_json::Value::String(CLIENT_NAME.to_owned()),
        );
        connection
            .create_shared_app(&serde_json::Value::Object(body))
            .await
    }

    async fn append(
        &self,
        gateway_base_url: &str,
        shared_app_id: &str,
        projection: &SharedAppProjection,
    ) -> Result<Option<GatewayRegistrationOutcome>> {
        let Some(connection) = self.connection_at(gateway_base_url).await? else {
            return Ok(None);
        };
        // Stamped per revision, not once per app: provenance that only the
        // first revision carries says nothing about the ones after it.
        let body = serde_json::json!({
            "manifest": projection.manifest,
            "bundle_base64": projection.bundle_base64,
            "client_name": CLIENT_NAME,
        });
        connection
            .create_shared_app_revision(shared_app_id, &body)
            .await
    }

    async fn consent(
        &self,
        gateway_base_url: &str,
        shared_app_id: &str,
        revision_id: &str,
    ) -> Result<Option<GatewayConsentOutcome>> {
        let Some(connection) = self.connection_at(gateway_base_url).await? else {
            return Ok(None);
        };
        connection
            .consent_shared_app(shared_app_id, Some(revision_id))
            .await
    }
}

/// The store-backed registration lifecycle: the durable `(app, deployment) →
/// shared app` mapping plus the calls that establish and advance it.
pub(crate) struct GatewayDraftRegistry {
    store: Arc<dyn Store>,
    /// Profile data directory the write-once bundle bytes live under.
    data_dir: PathBuf,
    client: Arc<dyn GatewayDraftClient>,
    /// One lock per app, so the read-then-register decision is serialized.
    ///
    /// Without it two concurrent invokes of an unregistered app both read no
    /// mapping and both create: the loser retries under a suffixed slug and
    /// succeeds, so the gateway mints two shared apps and one of them is a
    /// permanent orphan nothing will ever read again. Holding the lock across
    /// the whole decision means the second caller reads the mapping the first
    /// persisted. The map is keyed by app and bounded by the library, so it
    /// is not swept.
    app_locks: tokio::sync::Mutex<std::collections::HashMap<AppId, Arc<tokio::sync::Mutex<()>>>>,
}

impl GatewayDraftRegistry {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        data_dir: PathBuf,
        client: Arc<dyn GatewayDraftClient>,
    ) -> Self {
        Self {
            store,
            data_dir,
            client,
            app_locks: tokio::sync::Mutex::default(),
        }
    }

    /// The registration lock for one app.
    async fn app_lock(&self, app: AppId) -> Arc<tokio::sync::Mutex<()>> {
        self.app_locks.lock().await.entry(app).or_default().clone()
    }

    /// Register `app` at `base_url` for the first time.
    ///
    /// The slug is the gateway's to derive on the first attempt. A taken slug
    /// is the one failure worth asking again about, and exactly once: the
    /// retry names a slug suffixed with the app's own identity, which no
    /// other app of this profile can collide with. A second refusal is
    /// reported rather than retried — a registration loop against a gateway
    /// that keeps saying no is worse than an honest refusal.
    async fn register(
        &self,
        owner: &OwnerId,
        record: &AppRecord,
        revision: &AppRevision,
        base_url: &str,
    ) -> Result<GatewayRegistration> {
        let projection = self.project(record, revision).await?;
        let outcome = self.client.create(base_url, None, &projection).await?;
        let outcome = match outcome {
            Some(GatewayRegistrationOutcome::SlugTaken { .. }) => {
                let slug = suffixed_slug(&projection.name, record.id);
                self.client
                    .create(base_url, Some(&slug), &projection)
                    .await?
            }
            other => other,
        };
        match outcome {
            None => Ok(GatewayRegistration::NotRegistered),
            Some(GatewayRegistrationOutcome::Registered {
                shared_app_id,
                revision_id,
            }) => {
                self.persist(
                    owner,
                    record,
                    revision,
                    base_url,
                    &shared_app_id,
                    &revision_id,
                )
                .await?;
                Ok(GatewayRegistration::Registered {
                    shared_app_id,
                    revision_id,
                })
            }
            // The host knows something the gateway's message cannot: this was
            // the second attempt, under a slug the host chose to be unique.
            Some(GatewayRegistrationOutcome::SlugTaken { .. }) => {
                Ok(GatewayRegistration::Refused {
                    message: "this app's name is already taken at your model gateway".to_owned(),
                })
            }
            Some(GatewayRegistrationOutcome::Refused { message }) => {
                Ok(GatewayRegistration::Refused { message })
            }
        }
    }

    /// Push `revision` to an already-registered shared app and re-point the
    /// mapping at what the gateway now serves.
    async fn append(
        &self,
        owner: &OwnerId,
        draft: &AppGatewayDraft,
        record: &AppRecord,
        revision: &AppRevision,
        base_url: &str,
    ) -> Result<GatewayRegistration> {
        let projection = self.project(record, revision).await?;
        match self
            .client
            .append(base_url, &draft.shared_app_id, &projection)
            .await?
        {
            None => Ok(GatewayRegistration::NotRegistered),
            Some(GatewayRegistrationOutcome::Registered { revision_id, .. }) => {
                self.persist(
                    owner,
                    record,
                    revision,
                    base_url,
                    &draft.shared_app_id,
                    &revision_id,
                )
                .await?;
                Ok(GatewayRegistration::Registered {
                    shared_app_id: draft.shared_app_id.clone(),
                    revision_id,
                })
            }
            // A slug is not part of a revision append, so this arm should be
            // unreachable — which is exactly why it carries the gateway's own
            // words rather than a guess at what it meant.
            Some(GatewayRegistrationOutcome::SlugTaken { message }) => {
                Ok(GatewayRegistration::Refused { message })
            }
            Some(GatewayRegistrationOutcome::Refused { message }) => {
                Ok(GatewayRegistration::Refused { message })
            }
        }
    }

    /// Record what the gateway now holds for this app at this deployment.
    async fn persist(
        &self,
        owner: &OwnerId,
        record: &AppRecord,
        revision: &AppRevision,
        base_url: &str,
        shared_app_id: &str,
        gateway_revision_id: &str,
    ) -> Result<()> {
        self.store
            .put_app_gateway_draft_scoped(
                owner,
                &AppGatewayDraft {
                    app_id: record.id,
                    gateway_base_url: base_url.to_owned(),
                    shared_app_id: shared_app_id.to_owned(),
                    gateway_revision_id: gateway_revision_id.to_owned(),
                    synced_revision_id: revision.id,
                    updated_at: chrono::Utc::now(),
                },
            )
            .await
    }

    /// The app and the revision it currently presents.
    ///
    /// A soft-deleted app answers as missing, exactly as it does on the
    /// consent and invoke surfaces. An invoke racing a deletion must not mint
    /// a shared app at the gateway for something the library no longer holds:
    /// the store refuses the mapping write for the same reason, and refusing
    /// here means the network call never happens in the first place.
    async fn current(&self, owner: &OwnerId, app: AppId) -> Result<(AppRecord, AppRevision)> {
        let missing = || AgentError::Store(format!("app {app} not found"));
        let record = self
            .store
            .get_app_scoped(owner, app)
            .await?
            .ok_or_else(missing)?;
        if record.deleted_at.is_some() {
            return Err(missing());
        }
        let revision = self
            .store
            .get_app_revision_scoped(owner, record.current_revision)
            .await?
            .ok_or_else(missing)?;
        Ok((record, revision))
    }

    /// Project one local revision into the gateway's registration vocabulary.
    async fn project(
        &self,
        record: &AppRecord,
        revision: &AppRevision,
    ) -> Result<SharedAppProjection> {
        let path = self
            .data_dir
            .join(app_revision_relative_path(record.id, revision.id));
        let bundle = tokio::fs::read(&path).await.map_err(|error| {
            AgentError::Store(format!("could not read the app's bundle bytes: {error}"))
        })?;
        Ok(SharedAppProjection {
            name: revision.manifest.name.clone(),
            manifest: gateway_manifest(&revision.manifest),
            bundle_base64: base64::engine::general_purpose::STANDARD.encode(bundle),
        })
    }
}

#[async_trait]
impl GatewayDraftSource for GatewayDraftRegistry {
    async fn ensure_registered(
        &self,
        owner: &OwnerId,
        app: AppId,
        gateway_base_url: &str,
    ) -> Result<GatewayRegistration> {
        let base_url = normalized_gateway_base_url(gateway_base_url);
        // Serialize the read-then-register decision per app; see `app_locks`.
        let lock = self.app_lock(app).await;
        let _guard = lock.lock().await;
        let (record, revision) = self.current(owner, app).await?;
        match self
            .store
            .get_app_gateway_draft_scoped(owner, app, &base_url)
            .await?
        {
            // The gateway is already serving this exact local revision.
            Some(draft) if draft.synced_revision_id == revision.id => {
                Ok(GatewayRegistration::Registered {
                    shared_app_id: draft.shared_app_id,
                    revision_id: draft.gateway_revision_id,
                })
            }
            // Registered, but the app has moved on locally. The sync is lazy
            // by design: revisions nobody ever invoked are never pushed, so
            // the gateway's history is what was servable when it was used.
            Some(draft) => {
                self.append(owner, &draft, &record, &revision, &base_url)
                    .await
            }
            None => self.register(owner, &record, &revision, &base_url).await,
        }
    }

    async fn relay_consent(
        &self,
        owner: &OwnerId,
        app: AppId,
        gateway_base_url: &str,
    ) -> Result<GatewayConsentRelay> {
        let base_url = normalized_gateway_base_url(gateway_base_url);
        let (shared_app_id, revision_id) = match self
            .ensure_registered(owner, app, &base_url)
            .await?
        {
            GatewayRegistration::Registered {
                shared_app_id,
                revision_id,
            } => (shared_app_id, revision_id),
            GatewayRegistration::NotRegistered => return Ok(GatewayConsentRelay::NotRegistered),
            GatewayRegistration::Refused { message } => {
                return Ok(GatewayConsentRelay::Refused { message })
            }
        };
        match self
            .client
            .consent(&base_url, &shared_app_id, &revision_id)
            .await?
        {
            None => Ok(GatewayConsentRelay::NotRegistered),
            Some(GatewayConsentOutcome::Consented) => Ok(GatewayConsentRelay::Consented),
            Some(GatewayConsentOutcome::Refused { message }) => {
                Ok(GatewayConsentRelay::Refused { message })
            }
            // The gateway serves a revision this host did not pin — something
            // else appended one. Re-establish the pin from the current local
            // revision and consent to that, once.
            Some(GatewayConsentOutcome::RevisionMoved) => {
                let (record, revision) = self.current(owner, app).await?;
                let draft = self
                    .store
                    .get_app_gateway_draft_scoped(owner, app, &base_url)
                    .await?;
                let Some(draft) = draft else {
                    return Ok(GatewayConsentRelay::NotRegistered);
                };
                let revision_id = match self
                    .append(owner, &draft, &record, &revision, &base_url)
                    .await?
                {
                    GatewayRegistration::Registered { revision_id, .. } => revision_id,
                    GatewayRegistration::NotRegistered => {
                        return Ok(GatewayConsentRelay::NotRegistered)
                    }
                    GatewayRegistration::Refused { message } => {
                        return Ok(GatewayConsentRelay::Refused { message })
                    }
                };
                match self
                    .client
                    .consent(&base_url, &shared_app_id, &revision_id)
                    .await?
                {
                    None => Ok(GatewayConsentRelay::NotRegistered),
                    Some(GatewayConsentOutcome::Consented) => Ok(GatewayConsentRelay::Consented),
                    Some(GatewayConsentOutcome::Refused { message }) => {
                        Ok(GatewayConsentRelay::Refused { message })
                    }
                    Some(GatewayConsentOutcome::RevisionMoved) => {
                        Ok(GatewayConsentRelay::Refused {
                            message: "your model gateway is serving a different revision of \
                                      this app"
                                .to_owned(),
                        })
                    }
                }
            }
        }
    }
}

/// Relay one shared-app call, healing the gateway's own consent gate at most
/// once.
///
/// A local app reaches here only after the whole local ladder has passed: the
/// manifest pins the operation, the grant covers it, and the bound gateway
/// app still reads as it did at consent. So a `consent_required` from the
/// gateway is not a second decision for the user to make — the consent sheet
/// already displayed exactly the binding set the gateway consent names, and
/// the gateway recomputes that set server-side from the live revision,
/// accepting only a revision pin from here. Re-stating it and calling again
/// is what keeps a granted app from dead-ending on an invisible second
/// consent surface.
///
/// Exactly one retry, and only after a consent relay that actually succeeded.
/// A second `consent_required` is the gateway's answer and is returned as
/// such: a relay that kept re-consenting would spin against a deployment that
/// has decided no.
pub(crate) async fn relay_with_consent_self_heal<F, Fut>(
    drafts: &dyn GatewayDraftSource,
    owner: &OwnerId,
    app: AppId,
    gateway_base_url: &str,
    relay: F,
) -> std::result::Result<GatewayInvokeOutcome, GatewayDispatchError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<
        Output = std::result::Result<GatewayInvokeOutcome, GatewayDispatchError>,
    >,
{
    let outcome = relay().await?;
    if !matches!(outcome, GatewayInvokeOutcome::ConsentRequired { .. }) {
        return Ok(outcome);
    }
    match drafts.relay_consent(owner, app, gateway_base_url).await {
        Ok(GatewayConsentRelay::Consented) => {}
        Ok(refused) => {
            tracing::info!("this app's gateway consent could not be relayed: {refused:?}");
            return Ok(outcome);
        }
        Err(error) => {
            tracing::warn!("could not relay this app's gateway consent: {error}");
            return Ok(outcome);
        }
    }
    relay().await
}

/// The deployment a registration belongs to, when this profile is managed
/// with a gateway URL — read exactly as the invoke dispatcher reads it, so
/// the grant and invoke paths can never key a mapping differently.
pub(crate) async fn registration_base_url(runtime: &GatewayRuntime) -> Option<String> {
    let policy = runtime.policy().ok()?;
    policy.gateway_url.filter(|_| policy.managed)
}

/// The deployment key a registration is stored under.
///
/// Normalized the way [`crate::connectors::GatewayCredentials`] compares base
/// URLs, so the same deployment retyped with or without a trailing slash is
/// one key rather than two registrations. A URL that does not parse is used
/// as given: it will not match a session either, and inventing a key for it
/// would be worse than keying it verbatim.
pub(crate) fn normalized_gateway_base_url(base_url: &str) -> String {
    match reqwest::Url::parse(base_url) {
        Ok(url) => {
            let mut normalized = url.to_string();
            if !normalized.ends_with('/') {
                normalized.push('/');
            }
            normalized
        }
        Err(_) => base_url.to_owned(),
    }
}

/// Project a local manifest into the gateway's shared-app manifest.
///
/// A transcription, not a translation. Gateway bindings carry across
/// verbatim — the manifest's `gateway_app` *is* the gateway's
/// `connected_app_id`, and the operation ids are the gateway's own. Folder
/// and local `rest_api` bindings are dropped: they name capabilities that
/// exist only on this machine, and a gateway asked to hold them would be
/// asked to hold something it cannot serve. `operation_policies` is never
/// emitted — the operations a shared app may call are exactly the ones its
/// bindings name. `parameters` is empty because a local app takes none.
fn gateway_manifest(manifest: &AppManifest) -> serde_json::Value {
    let bindings: Vec<serde_json::Value> = manifest
        .bindings
        .iter()
        .filter_map(|binding| match binding {
            AppBinding::GatewayOperations(binding) => Some(serde_json::json!({
                "connected_app_id": binding.gateway_app,
                "operation_ids": binding.operation_ids,
            })),
            AppBinding::Operations(_) | AppBinding::Folder(_) => None,
        })
        .collect();
    serde_json::json!({
        "title": manifest.name,
        "bindings": bindings,
        "parameters": [],
    })
}

/// The slug a second registration attempt asks for: the app's name, plus
/// enough of its own identity that no other app of this profile can collide
/// with it.
fn suffixed_slug(name: &str, app: AppId) -> String {
    let identity = app.0.simple().to_string();
    format!("{}-{}", slug_stem(name), &identity[..8])
}

/// A lowercase, hyphen-separated stem of a display name, bounded so the
/// suffix always survives. A name with nothing slug-able in it (an entirely
/// non-ASCII title) falls back to a fixed stem rather than an empty one.
fn slug_stem(name: &str) -> String {
    let mut stem = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            stem.push(character.to_ascii_lowercase());
        } else if !stem.ends_with('-') {
            stem.push('-');
        }
        if stem.len() >= 48 {
            break;
        }
    }
    let stem = stem.trim_matches('-');
    if stem.is_empty() {
        "app".to_owned()
    } else {
        stem.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use tidebreak_core::local_app::{
        AppFolderBinding, AppGatewayOperationsBinding, AppOperationsBinding, FolderAccess,
    };

    use super::*;

    /// A registration seam that only ever answers the consent relay.
    struct ScriptedDrafts {
        relayed: AtomicUsize,
    }

    #[async_trait]
    impl GatewayDraftSource for ScriptedDrafts {
        async fn ensure_registered(
            &self,
            _owner: &OwnerId,
            _app: AppId,
            _base_url: &str,
        ) -> Result<GatewayRegistration> {
            unreachable!("the relay resolves registration before the self-heal runs")
        }

        async fn relay_consent(
            &self,
            _owner: &OwnerId,
            _app: AppId,
            _base_url: &str,
        ) -> Result<GatewayConsentRelay> {
            self.relayed.fetch_add(1, Ordering::SeqCst);
            Ok(GatewayConsentRelay::Consented)
        }
    }

    /// The gateway's consent gate is healed exactly once. Healing at all is
    /// what keeps a granted app from dead-ending on a second, invisible
    /// consent surface; healing *once* is what keeps a deployment that has
    /// decided no from being asked forever.
    #[tokio::test]
    async fn a_consent_refusal_is_healed_once_and_never_looped() {
        let refusal = || GatewayInvokeOutcome::ConsentRequired {
            message: "consent required".to_owned(),
        };

        let drafts = ScriptedDrafts {
            relayed: AtomicUsize::new(0),
        };
        let attempts = AtomicUsize::new(0);
        let outcome = relay_with_consent_self_heal(
            &drafts,
            &OwnerId::local(),
            AppId::new(),
            "https://gw.example/",
            || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(if attempt == 0 {
                        GatewayInvokeOutcome::ConsentRequired {
                            message: "consent required".to_owned(),
                        }
                    } else {
                        GatewayInvokeOutcome::Executed {
                            status: 200,
                            content_type: None,
                            body_base64: String::new(),
                        }
                    })
                }
            },
        )
        .await
        .unwrap();
        assert!(matches!(outcome, GatewayInvokeOutcome::Executed { .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(drafts.relayed.load(Ordering::SeqCst), 1);

        let drafts = ScriptedDrafts {
            relayed: AtomicUsize::new(0),
        };
        let attempts = AtomicUsize::new(0);
        let outcome = relay_with_consent_self_heal(
            &drafts,
            &OwnerId::local(),
            AppId::new(),
            "https://gw.example/",
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async { Ok(refusal()) }
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, GatewayInvokeOutcome::ConsentRequired { .. }),
            "a second refusal is the gateway's answer, carried back as one"
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "never a third call");
    }

    /// The projection is the whole contract between a local manifest and the
    /// gateway's shared-app manifest, and two of its rules are silent
    /// failures if they ever move: a dropped-arm binding that leaks across
    /// asks the gateway to hold a capability only this machine has, and an
    /// emitted `operation_policies` would widen what the shared app may call
    /// beyond what its bindings name. (The third — that every local app name
    /// survives as a title — is checked at compile time above.)
    #[test]
    fn the_gateway_manifest_carries_gateway_bindings_and_nothing_else() {
        let manifest = AppManifest {
            name: "Issue triage".into(),
            bindings: vec![
                AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                    gateway_app: "gw-issues".into(),
                    operation_ids: vec!["listIssues".into()],
                }),
                AppBinding::Operations(AppOperationsBinding {
                    app: tidebreak_core::id::ConnectedAppId::new(),
                    operation_ids: vec!["localOnly".into()],
                }),
                AppBinding::Folder(AppFolderBinding {
                    folder: tidebreak_core::id::HostRootId::from_uuid(uuid::Uuid::new_v4())
                        .unwrap(),
                    access: FolderAccess::Read,
                }),
            ],
        };
        assert_eq!(
            gateway_manifest(&manifest),
            json!({
                "title": "Issue triage",
                "bindings": [{"connected_app_id": "gw-issues", "operation_ids": ["listIssues"]}],
                "parameters": [],
            })
        );
    }
}
