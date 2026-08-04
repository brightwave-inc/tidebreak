//! A [`SandboxBackend`] over the Docker CLI: provision a local container, resolve
//! it to a loopback address, and tear it down idempotently, plus a correlation-tag
//! orphan sweep that reclaims containers whose provisioning intent has no live run.
//!
//! # Placement
//!
//! This lives in `openwave-server` rather than in `openwave-sandbox-protocol`: the
//! protocol crate deliberately defines only the wire types and their semantics and
//! carries no transport or process dependency, while this adapter shells out to the
//! `docker` CLI. The server already owns the durable provisioning intents this sweep
//! reconciles against, and already depends on the protocol crate.
//!
//! # Scope
//!
//! This slice is the container *lifecycle* only. It stands a container up, publishes
//! its listener port to a loopback host port, and reports the reachable base URL.
//! The sandbox-agent wire transport that dials that URL — framing, the attach
//! handshake, the event stream — is a separate slice; nothing here speaks it.
//!
//! # The lifetime cap
//!
//! Docker has no TTL of its own, and a cap enforced by a host-side timer is worth
//! nothing in the case that matters: the host process dying is exactly what strands
//! a container, and a timer dies with it. So the cap is delivered *into* the
//! container — [`LIFETIME_CAP_ENV`] — and the sandbox agent exits when it elapses.
//! The agent is the init's only child, so its exit is the container's exit: the
//! container stops on its own whether or not any host is still alive to ask.
//!
//! This is an *absolute* cap, not an idle timeout: it bounds how long a container
//! may exist, not how long it may be quiet. The two are different guarantees and
//! only the absolute one is enforceable without the host participating — an idle
//! watchdog needs a keepalive from the attached host to distinguish a slow exec
//! from an abandoned run, and that keepalive does not exist on the transport yet.
//! The orphan sweep remains the reclaimer for a *live* host that lost track of a
//! container; the cap is what covers a host that is gone.
//!
//! # Detected capability
//!
//! The container runtime is a detected capability, exactly as Seatbelt is for local
//! code execution: [`DockerSandboxBackend::is_available`] reports whether the runtime
//! binary is resolvable, and the host consults it before choosing this backend. When
//! the binary is absent the trait methods fail with a normalized [`BackendError`]
//! rather than panicking — the runtime is never a hard dependency. A Docker-CLI
//! compatible runtime such as `podman` works through the same code path by pointing
//! [`DockerConfig::binary`] at it.
//!
//! # Container hardening
//!
//! In-container execution is the containment, so the container's own confinement
//! is the security control and not a detail of packaging. Every provisioning runs
//! the image as an unprivileged user, with every Linux capability dropped,
//! privilege escalation refused, a read-only root filesystem, and ceilings on
//! memory and process count. See [`SandboxHardening`] for what each of those buys
//! and where the writable surface is.
//!
//! # Egress enforcement
//!
//! Egress is enforced by network topology, not by anything inside the sandbox
//! container (whose `--cap-drop ALL` profile stays intact). Each provisioning
//! creates an *internal* Docker network — no route out — as the sandbox
//! container's only network, and stands up a second container from the same
//! image running the agent binary's `egress-proxy` mode. Only the proxy is
//! dual-homed: created on the default bridge (outbound reach plus a
//! loopback-published host port) and then connected onto the internal network
//! under the [`EGRESS_PROXY_ALIAS`] name. The sandbox's `HTTP(S)_PROXY`
//! environment points compliant tools at the proxy; a command that ignores it
//! has no route anywhere.
//!
//! The proxy enforces the run's compiled [`SandboxNetworkPolicy`] (deny by
//! default, delivered as JSON in its environment) with the same CONNECT-only
//! contract as the native local adapter's loopback broker, and it also carries
//! the host-to-agent transport: a port published on a container whose only
//! network is internal is not reachable from the host, so the proxy's published
//! port TCP-relays to the sandbox's supervisor across the internal network and
//! [`address`](SandboxBackend::address) resolves the proxy's port. The per-run
//! transport secret still gates attach.
//!
//! The sandbox never needs external DNS: CONNECT carries the destination
//! *name* and the proxy resolves it outside the sandbox. So the sandbox
//! container's DNS upstream is pointed at a blackhole ([`SANDBOX_DNS_SINK`])
//! — the embedded resolver still answers the internal network's own names
//! (the proxy alias) authoritatively, but an external lookup has nowhere to
//! go. On engines new enough to not forward external lookups from internal
//! networks the flag is redundant; on older engines that do forward, it turns
//! the name-lookup side channel into a query addressed to an unroutable
//! documentation prefix. Residual: on those older engines the query packet
//! still *leaves* the host toward that prefix before being dropped unanswered,
//! so an observer on the local segment could see looked-up names; payload
//! connections stay blocked either way.
//!
//! # Image verification
//!
//! The default image ref is the published documents-variant agent image pinned
//! **by digest** ([`PUBLISHED_IMAGE_DIGEST`]): a `repository@sha256:…` ref is
//! content-addressed, so the daemon resolves it to exactly those bytes or fails
//! the provisioning — repointing the tag on the registry changes nothing, and a
//! locally present image of another content cannot answer for it. That is the
//! fail-closed digest check issue #1188 asked for, enforced at resolution
//! rather than re-derived after the fact.
//!
//! A ref *without* a digest — the local development fallback while no pin is
//! recorded, or an operator override that names a mutable tag — is used
//! verbatim and stays unverified: the daemon resolves the tag at provisioning
//! time with no host-side integrity check. The backend reports which case it is
//! in through [`SandboxBackend::verifies_image_integrity`], so detached
//! admission and the settings surface treat an unpinned image as the unmet
//! precondition it is, and startup logs the unverified case.
//!
//! # The transport secret
//!
//! [`SandboxAddress`] bundles a per-run [`TransportSecret`]. The design has the host
//! mint that secret and deliver it through the provider's control plane, but the
//! provisioning path does not carry it yet, so this adapter mints it at
//! [`provision`](SandboxBackend::provision), injects it into the container's
//! environment (the local control plane is Docker itself), and recovers it in
//! [`address`](SandboxBackend::address). This keeps `address` a pure function of the
//! handle — it survives a host restart, because everything it needs lives in Docker's
//! own state — and the host-minted delivery wiring is a later slice's concern.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use openwave_core::NetworkPolicy;
use openwave_egress::DomainPattern;
use openwave_sandbox_protocol::{
    BackendError, ProvisionRequest, RunId, SandboxAddress, SandboxBackend, SandboxHandle,
    SandboxNetworkPolicy, SandboxTag, TransportSecret,
};
use tokio::process::Command;
use uuid::Uuid;

/// Label key stamping the host-minted correlation tag onto a provisioned container.
/// The orphan sweep filters on this label and reads the tag back from it.
pub const RUN_TAG_LABEL: &str = "openwave.run-tag";
/// Informational label recording which run a container serves.
pub const RUN_ID_LABEL: &str = "openwave.run-id";
/// Environment variable carrying the per-run transport secret into the container.
const TRANSPORT_SECRET_ENV: &str = "OPENWAVE_TRANSPORT_SECRET";
/// Environment variable carrying the absolute lifetime cap, in seconds, into the
/// container. The sandbox agent's entrypoint reads it and exits when it elapses.
/// Kept in sync with the agent's own constant of the same name.
pub const LIFETIME_CAP_ENV: &str = "OPENWAVE_SANDBOX_LIFETIME_CAP_SECS";
/// Go-template that lists a tagged container's id and correlation tag, tab-separated.
/// Kept in sync with [`RUN_TAG_LABEL`] by a unit test.
const TAG_LIST_FORMAT: &str = "{{.ID}}\t{{.Label \"openwave.run-tag\"}}";
/// Go-template that lists a tagged network's name and correlation tag,
/// tab-separated. Kept in sync with [`RUN_TAG_LABEL`] by the same unit test.
const NETWORK_TAG_LIST_FORMAT: &str = "{{.Name}}\t{{.Label \"openwave.run-tag\"}}";

/// The DNS name the sandbox reaches the egress proxy under on the internal
/// network, and the host the sandbox's `HTTP(S)_PROXY` environment names.
pub const EGRESS_PROXY_ALIAS: &str = "openwave-egress";
/// The in-proxy CONNECT listener port. Kept in sync with the agent binary's
/// `egress-proxy` default; never published to the host.
pub(crate) const EGRESS_PROXY_PORT: u16 = 3128;
/// Environment variable carrying the compiled egress policy (JSON) into the
/// proxy container. Kept in sync with the agent's constant of the same name.
const EGRESS_POLICY_ENV: &str = "OPENWAVE_EGRESS_POLICY";
/// The sandbox container's DNS upstream: an RFC 5737 documentation address
/// (TEST-NET-1), guaranteed never assigned, so a forwarded lookup goes
/// nowhere and gets no answer. Docker's embedded resolver keeps answering the
/// internal network's own names ([`EGRESS_PROXY_ALIAS`] included)
/// authoritatively regardless of the upstream; only external lookups — which
/// the sandbox never legitimately needs, since the proxy resolves CONNECT
/// destinations host-side — are affected. See the module docs' egress section
/// for the residual this leaves on older engines.
const SANDBOX_DNS_SINK: &str = "192.0.2.1";
/// Environment variable overriding the proxy's transport-relay listener.
const RELAY_LISTEN_ENV: &str = "OPENWAVE_RELAY_LISTEN";
/// Environment variable naming the relay's upstream — the sandbox container's
/// supervisor endpoint on the internal network.
const RELAY_TARGET_ENV: &str = "OPENWAVE_RELAY_TARGET";

/// The default runtime binary, resolved on `PATH`.
const DEFAULT_BINARY: &str = "docker";
/// The published documents-variant agent image: the agent binary plus
/// LibreOffice and the document skills' pinned Python dependencies, built and
/// pushed by `.github/workflows/publish-sandbox-image.yml` so background
/// document runs need no in-sandbox package install.
const PUBLISHED_IMAGE_REPOSITORY: &str = "ghcr.io/brightwave-inc/openwave-sandbox-agent-documents";
/// The immutable manifest-list digest (`sha256:<64 hex>`) of the published
/// documents image the default configuration runs.
///
/// PROVENANCE: recorded from the publish workflow's run summary — the digest
/// each publish run prints next to the documents ref. Update it in a one-line
/// PR whenever a new image version is published.
///
/// `None` until the first publish run exists: the digest cannot be pinned
/// before an image is published, and the publish workflow lands together with
/// this pin's machinery. While unset the default fails over to
/// [`LOCAL_DEV_IMAGE`] — a ref that only exists after a local `docker build`
/// and carries no digest verification, which
/// [`SandboxBackend::verifies_image_integrity`] reports honestly.
// Manifest-list digest of ghcr.io/brightwave-inc/openwave-sandbox-agent-documents:v0.26.0,
// published by workflow run 30861671881 (tag push v0.26.0, 2026-08-03); the run's
// step summary records the same value from a post-push `imagetools inspect`.
const PUBLISHED_IMAGE_DIGEST: Option<&str> =
    Some("sha256:dd22da7a3c5b1f315e888da902e7a46ae034585e2ab5c09c0ae4588a69f158a2");
/// The locally built development image, produced by the documented
/// `docker build -f crates/openwave-sandbox-agent/Dockerfile -t openwave-sandbox-agent .`
/// (whose default target is the documents variant). The fallback default while
/// no published digest is pinned; never digest-verified.
const LOCAL_DEV_IMAGE: &str = "openwave-sandbox-agent:latest";
/// The in-container port the sandbox supervisor listens on by default.
const DEFAULT_LISTENER_PORT: u16 = 8080;
/// The lifetime cap applied when a provisioning request names none: four hours.
/// It is a leak backstop, not a scheduling policy — comfortably longer than any
/// legitimate run, short enough that a container stranded by a dead host does not
/// outlive the day.
const DEFAULT_LIFETIME_CAP_SECS: u64 = 4 * 60 * 60;

/// The unprivileged `uid:gid` the container runs as. Kept in sync with the
/// `USER` directive and the workspace ownership in the sandbox-agent image's
/// Dockerfile: the host forces the identity so an image that lost its own
/// `USER` still cannot run the agent as root, and the numbers have to match or
/// the running user would not own the workspace the image provisioned.
const SANDBOX_USER: &str = "10001:10001";
/// The in-container workspace root, mounted writable over the read-only root
/// filesystem. Kept in sync with the image's `OPENWAVE_SANDBOX_WORKSPACE`.
const DEFAULT_WORKSPACE_DIR: &str = "/workspace";
/// Default memory ceiling for one container. Generous on purpose: a legitimate
/// data-analysis exec can hold a whole spreadsheet or rendered document set in
/// memory, and a limit tight enough to kill that turns a working feature into an
/// unexplained OOM. It exists to stop one runaway container from taking the
/// host's memory, not to schedule work.
const DEFAULT_MEMORY_LIMIT: &str = "4g";
/// Default process-count ceiling. Comfortably above what a shell pipeline, a
/// Python helper, or a headless LibreOffice conversion needs, and far below what
/// a fork bomb needs to wedge the host.
const DEFAULT_PIDS_LIMIT: u32 = 512;
/// Default size of the writable `/tmp` the read-only root filesystem needs.
/// Docker's own `--tmpfs` default is 64 MiB, which is too small for document
/// conversion scratch, so the size is always named explicitly. `exec` is granted
/// because helper tooling legitimately writes and runs scratch programs.
const DEFAULT_TMPFS_OPTIONS: &str = "rw,exec,nosuid,nodev,size=1g";

const RUNTIME_UNAVAILABLE: &str = "container runtime is unavailable";
const NETWORK_CREATE_FAILED: &str = "could not create the sandbox's internal network";
const PROXY_RUN_FAILED: &str = "could not start the sandbox egress proxy";
const PROXY_CONNECT_FAILED: &str = "could not attach the egress proxy to the sandbox network";
const NETWORK_REMOVE_FAILED: &str = "could not remove the sandbox network";
const NETWORK_LIST_FAILED: &str = "could not list sandbox networks";
const RUNTIME_SPAWN_FAILED: &str = "could not invoke the container runtime";
const RUN_FAILED: &str = "could not start the sandbox container";
const PORT_LOOKUP_FAILED: &str = "could not resolve the sandbox's published port";
const INSPECT_FAILED: &str = "could not inspect the sandbox container";
const REMOVE_FAILED: &str = "could not remove the sandbox container";
const LIST_FAILED: &str = "could not list sandbox containers";

/// How this backend drives the container runtime.
#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// The runtime binary. `docker` by default; point it at `podman` for the
    /// Docker-CLI-compatible path. Either a bare name resolved on `PATH` or an
    /// explicit path.
    pub binary: String,
    /// The image ref to run. Defaults to the published documents image pinned
    /// by digest, or to the locally built development image while no digest is
    /// recorded; see [`default_image`].
    pub image: String,
    /// The in-container port to publish to a loopback host port.
    pub listener_port: u16,
    /// A command (and arguments) to run in the container, overriding the image's own
    /// entrypoint. Empty means the image's default is used — the real agent image
    /// needs no override; tests pass a trivial port-holding command.
    pub command: Vec<String>,
    /// A command (and arguments) for the egress-proxy container, overriding the
    /// image's entrypoint. Empty means the image's entrypoint runs with the
    /// `egress-proxy` argument — the real agent image's second face; tests on
    /// stand-in images pass a trivial port-holding command.
    pub proxy_command: Vec<String>,
    /// The absolute lifetime cap, in seconds, applied to a provisioning request
    /// that names none of its own. `None` disables the fallback, leaving a
    /// capless request uncapped; a request's own
    /// [`lifetime_cap_secs`](ProvisionRequest::lifetime_cap_secs) always wins.
    pub lifetime_cap_secs: Option<u64>,
    /// How tightly the container is confined. Defaults to the hardened profile;
    /// see [`SandboxHardening`].
    pub hardening: SandboxHardening,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            binary: DEFAULT_BINARY.to_owned(),
            image: default_image(),
            listener_port: DEFAULT_LISTENER_PORT,
            command: Vec::new(),
            proxy_command: Vec::new(),
            lifetime_cap_secs: Some(DEFAULT_LIFETIME_CAP_SECS),
            hardening: SandboxHardening::default(),
        }
    }
}

/// The confinement applied to a provisioned container.
///
/// The defaults are the profile the sandbox is meant to run under; the fields
/// exist so an operator can raise a ceiling for a workload that needs it, not so
/// the confinement is optional. Each `None` or `false` gives up a control, and
/// the container is the only thing standing between model-authored commands and
/// the host.
#[derive(Debug, Clone)]
pub struct SandboxHardening {
    /// The `uid:gid` the container runs as, forced from the host so an image
    /// without its own `USER` still cannot run as root. `None` leaves the
    /// image's user in effect.
    pub user: Option<String>,
    /// Drop every Linux capability. Nothing in the agent needs one: it binds an
    /// unprivileged port, runs shell children as itself, and changes no
    /// ownership.
    pub drop_capabilities: bool,
    /// Refuse privilege escalation, so a setuid binary inside the image cannot
    /// hand a model-authored command more than the sandbox user has.
    pub no_new_privileges: bool,
    /// Mount the root filesystem read-only, so a command cannot modify the image
    /// it runs from — the agent binary and the document helpers included. The
    /// writable surface is then exactly [`workspace_dir`](Self::workspace_dir)
    /// and the tmpfs mounts.
    pub read_only_rootfs: bool,
    /// The workspace path to back with a writable anonymous volume. Required
    /// once the root filesystem is read-only, since the agent's tools are rooted
    /// there. A volume rather than a tmpfs so workspace files live on disk and do
    /// not spend the container's memory budget; it is removed with the container.
    pub workspace_dir: Option<String>,
    /// Paths to mount as writable tmpfs, with their mount options. Needed for
    /// anything that writes outside the workspace under a read-only root.
    pub tmpfs: Vec<(String, String)>,
    /// Memory ceiling, in Docker's own size syntax.
    pub memory_limit: Option<String>,
    /// Process-count ceiling.
    pub pids_limit: Option<u32>,
    /// Run the container under an init process that reaps orphaned children, so
    /// a command that backgrounds work does not leave zombies behind the agent.
    /// The agent stays the init's only child, so the lifetime cap it enforces
    /// against itself still ends the container.
    pub init: bool,
}

impl Default for SandboxHardening {
    fn default() -> Self {
        Self {
            user: Some(SANDBOX_USER.to_owned()),
            drop_capabilities: true,
            no_new_privileges: true,
            read_only_rootfs: true,
            workspace_dir: Some(DEFAULT_WORKSPACE_DIR.to_owned()),
            tmpfs: vec![("/tmp".to_owned(), DEFAULT_TMPFS_OPTIONS.to_owned())],
            memory_limit: Some(DEFAULT_MEMORY_LIMIT.to_owned()),
            pids_limit: Some(DEFAULT_PIDS_LIMIT),
            init: true,
        }
    }
}

/// A [`SandboxBackend`] backed by a local Docker (or podman) container.
pub struct DockerSandboxBackend {
    config: DockerConfig,
}

impl DockerSandboxBackend {
    /// Build a backend from explicit configuration.
    #[must_use]
    pub fn new(config: DockerConfig) -> Self {
        Self { config }
    }

    /// Build a backend with the default configuration (the `docker` binary and the
    /// default image; see [`default_image`]).
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(DockerConfig::default())
    }

    /// Whether the configured container runtime binary is resolvable on this host.
    ///
    /// This is the detected-capability seam: the host checks it before choosing this
    /// backend, exactly as it checks Seatbelt support before the local exec adapter.
    #[must_use]
    pub fn is_available(&self) -> bool {
        resolve_on_path(&self.config.binary).is_some()
    }

    /// A fresh runtime command with stdio detached from this process.
    fn command(&self) -> Command {
        let mut command = Command::new(&self.config.binary);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// `docker rm -f -v`, treating a missing container as success. `-v` removes
    /// the anonymous workspace volume the provisioning created; without it every
    /// torn-down sandbox would leave its workspace dangling on the host's disk.
    /// Only anonymous volumes are affected, so nothing an operator named is
    /// touched.
    async fn remove(&self, reference: &str) -> Result<(), BackendError> {
        let mut command = self.command();
        command.args(["rm", "-f", "-v", reference]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Teardown(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if output.status.success() || is_no_such_container(&output.stderr) {
            Ok(())
        } else {
            Err(BackendError::Teardown(REMOVE_FAILED.to_owned()))
        }
    }

    /// List every container carrying the correlation-tag label, with its tag parsed.
    async fn list_tagged(&self) -> Result<Vec<SandboxHandle>, BackendError> {
        let mut command = self.command();
        command.args([
            "ps",
            "-a",
            "--filter",
            &format!("label={RUN_TAG_LABEL}"),
            "--format",
            TAG_LIST_FORMAT,
        ]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Teardown(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return Err(BackendError::Teardown(LIST_FAILED.to_owned()));
        }
        Ok(parse_tag_listing(&output.stdout))
    }

    /// Recover the per-run transport secret from the container's environment.
    async fn read_transport_secret(&self, reference: &str) -> Result<String, BackendError> {
        let mut command = self.command();
        command.args(["inspect", "--format", "{{json .Config.Env}}", reference]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Unaddressable(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return if is_no_such_container(&output.stderr) {
                Err(BackendError::UnknownHandle)
            } else {
                Err(BackendError::Unaddressable(INSPECT_FAILED.to_owned()))
            };
        }
        let env: Vec<String> = serde_json::from_slice(&output.stdout)
            .map_err(|_| BackendError::Unaddressable(INSPECT_FAILED.to_owned()))?;
        let prefix = format!("{TRANSPORT_SECRET_ENV}=");
        env.iter()
            .find_map(|entry| entry.strip_prefix(&prefix))
            .map(str::to_owned)
            .ok_or_else(|| BackendError::Unaddressable(INSPECT_FAILED.to_owned()))
    }
}

impl DockerSandboxBackend {
    /// Run one runtime invocation whose only interesting outcome is success.
    async fn invoke(&self, args: &[String], failure: &str) -> Result<Vec<u8>, BackendError> {
        let mut command = self.command();
        command.args(args);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Provision(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return Err(BackendError::Provision(failure.to_owned()));
        }
        Ok(output.stdout)
    }

    /// Best-effort removal of whatever a failed provisioning half-made. The
    /// caller still enqueues the durable teardown obligation, so the tag sweep
    /// re-covers anything this pass could not confirm.
    async fn unwind_provision(&self, tag: SandboxTag) {
        let _ = self.remove(&proxy_name(tag)).await;
        let _ = self.remove_network(tag).await;
    }

    /// `docker network rm`, treating a missing network as success.
    async fn remove_network(&self, tag: SandboxTag) -> Result<(), BackendError> {
        let mut command = self.command();
        command.args(["network", "rm", &network_name(tag)]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Teardown(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if output.status.success() || is_no_such_network(&output.stderr) {
            Ok(())
        } else {
            Err(BackendError::Teardown(NETWORK_REMOVE_FAILED.to_owned()))
        }
    }

    /// List every network carrying the correlation-tag label, with its tag parsed.
    async fn list_tagged_networks(&self) -> Result<Vec<(String, SandboxTag)>, BackendError> {
        let mut command = self.command();
        command.args([
            "network",
            "ls",
            "--filter",
            &format!("label={RUN_TAG_LABEL}"),
            "--format",
            NETWORK_TAG_LIST_FORMAT,
        ]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Teardown(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return Err(BackendError::Teardown(NETWORK_LIST_FAILED.to_owned()));
        }
        Ok(parse_network_tag_listing(&output.stdout))
    }
}

#[async_trait]
impl SandboxBackend for DockerSandboxBackend {
    /// Verified exactly when the configured ref is digest-pinned: a
    /// `repository@sha256:…` ref is content-addressed, so the daemon either
    /// resolves those bytes or fails the provisioning. A tag ref (the local
    /// development fallback, or an operator override without a digest) is not
    /// verified, and this reports it — fail closed, never assumed.
    fn verifies_image_integrity(&self) -> bool {
        image_digest_pinned(&self.config.image)
    }

    async fn provision(&self, request: ProvisionRequest) -> Result<SandboxHandle, BackendError> {
        if !self.is_available() {
            return Err(BackendError::Provision(RUNTIME_UNAVAILABLE.to_owned()));
        }
        let tag = request.tag;
        let cap = effective_lifetime_cap(&self.config, &request);
        let policy_json = serde_json::to_string(&request.network_policy)
            .map_err(|_| BackendError::Provision(RUN_FAILED.to_owned()))?;

        // 1. The internal network — the sandbox's only network, with no route
        //    out. Tagged so the orphan sweep reclaims it with the containers.
        self.invoke(&network_create_args(tag), NETWORK_CREATE_FAILED)
            .await?;

        // 2. The egress proxy, on the default bridge: outbound reach, the
        //    loopback-published transport port, and the compiled policy in its
        //    environment.
        if let Err(error) = self
            .invoke(
                &proxy_run_args(&self.config, tag, request.run_id, cap, &policy_json),
                PROXY_RUN_FAILED,
            )
            .await
        {
            self.unwind_provision(tag).await;
            return Err(error);
        }

        // 3. Dual-home the proxy onto the internal network under the alias the
        //    sandbox's HTTP(S)_PROXY environment names.
        if let Err(error) = self
            .invoke(&proxy_connect_args(tag), PROXY_CONNECT_FAILED)
            .await
        {
            self.unwind_provision(tag).await;
            return Err(error);
        }

        // 4. The sandbox itself, confined to the internal network.
        //
        // Docker enforces no lifetime cap from outside the container, and neither
        // can this process: a host-side timer dies with the host, which is the very
        // failure that strands a container. The cap therefore rides into the
        // container and the agent inside enforces it against itself. See the module
        // docs for why that is an absolute cap and not an idle timeout.
        let secret = Uuid::new_v4().to_string();
        let mut command = self.command();
        command.args(run_args(&self.config, tag, request.run_id, cap));
        // Set the secret on the runtime CLI's own environment and pass it through with
        // a valueless `--env`, so it never appears in this process's argv. The
        // delegated task never travels here at all: it arrives in the run-init
        // frame after the handle commits, so a sandbox reclaimed before that
        // point never executed anything.
        command.env(TRANSPORT_SECRET_ENV, &secret);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Provision(RUNTIME_SPAWN_FAILED.to_owned()));
        let reference = match output {
            Ok(output) if output.status.success() => parse_container_id(&output.stdout),
            _ => None,
        };
        let Some(reference) = reference else {
            self.unwind_provision(tag).await;
            return Err(BackendError::Provision(RUN_FAILED.to_owned()));
        };
        Ok(SandboxHandle { reference, tag })
    }

    async fn address(&self, handle: &SandboxHandle) -> Result<SandboxAddress, BackendError> {
        if !self.is_available() {
            return Err(BackendError::Unaddressable(RUNTIME_UNAVAILABLE.to_owned()));
        }
        // Resolve the published host port from `docker inspect` rather than
        // `docker port`: inspect returns the whole port map as JSON (parseable and
        // unit-testable without a daemon), and it does not depend on `docker port`'s
        // line format. The publish is loopback-only, so the binding's host IP is
        // 127.0.0.1 and `base_url` names it directly.
        //
        // The published port lives on the *proxy* container: the sandbox's only
        // network is internal, so its own port could not be published, and the
        // proxy relays the transport across the internal network. The proxy's
        // deterministic name is a pure function of the handle's tag, so this
        // remains recoverable after a host restart.
        let mut command = self.command();
        command.args([
            "inspect",
            "--format",
            "{{json .NetworkSettings.Ports}}",
            &proxy_name(handle.tag),
        ]);
        let output = command
            .output()
            .await
            .map_err(|_| BackendError::Unaddressable(RUNTIME_SPAWN_FAILED.to_owned()))?;
        if !output.status.success() {
            return if is_no_such_container(&output.stderr) {
                Err(BackendError::UnknownHandle)
            } else {
                Err(BackendError::Unaddressable(PORT_LOOKUP_FAILED.to_owned()))
            };
        }
        let port = parse_inspect_ports(&output.stdout, self.config.listener_port)
            .ok_or_else(|| BackendError::Unaddressable(PORT_LOOKUP_FAILED.to_owned()))?;
        let secret = self.read_transport_secret(&handle.reference).await?;
        Ok(SandboxAddress {
            base_url: base_url(port),
            transport_secret: TransportSecret::new(secret),
        })
    }

    async fn destroy(&self, handle: &SandboxHandle) -> Result<(), BackendError> {
        if !self.is_available() {
            return Err(BackendError::Teardown(RUNTIME_UNAVAILABLE.to_owned()));
        }
        // Containers before the network: a network with attached containers
        // refuses removal. Each step is idempotent on "already gone".
        self.remove(&handle.reference).await?;
        self.remove(&proxy_name(handle.tag)).await?;
        self.remove_network(handle.tag).await
    }

    /// Reclaim orphaned containers: destroy every tagged container whose
    /// correlation tag is *not* in `live_tags`.
    ///
    /// The reclaimable predicate is exactly "the container's stamped
    /// `openwave.run-tag` names no live run". Idempotent: a second sweep with
    /// the same live set finds the reclaimed containers already gone and
    /// reclaims nothing. `Ok` upholds the trait's guarantee that no container
    /// outside `live_tags` remains — an unavailable runtime or an unconfirmed
    /// removal is an error, so the sweep never marks a teardown obligation done
    /// on a pass that proved nothing.
    async fn reclaim_orphans(
        &self,
        live_tags: &HashSet<SandboxTag>,
    ) -> Result<Vec<SandboxHandle>, BackendError> {
        if !self.is_available() {
            return Err(BackendError::Teardown(RUNTIME_UNAVAILABLE.to_owned()));
        }
        let mut reclaimed = Vec::new();
        let mut unconfirmed = false;
        // The egress-proxy containers carry the same tag label, so one listing
        // covers both halves of a run's pair.
        for handle in self.list_tagged().await? {
            if live_tags.contains(&handle.tag) {
                continue;
            }
            if self.remove(&handle.reference).await.is_ok() {
                reclaimed.push(handle);
            } else {
                unconfirmed = true;
            }
        }
        // Networks after their containers, for the same attachment reason as
        // `destroy`.
        for (_, tag) in self.list_tagged_networks().await? {
            if live_tags.contains(&tag) {
                continue;
            }
            if self.remove_network(tag).await.is_err() {
                unconfirmed = true;
            }
        }
        if unconfirmed {
            return Err(BackendError::Teardown(REMOVE_FAILED.to_owned()));
        }
        Ok(reclaimed)
    }
}

/// The cap this provisioning runs under: the request's own if it names one, else
/// the backend's configured fallback. A zero cap is treated as no cap rather than
/// as "expire immediately", so a misconfigured zero cannot make every sandbox die
/// on arrival.
fn effective_lifetime_cap(config: &DockerConfig, request: &ProvisionRequest) -> Option<u64> {
    request
        .lifetime_cap_secs
        .or(config.lifetime_cap_secs)
        .filter(|secs| *secs > 0)
}

/// The default image ref: the published documents image pinned by digest once
/// [`PUBLISHED_IMAGE_DIGEST`] is recorded, the locally built development image
/// until then. A digest-pinned ref is what makes the default verified — see
/// the module docs' image-verification section.
fn default_image() -> String {
    match PUBLISHED_IMAGE_DIGEST {
        Some(digest) => format!("{PUBLISHED_IMAGE_REPOSITORY}@{digest}"),
        None => LOCAL_DEV_IMAGE.to_owned(),
    }
}

/// Whether an image ref is pinned by a well-formed content digest
/// (`…@sha256:<64 hex>`), making its resolution content-addressed and
/// therefore fail-closed on any mismatch.
pub(crate) fn image_digest_pinned(image: &str) -> bool {
    image
        .rsplit_once("@sha256:")
        .is_some_and(|(repository, hex)| {
            !repository.is_empty()
                && hex.len() == 64
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// The per-run internal network's name — a pure function of the tag, so
/// teardown and restart recovery need no extra state.
fn network_name(tag: SandboxTag) -> String {
    format!("openwave-net-{tag}")
}

/// The per-run egress-proxy container's name.
fn proxy_name(tag: SandboxTag) -> String {
    format!("openwave-egress-{tag}")
}

/// The per-run sandbox container's name: the DNS name the proxy's transport
/// relay dials on the internal network. Crate-visible so the Docker-gated
/// e2e egress probe can `docker exec` into the sandbox by name.
pub(crate) fn sandbox_name(tag: SandboxTag) -> String {
    format!("openwave-sbx-{tag}")
}

/// Compile a chat's provider-neutral [`NetworkPolicy`] into the closed,
/// class-expanded form a sandbox backend enforces.
///
/// Persisted policies are already normalized (exact lowercase hosts, no
/// wildcards), but the filter here is defensive rather than trusting: an entry
/// that fails to parse as an exact domain pattern is dropped — narrowing, never
/// widening — instead of shipped to the enforcement point.
pub(crate) fn compile_network_policy(policy: &NetworkPolicy) -> SandboxNetworkPolicy {
    let package_domains = || {
        openwave_code_execution::PACKAGE_MANAGER_DOMAINS
            .iter()
            .map(|domain| (*domain).to_owned())
            .collect::<Vec<_>>()
    };
    match policy {
        NetworkPolicy::Off => SandboxNetworkPolicy::deny_all(),
        NetworkPolicy::Open => SandboxNetworkPolicy::open(),
        NetworkPolicy::PackageManagers => SandboxNetworkPolicy {
            allow_all_public: false,
            allowed_hosts: Vec::new(),
            https_only_hosts: package_domains(),
        },
        NetworkPolicy::AllowedHosts {
            allowed_hosts,
            package_managers,
        } => SandboxNetworkPolicy {
            allow_all_public: false,
            allowed_hosts: allowed_hosts
                .iter()
                .filter(|host| !host.starts_with("*."))
                .filter_map(|host| DomainPattern::parse(host).ok().map(|_| host.to_owned()))
                .collect(),
            https_only_hosts: if *package_managers {
                package_domains()
            } else {
                Vec::new()
            },
        },
    }
}

/// The `docker network create` argument vector for one provisioning: an
/// *internal* network — no route out — tagged for the orphan sweep.
fn network_create_args(tag: SandboxTag) -> Vec<String> {
    vec![
        "network".to_owned(),
        "create".to_owned(),
        "--internal".to_owned(),
        "--label".to_owned(),
        format!("{RUN_TAG_LABEL}={tag}"),
        network_name(tag),
    ]
}

/// The `docker run` argument vector for the egress-proxy container. It runs on
/// the default bridge (outbound reach) with the transport port published to
/// host loopback and the compiled policy in its environment; a later
/// `network connect` dual-homes it onto the internal network.
fn proxy_run_args(
    config: &DockerConfig,
    tag: SandboxTag,
    run_id: RunId,
    lifetime_cap_secs: Option<u64>,
    policy_json: &str,
) -> Vec<String> {
    let mut args = vec![
        "run".to_owned(),
        "-d".to_owned(),
        "--name".to_owned(),
        proxy_name(tag),
        "--label".to_owned(),
        format!("{RUN_TAG_LABEL}={tag}"),
        "--label".to_owned(),
        format!("{RUN_ID_LABEL}={run_id}"),
        "--env".to_owned(),
        format!("{EGRESS_POLICY_ENV}={policy_json}"),
        "--env".to_owned(),
        format!("{RELAY_LISTEN_ENV}=0.0.0.0:{}", config.listener_port),
        "--env".to_owned(),
        format!(
            "{RELAY_TARGET_ENV}={}:{}",
            sandbox_name(tag),
            config.listener_port
        ),
    ];
    // The proxy dies on its own exactly like a stranded sandbox.
    if let Some(secs) = lifetime_cap_secs {
        args.push("--env".to_owned());
        args.push(format!("{LIFETIME_CAP_ENV}={secs}"));
    }
    args.extend(proxy_hardening_args(&config.hardening));
    args.extend([
        "--publish".to_owned(),
        format!("127.0.0.1::{}", config.listener_port),
        config.image.clone(),
    ]);
    if config.proxy_command.is_empty() {
        args.push("egress-proxy".to_owned());
    } else {
        args.extend(config.proxy_command.iter().cloned());
    }
    args
}

/// The proxy's confinement: the sandbox profile minus the writable surface —
/// the proxy writes nothing — and minus `--init`, since it never spawns
/// children. It is trusted code, but it faces the untrusted network on one side
/// and the untrusted sandbox on the other, so it keeps the non-root,
/// no-capability, read-only, ceiling-bounded profile.
fn proxy_hardening_args(hardening: &SandboxHardening) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(user) = &hardening.user {
        args.extend(["--user".to_owned(), user.clone()]);
    }
    if hardening.drop_capabilities {
        args.extend(["--cap-drop".to_owned(), "ALL".to_owned()]);
    }
    if hardening.no_new_privileges {
        args.extend([
            "--security-opt".to_owned(),
            "no-new-privileges:true".to_owned(),
        ]);
    }
    if hardening.read_only_rootfs {
        args.push("--read-only".to_owned());
    }
    if let Some(memory) = &hardening.memory_limit {
        args.extend(["--memory".to_owned(), memory.clone()]);
    }
    if let Some(pids) = hardening.pids_limit {
        args.extend(["--pids-limit".to_owned(), pids.to_string()]);
    }
    args
}

/// The `docker network connect` argument vector that dual-homes the proxy onto
/// the internal network under the alias the sandbox's proxy environment names.
fn proxy_connect_args(tag: SandboxTag) -> Vec<String> {
    vec![
        "network".to_owned(),
        "connect".to_owned(),
        "--alias".to_owned(),
        EGRESS_PROXY_ALIAS.to_owned(),
        network_name(tag),
        proxy_name(tag),
    ]
}

/// The `docker run` argument vector for the sandbox container. Factored out so
/// the label, network, and env-passthrough composition is testable without a
/// runtime.
fn run_args(
    config: &DockerConfig,
    tag: SandboxTag,
    run_id: RunId,
    lifetime_cap_secs: Option<u64>,
) -> Vec<String> {
    let proxy_url = format!("http://{EGRESS_PROXY_ALIAS}:{EGRESS_PROXY_PORT}");
    let mut args = vec![
        "run".to_owned(),
        "-d".to_owned(),
        "--name".to_owned(),
        sandbox_name(tag),
        // The sandbox's ONLY network: internal, so a command that ignores the
        // proxy environment has no route anywhere. No port is published here —
        // it could not reach the host anyway; the proxy relays the transport.
        "--network".to_owned(),
        network_name(tag),
        // External DNS points at a blackhole: the proxy resolves CONNECT
        // destinations host-side, so the sandbox needs only the internal
        // network's own names, which the embedded resolver answers without
        // consulting this upstream.
        "--dns".to_owned(),
        SANDBOX_DNS_SINK.to_owned(),
        "--label".to_owned(),
        format!("{RUN_TAG_LABEL}={tag}"),
        "--label".to_owned(),
        format!("{RUN_ID_LABEL}={run_id}"),
        "--env".to_owned(),
        TRANSPORT_SECRET_ENV.to_owned(),
        // Convenience for compliant tools; the topology is the enforcement.
        "--env".to_owned(),
        format!("HTTP_PROXY={proxy_url}"),
        "--env".to_owned(),
        format!("HTTPS_PROXY={proxy_url}"),
        "--env".to_owned(),
        format!("http_proxy={proxy_url}"),
        "--env".to_owned(),
        format!("https_proxy={proxy_url}"),
    ];
    args.extend(hardening_args(&config.hardening));
    // Unlike the secret, the cap is not sensitive, so it travels as
    // an ordinary `--env NAME=value` rather than by name through this process's
    // own environment.
    if let Some(secs) = lifetime_cap_secs {
        args.push("--env".to_owned());
        args.push(format!("{LIFETIME_CAP_ENV}={secs}"));
    }
    args.push(config.image.clone());
    args.extend(config.command.iter().cloned());
    args
}

/// The confinement flags for one provisioning. Separate from [`run_args`] so the
/// security-relevant part of the argv is one readable list: a control silently
/// dropped here is invisible at runtime, since a container missing them starts
/// and serves exactly like a hardened one.
fn hardening_args(hardening: &SandboxHardening) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(user) = &hardening.user {
        args.extend(["--user".to_owned(), user.clone()]);
    }
    if hardening.drop_capabilities {
        args.extend(["--cap-drop".to_owned(), "ALL".to_owned()]);
    }
    if hardening.no_new_privileges {
        args.extend([
            "--security-opt".to_owned(),
            "no-new-privileges:true".to_owned(),
        ]);
    }
    if hardening.read_only_rootfs {
        args.push("--read-only".to_owned());
    }
    // The workspace is mounted whether or not the root filesystem is read-only:
    // the agent's tools write there either way, and an anonymous volume keeps
    // that writing off the container's writable layer. `remove` passes `-v`, so
    // the volume dies with the container rather than accumulating.
    if let Some(workspace) = &hardening.workspace_dir {
        args.extend(["--volume".to_owned(), workspace.clone()]);
    }
    for (path, options) in &hardening.tmpfs {
        args.extend(["--tmpfs".to_owned(), format!("{path}:{options}")]);
    }
    if let Some(memory) = &hardening.memory_limit {
        args.extend(["--memory".to_owned(), memory.clone()]);
    }
    if let Some(pids) = hardening.pids_limit {
        args.extend(["--pids-limit".to_owned(), pids.to_string()]);
    }
    if hardening.init {
        args.push("--init".to_owned());
    }
    args
}

/// The loopback base URL for a published host port.
fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The container id printed by `docker run -d`: the first non-empty hex line.
fn parse_container_id(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let id = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    (id.len() >= 12 && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| id.to_owned())
}

/// One host-side binding of a container port, as `docker inspect` renders it.
#[derive(serde::Deserialize)]
struct PortBinding {
    #[serde(rename = "HostIp")]
    host_ip: Option<String>,
    #[serde(rename = "HostPort")]
    host_port: Option<String>,
}

/// The published host port for `container_port` from the JSON `docker inspect
/// {{json .NetworkSettings.Ports}}` renders, e.g.
/// `{"8080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49155"}]}`. A container that
/// publishes no such port renders the key as `null` (or omits it), which yields
/// `None`. A loopback binding is preferred so the resolved port matches the
/// loopback `base_url`.
fn parse_inspect_ports(stdout: &[u8], container_port: u16) -> Option<u16> {
    let ports: std::collections::HashMap<String, Option<Vec<PortBinding>>> =
        serde_json::from_slice(stdout).ok()?;
    let bindings = ports.get(&format!("{container_port}/tcp"))?.as_ref()?;
    let binding = bindings
        .iter()
        .find(|binding| is_loopback(binding.host_ip.as_deref()))
        .or_else(|| bindings.first())?;
    binding
        .host_port
        .as_deref()?
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
}

/// Whether a binding's host IP is a loopback address.
fn is_loopback(host_ip: Option<&str>) -> bool {
    matches!(host_ip, Some("127.0.0.1" | "::1"))
}

/// Parse the id/tag pairs from the tag-listing template's tab-separated output.
fn parse_tag_listing(stdout: &[u8]) -> Vec<SandboxHandle> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .filter_map(|line| {
            let (id, tag) = line.trim().split_once('\t')?;
            let (id, tag) = (id.trim(), tag.trim());
            if id.is_empty() {
                return None;
            }
            // A tag that does not parse as a valid identity is a label this backend
            // did not mint; leave it alone rather than reclaim something foreign.
            let tag = tag.parse::<SandboxTag>().ok()?;
            Some(SandboxHandle {
                reference: id.to_owned(),
                tag,
            })
        })
        .collect()
}

/// Parse the name/tag pairs from the network tag-listing template's output.
fn parse_network_tag_listing(stdout: &[u8]) -> Vec<(String, SandboxTag)> {
    let text = String::from_utf8_lossy(stdout);
    text.lines()
        .filter_map(|line| {
            let (name, tag) = line.trim().split_once('\t')?;
            let (name, tag) = (name.trim(), tag.trim());
            if name.is_empty() {
                return None;
            }
            let tag = tag.parse::<SandboxTag>().ok()?;
            Some((name.to_owned(), tag))
        })
        .collect()
}

/// Whether a runtime error names a container that does not exist — the idempotent
/// case for destroy, and the unknown-handle case for address. Covers both Docker
/// (`No such container`) and podman (`no such container` / `no such object`).
fn is_no_such_container(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    text.contains("no such container") || text.contains("no such object")
}

/// Whether a runtime error names a network that does not exist — the idempotent
/// case for network removal. Docker phrases it `network <name> not found` (the
/// name sits between the words, so the match is on the bare suffix); podman
/// uses `unable to find network` / `no such network`.
fn is_no_such_network(stderr: &[u8]) -> bool {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    text.contains("no such network")
        || text.contains("not found")
        || text.contains("unable to find network")
}

/// Resolve a runtime binary the way a shell would: an explicit path is checked as
/// given, a bare name is searched on `PATH`. Returns the resolved path if it names
/// an executable file.
fn resolve_on_path(binary: &str) -> Option<PathBuf> {
    let candidate = Path::new(binary);
    if candidate.components().count() > 1 {
        return is_executable_file(candidate).then(|| candidate.to_path_buf());
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| {
        let full = dir.join(binary);
        is_executable_file(&full).then_some(full)
    })
}

/// Whether `path` is a regular file with an executable bit (on unix) or simply a
/// file (elsewhere).
fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend pointed at a runtime binary that cannot exist.
    fn unavailable_backend() -> DockerSandboxBackend {
        DockerSandboxBackend::new(DockerConfig {
            binary: "openwave-nonexistent-runtime-xyzzy".to_owned(),
            ..DockerConfig::default()
        })
    }

    #[test]
    fn detection_reports_unavailable_when_the_binary_is_absent() {
        assert!(!unavailable_backend().is_available());
        // A real shell builtin path that does exist resolves; `/bin/sh` stands in
        // for a present runtime binary so the positive branch is covered too.
        #[cfg(unix)]
        assert!(resolve_on_path("/bin/sh").is_some());
    }

    /// The default-image pin's honesty invariants, guarding the decision that
    /// is easiest to reverse by accident: a recorded [`PUBLISHED_IMAGE_DIGEST`]
    /// must be well-formed and make the default verified; while unset the
    /// default must be the local development build and be *reported* as
    /// unverified rather than assumed safe.
    #[test]
    fn default_image_pin_is_wellformed_and_reported_honestly() {
        let default = DockerConfig::default().image;
        match PUBLISHED_IMAGE_DIGEST {
            Some(digest) => {
                assert_eq!(default, format!("{PUBLISHED_IMAGE_REPOSITORY}@{digest}"));
                assert!(PUBLISHED_IMAGE_REPOSITORY.starts_with("ghcr.io/brightwave-inc/"));
                assert!(image_digest_pinned(&default));
                assert!(DockerSandboxBackend::with_defaults().verifies_image_integrity());
            }
            None => {
                assert_eq!(default, LOCAL_DEV_IMAGE);
                assert!(!DockerSandboxBackend::with_defaults().verifies_image_integrity());
            }
        }

        // The pin grammar itself: exactly a 64-hex sha256 suffix counts.
        let digest_ref = format!("ghcr.io/brightwave-inc/example@sha256:{}", "a".repeat(64));
        assert!(image_digest_pinned(&digest_ref));
        assert!(!image_digest_pinned(
            "ghcr.io/brightwave-inc/example:latest"
        ));
        assert!(!image_digest_pinned(&format!(
            "ghcr.io/brightwave-inc/example@sha256:{}",
            "a".repeat(63)
        )));
        assert!(!image_digest_pinned(&format!("@sha256:{}", "a".repeat(64))));
    }

    #[tokio::test]
    async fn trait_methods_fail_normalized_when_the_runtime_is_absent() {
        let backend = unavailable_backend();
        let request = ProvisionRequest {
            run_id: RunId::new(),
            tag: SandboxTag::new(),
            lifetime_cap_secs: None,
            network_policy: Default::default(),
        };
        assert!(matches!(
            backend.provision(request).await,
            Err(BackendError::Provision(_))
        ));
        let handle = SandboxHandle {
            reference: "deadbeefcafe".to_owned(),
            tag: SandboxTag::new(),
        };
        assert!(matches!(
            backend.address(&handle).await,
            Err(BackendError::Unaddressable(_))
        ));
        assert!(matches!(
            backend.destroy(&handle).await,
            Err(BackendError::Teardown(_))
        ));
        // With no runtime the sweep proves nothing, so it must error rather
        // than report an Ok the caller would read as "no orphans remain".
        assert!(matches!(
            backend.reclaim_orphans(&HashSet::new()).await,
            Err(BackendError::Teardown(_))
        ));
    }

    #[test]
    fn run_args_stamp_the_tag_confine_the_network_and_append_the_command() {
        let tag = SandboxTag::new();
        let run_id = RunId::new();
        let config = DockerConfig {
            listener_port: 9000,
            image: "example/image:tag".to_owned(),
            command: vec!["sleep".to_owned(), "infinity".to_owned()],
            ..DockerConfig::default()
        };
        let args = run_args(&config, tag, run_id, Some(900));

        assert_eq!(args[0], "run");
        assert!(args.contains(&"-d".to_owned()));
        assert!(args.contains(&format!("{RUN_TAG_LABEL}={tag}")));
        assert!(args.contains(&format!("{RUN_ID_LABEL}={run_id}")));
        // The secret is passed by name only, never as an argv value.
        assert!(args.contains(&"--env".to_owned()));
        assert!(args.contains(&TRANSPORT_SECRET_ENV.to_owned()));
        assert!(args
            .iter()
            .all(|arg| !arg.contains(TRANSPORT_SECRET_ENV) || arg == TRANSPORT_SECRET_ENV));

        // The sandbox's only network is the per-run internal network, and it
        // publishes NO port: the proxy relays the transport, and a published
        // port here would be dead weight at best.
        let network_at = args.iter().position(|arg| arg == "--network").unwrap();
        assert_eq!(args[network_at + 1], network_name(tag));
        assert!(!args.iter().any(|arg| arg == "--publish"));
        // External DNS is blackholed: the embedded resolver still answers the
        // internal network's names, and the proxy resolves CONNECT
        // destinations, so nothing legitimate needs an external lookup.
        let dns_at = args.iter().position(|arg| arg == "--dns").unwrap();
        assert_eq!(args[dns_at + 1], SANDBOX_DNS_SINK);
        // Compliant tools are pointed at the proxy by every spelling.
        for var in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            assert!(args.contains(&format!(
                "{var}=http://{EGRESS_PROXY_ALIAS}:{EGRESS_PROXY_PORT}"
            )));
        }
        // The deterministic name is what the proxy's relay dials.
        let name_at = args.iter().position(|arg| arg == "--name").unwrap();
        assert_eq!(args[name_at + 1], sandbox_name(tag));

        // The image precedes the container command.
        let image_at = args
            .iter()
            .position(|arg| arg == "example/image:tag")
            .unwrap();
        let command_at = args.iter().position(|arg| arg == "sleep").unwrap();
        assert!(image_at < command_at);
        assert_eq!(args.last().unwrap(), "infinity");

        // The cap reaches the container as a value, since the agent inside is what
        // enforces it.
        assert!(args.contains(&format!("{LIFETIME_CAP_ENV}=900")));

        // An uncapped provisioning names no cap at all rather than passing a zero
        // the container would read as "expire now".
        let uncapped = run_args(&config, tag, run_id, None);
        assert!(uncapped.iter().all(|arg| !arg.contains(LIFETIME_CAP_ENV)));
    }

    /// The proxy container is the only dual-homed, host-published piece, and it
    /// carries the compiled policy. Every property here is invisible at runtime
    /// if silently dropped, so each is pinned.
    #[test]
    fn proxy_and_network_args_compose_the_egress_topology() {
        let tag = SandboxTag::new();
        let run_id = RunId::new();
        let config = DockerConfig {
            listener_port: 9000,
            image: "example/image:tag".to_owned(),
            ..DockerConfig::default()
        };

        // The network is internal — the property the whole boundary rests on.
        let net = network_create_args(tag);
        assert!(net.contains(&"--internal".to_owned()));
        assert!(net.contains(&format!("{RUN_TAG_LABEL}={tag}")));
        assert_eq!(net.last().unwrap(), &network_name(tag));

        let policy = r#"{"allow_all_public":false}"#;
        let args = proxy_run_args(&config, tag, run_id, Some(900), policy);
        // Published to host loopback: this is the transport's front door.
        assert!(args.contains(&"127.0.0.1::9000".to_owned()));
        // Policy, relay wiring, and the lifetime cap ride the environment.
        assert!(args.contains(&format!("{EGRESS_POLICY_ENV}={policy}")));
        assert!(args.contains(&format!("{RELAY_LISTEN_ENV}=0.0.0.0:9000")));
        assert!(args.contains(&format!("{RELAY_TARGET_ENV}={}:9000", sandbox_name(tag))));
        assert!(args.contains(&format!("{LIFETIME_CAP_ENV}=900")));
        // The default command is the agent image's second face.
        assert_eq!(args.last().unwrap(), "egress-proxy");
        // The proxy keeps the non-root, no-capability, read-only profile.
        assert!(args.contains(&"--cap-drop".to_owned()));
        assert!(args.contains(&"--read-only".to_owned()));
        // It is tagged for the sweep and deterministically named for recovery.
        assert!(args.contains(&format!("{RUN_TAG_LABEL}={tag}")));
        assert!(args.contains(&proxy_name(tag)));
        // The proxy resolves CONNECT destinations for the sandbox, so it must
        // keep real DNS — the blackhole belongs to the sandbox only.
        assert!(!args.iter().any(|arg| arg == "--dns"));

        // A test image without the agent binary can stand in a port-holder.
        let stand_in = DockerConfig {
            proxy_command: vec!["nc".to_owned(), "-l".to_owned()],
            ..config.clone()
        };
        let args = proxy_run_args(&stand_in, tag, run_id, None, policy);
        assert_eq!(args.last().unwrap(), "-l");

        // The connect dual-homes the proxy under the alias the sandbox's proxy
        // environment names.
        let connect = proxy_connect_args(tag);
        assert_eq!(
            connect,
            vec![
                "network".to_owned(),
                "connect".to_owned(),
                "--alias".to_owned(),
                EGRESS_PROXY_ALIAS.to_owned(),
                network_name(tag),
                proxy_name(tag),
            ]
        );
    }

    /// Host-side policy compilation: classes expand to exact hosts, deny stays
    /// deny, and nothing ever widens.
    #[test]
    fn network_policy_compiles_to_the_closed_sandbox_form() {
        assert!(compile_network_policy(&NetworkPolicy::Off).denies_everything());

        let open = compile_network_policy(&NetworkPolicy::Open);
        assert!(open.allow_all_public);

        let packages = compile_network_policy(&NetworkPolicy::PackageManagers);
        assert!(!packages.allow_all_public);
        assert!(packages.allowed_hosts.is_empty());
        assert!(packages
            .https_only_hosts
            .iter()
            .any(|host| host == "pypi.org"));
        assert!(packages.permits("pypi.org", 443));
        assert!(!packages.permits("pypi.org", 80));

        let custom = compile_network_policy(&NetworkPolicy::AllowedHosts {
            allowed_hosts: vec![
                "api.example.com".to_owned(),
                // Defensive: wildcards and malformed entries are dropped, not
                // shipped — narrowing, never widening.
                "*.unsafe.example".to_owned(),
                "not a host".to_owned(),
            ],
            package_managers: true,
        });
        assert_eq!(custom.allowed_hosts, vec!["api.example.com".to_owned()]);
        assert!(custom.permits("crates.io", 443));
        assert!(!custom.permits("x.unsafe.example", 443));
    }

    #[test]
    fn network_tag_listing_parser_keeps_valid_tags() {
        let tag = SandboxTag::new();
        let stdout = format!("openwave-net-{tag}\t{tag}\nother-net\tnot-a-uuid\n");
        let networks = parse_network_tag_listing(stdout.as_bytes());
        assert_eq!(networks, vec![(network_name(tag), tag)]);
    }

    /// Every confinement control reaches the argv. A container missing them
    /// starts, serves, and passes every other test in this file identically, so
    /// dropping one is invisible without an assertion that names it.
    #[test]
    fn run_args_carry_the_container_confinement() {
        let config = DockerConfig::default();
        let args = run_args(&config, SandboxTag::new(), RunId::new(), None);
        let pair = |flag: &str| {
            args.iter()
                .position(|arg| arg == flag)
                .and_then(|at| args.get(at + 1))
                .cloned()
        };

        // Non-root, with no capabilities and no path to acquiring more.
        assert_eq!(pair("--user").as_deref(), Some(SANDBOX_USER));
        assert_eq!(pair("--cap-drop").as_deref(), Some("ALL"));
        assert_eq!(
            pair("--security-opt").as_deref(),
            Some("no-new-privileges:true")
        );

        // A read-only image, with the workspace and /tmp as the only writable
        // surface — a read-only root with no writable workspace would break the
        // agent's tools outright.
        assert!(args.contains(&"--read-only".to_owned()));
        assert_eq!(pair("--volume").as_deref(), Some(DEFAULT_WORKSPACE_DIR));
        assert_eq!(
            pair("--tmpfs").as_deref(),
            Some(format!("/tmp:{DEFAULT_TMPFS_OPTIONS}").as_str())
        );

        // Ceilings, and an init to reap what a model-authored command orphans.
        assert_eq!(pair("--memory").as_deref(), Some(DEFAULT_MEMORY_LIMIT));
        assert_eq!(
            pair("--pids-limit").as_deref(),
            Some(DEFAULT_PIDS_LIMIT.to_string().as_str())
        );
        assert!(args.contains(&"--init".to_owned()));

        // Every flag precedes the image, or Docker would read it as an argument
        // to the container command instead of applying it.
        let image_at = args.iter().position(|arg| arg == &config.image).unwrap();
        assert_eq!(image_at, args.len() - 1);
    }

    #[test]
    fn the_request_cap_wins_over_the_configured_fallback() {
        let request = |lifetime_cap_secs| ProvisionRequest {
            run_id: RunId::new(),
            tag: SandboxTag::new(),
            lifetime_cap_secs,
            network_policy: Default::default(),
        };
        let config = DockerConfig::default();
        assert_eq!(config.lifetime_cap_secs, Some(DEFAULT_LIFETIME_CAP_SECS));

        // A request that names a cap gets exactly that one; one that does not
        // still gets capped, so nothing provisions with an unbounded lifetime by
        // omission.
        assert_eq!(
            effective_lifetime_cap(&config, &request(Some(60))),
            Some(60)
        );
        assert_eq!(
            effective_lifetime_cap(&config, &request(None)),
            Some(DEFAULT_LIFETIME_CAP_SECS)
        );

        // Zero from either side means uncapped, never expire-on-arrival.
        assert_eq!(effective_lifetime_cap(&config, &request(Some(0))), None);
        let uncapped = DockerConfig {
            lifetime_cap_secs: None,
            ..DockerConfig::default()
        };
        assert_eq!(effective_lifetime_cap(&uncapped, &request(None)), None);
    }

    #[test]
    fn tag_list_format_tracks_the_label_constant() {
        assert!(TAG_LIST_FORMAT.contains(RUN_TAG_LABEL));
        assert!(NETWORK_TAG_LIST_FORMAT.contains(RUN_TAG_LABEL));
    }

    #[test]
    fn inspect_ports_resolve_the_published_host_port() {
        // The published-port shape `docker inspect {{json .NetworkSettings.Ports}}`
        // renders, with the loopback binding chosen.
        let published = br#"{"8080/tcp":[{"HostIp":"127.0.0.1","HostPort":"49155"}]}"#;
        assert_eq!(parse_inspect_ports(published, 8080), Some(49_155));

        // Two bindings: the loopback one wins over a wildcard one.
        let dual = br#"{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"7000"},{"HostIp":"127.0.0.1","HostPort":"49200"}]}"#;
        assert_eq!(parse_inspect_ports(dual, 8080), Some(49_200));

        // An unpublished port renders as a null binding.
        let unbound = br#"{"8080/tcp":null}"#;
        assert_eq!(parse_inspect_ports(unbound, 8080), None);

        // A different container port than the one asked for.
        let other = br#"{"9090/tcp":[{"HostIp":"127.0.0.1","HostPort":"49155"}]}"#;
        assert_eq!(parse_inspect_ports(other, 8080), None);

        // A container with no published ports at all.
        assert_eq!(parse_inspect_ports(b"{}", 8080), None);
        // Malformed output resolves to nothing rather than panicking.
        assert_eq!(parse_inspect_ports(b"not json", 8080), None);
    }

    #[test]
    fn container_id_parsing_accepts_hex_and_rejects_noise() {
        assert_eq!(
            parse_container_id(b"3f2a9c1d4e5b6a7c8d9e0f1a2b3c4d5e\n").as_deref(),
            Some("3f2a9c1d4e5b6a7c8d9e0f1a2b3c4d5e")
        );
        assert_eq!(
            parse_container_id(b"Unable to find image\n").as_deref(),
            None
        );
        assert_eq!(parse_container_id(b"\n").as_deref(), None);
    }

    #[test]
    fn tag_listing_parser_keeps_valid_tags_and_drops_foreign_ones() {
        let tag = SandboxTag::new();
        let stdout = format!("abc123abc123\t{tag}\ndef456def456\tnot-a-uuid\n\t{tag}\n");
        let handles = parse_tag_listing(stdout.as_bytes());
        assert_eq!(handles.len(), 1);
        assert_eq!(handles[0].reference, "abc123abc123");
        assert_eq!(handles[0].tag, tag);
    }

    #[test]
    fn missing_container_is_recognized_across_runtimes() {
        assert!(is_no_such_container(b"Error: No such container: abc"));
        assert!(is_no_such_container(b"error: no such object abc"));
        assert!(!is_no_such_container(b"Error: container is restarting"));
    }

    /// The missing-network phrasings that make repeated teardown idempotent.
    /// Docker interpolates the name between "network" and "not found", which is
    /// exactly the case a stricter phrase match gets wrong.
    #[test]
    fn missing_network_is_recognized_across_runtimes() {
        assert!(is_no_such_network(
            b"Error response from daemon: network openwave-net-abc not found"
        ));
        assert!(is_no_such_network(b"Error: no such network abc"));
        assert!(is_no_such_network(
            b"Error: unable to find network with name or ID abc: network not found"
        ));
        assert!(!is_no_such_network(
            b"Error response from daemon: network openwave-net-abc has active endpoints"
        ));
    }

    // --- Live-Docker tests, skipped cleanly when no runtime is present. ---

    /// A backend on a trivial public image that stays running and holds its
    /// listener port.
    fn live_backend() -> DockerSandboxBackend {
        DockerSandboxBackend::new(DockerConfig {
            image: "alpine:3.20".to_owned(),
            listener_port: 8080,
            // A foreground shell loop that keeps the container alive and re-listens
            // on 8080 with busybox `nc` (present in alpine's base busybox, unlike
            // `httpd`). The loop never exits, so the container stays running and its
            // published port keeps a real listener behind it; `|| sleep 1` avoids a
            // busy spin if a listen attempt ever fails.
            command: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "while true; do nc -l -p 8080 || sleep 1; done".to_owned(),
            ],
            // The stand-in image has no agent binary, so the egress-proxy
            // container holds its published port the same way. Reachability of
            // the published port then proves the topology stood up (network,
            // dual-homed proxy, sandbox), not the relay's forwarding — that is
            // the sandbox-resident e2e lane's to prove.
            proxy_command: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "while true; do nc -l -p 8080 || sleep 1; done".to_owned(),
            ],
            ..DockerConfig::default()
        })
    }

    /// Poll until the container reports `State.Running`, so addressing and the
    /// reachability probe do not race container startup.
    async fn wait_running(backend: &DockerSandboxBackend, reference: &str) -> bool {
        for _ in 0..50 {
            let mut command = backend.command();
            command.args(["inspect", "--format", "{{.State.Running}}", reference]);
            if let Ok(output) = command.output().await {
                if output.status.success()
                    && String::from_utf8_lossy(&output.stdout).trim() == "true"
                {
                    return true;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        false
    }

    /// Every tag currently stamped on the daemon, so a sweep in the test treats
    /// pre-existing containers (a dev machine's real ones) as live and never reaps
    /// anything but the orphan this test creates.
    async fn existing_tags(backend: &DockerSandboxBackend) -> HashSet<SandboxTag> {
        backend
            .list_tagged()
            .await
            .unwrap()
            .into_iter()
            .map(|handle| handle.tag)
            .collect()
    }

    /// Whether the runtime's daemon answers, so a present binary with a stopped
    /// daemon skips the live test rather than failing it.
    async fn daemon_ready(backend: &DockerSandboxBackend) -> bool {
        let mut command = backend.command();
        command.args(["version", "--format", "{{.Server.Version}}"]);
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await,
            Ok(Ok(output)) if output.status.success()
        )
    }

    async fn tcp_reachable(base_url: &str) -> bool {
        let authority = base_url.trim_start_matches("http://");
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(authority).await.is_ok() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn docker_lifecycle_provision_address_destroy_and_orphan_sweep() {
        let backend = live_backend();
        if !backend.is_available() {
            eprintln!("skipping: no container runtime on PATH");
            return;
        }
        if !daemon_ready(&backend).await {
            eprintln!("skipping: container runtime binary present but daemon unreachable");
            return;
        }

        // Snapshot pre-existing tags so the sweep below cannot touch them.
        let baseline = existing_tags(&backend).await;

        // Provision the run we keep alive across the sweep.
        let live_tag = SandboxTag::new();
        let live = backend
            .provision(ProvisionRequest {
                run_id: RunId::new(),
                tag: live_tag,
                lifetime_cap_secs: None,
                network_policy: Default::default(),
            })
            .await
            .expect("provision live container");

        // Provision the orphan: a container whose intent the host will not recognize.
        let orphan_tag = SandboxTag::new();
        let orphan = backend
            .provision(ProvisionRequest {
                run_id: RunId::new(),
                tag: orphan_tag,
                lifetime_cap_secs: None,
                network_policy: Default::default(),
            })
            .await
            .expect("provision orphan container");

        // Wait for the container to actually be running before addressing it, so the
        // published-port lookup and reachability probe do not race startup.
        assert!(
            wait_running(&backend, &live.reference).await,
            "live container never reached the running state"
        );

        // address resolves to a loopback URL with a live listener behind it.
        let address = backend.address(&live).await.expect("resolve live address");
        assert!(address.base_url.starts_with("http://127.0.0.1:"));
        assert!(!address.transport_secret.expose().is_empty());
        assert!(
            tcp_reachable(&address.base_url).await,
            "published port {} was not reachable",
            address.base_url
        );

        // The sweep's live set is everything present except the orphan, so exactly
        // the orphan is reclaimable.
        let mut live_tags = existing_tags(&backend).await;
        live_tags.remove(&orphan_tag);
        let _ = baseline; // documented intent: pre-existing tags are preserved.
        let reclaimed = backend
            .reclaim_orphans(&live_tags)
            .await
            .expect("sweep orphans");
        assert!(
            reclaimed.iter().any(|handle| handle.tag == orphan_tag),
            "the orphan was not reclaimed"
        );

        // The orphan is gone; the live container is untouched and still addressable.
        assert!(matches!(
            backend.address(&orphan).await,
            Err(BackendError::UnknownHandle)
        ));
        assert!(backend.address(&live).await.is_ok());

        // destroy is idempotent: a second removal of the same container succeeds.
        backend.destroy(&live).await.expect("destroy live");
        backend
            .destroy(&live)
            .await
            .expect("destroy is idempotent on a missing container");

        // Best-effort cleanup in case an assertion above left the orphan behind.
        let _ = backend.destroy(&orphan).await;
    }
}
