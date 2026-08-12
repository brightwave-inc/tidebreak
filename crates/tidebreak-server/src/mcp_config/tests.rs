use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tidebreak_core::connected_app::{ConnectedApp, ConnectedAppKind};
use tidebreak_core::id::ConnectedAppId;
use tidebreak_core::{AgentError, Result, SecretProvider, Store, ToolRegistry};

use super::*;

use super::validation::{connection_diagnostic, validate_servers};

use tidebreak_core::DbStore;

fn parse(json: &str) -> Result<ConfiguredMcpServers> {
    let config: McpServersConfig = serde_json::from_str(json)?;
    validate_servers(&config.servers)?;
    Ok(ConfiguredMcpServers(config.servers))
}

/// Fresh boot-style ids for a test's definitions.
fn ids_for(definitions: &[McpServerDefinition]) -> BTreeMap<String, ConnectedAppId> {
    definitions
        .iter()
        .map(|definition| (definition.name.clone(), ConnectedAppId::new()))
        .collect()
}

/// The persisted `mcp_server` definitions, read back through the
/// connected-app record the way `initialize` does.
async fn saved_definitions(store: &Arc<dyn Store>) -> Vec<McpServerDefinition> {
    store
        .list_connected_apps()
        .await
        .unwrap()
        .into_iter()
        .filter(|record| record.kind == ConnectedAppKind::McpServer)
        .map(|record| serde_json::from_value(record.definition).unwrap())
        .collect()
}

/// The persisted `mcp_server` records themselves, so a test can look at
/// the stored JSON rather than the type it parses into.
async fn saved_records(store: &Arc<dyn Store>) -> Vec<ConnectedApp> {
    store
        .list_connected_apps()
        .await
        .unwrap()
        .into_iter()
        .filter(|record| record.kind == ConnectedAppKind::McpServer)
        .collect()
}

/// Persist definitions as connected-app records, the way a settings save
/// would, without connecting anything.
async fn seed_records(store: &Arc<dyn Store>, definitions: &[McpServerDefinition]) {
    let now = chrono::Utc::now();
    let records: Vec<ConnectedApp> = definitions
        .iter()
        .map(|definition| ConnectedApp {
            id: ConnectedAppId::new(),
            name: definition.name.clone(),
            kind: ConnectedAppKind::McpServer,
            definition: serde_json::to_value(definition).unwrap(),
            created_at: now,
            updated_at: now,
        })
        .collect();
    store
        .replace_connected_apps(ConnectedAppKind::McpServer, &records)
        .await
        .unwrap();
}

/// The signed-out stand-in: every resolution demands a session.
struct NoGateway;

/// An in-memory secret store, so a test can assert what the runtime put
/// there — and what it did not.
#[derive(Default)]
struct TestSecrets(std::sync::Mutex<BTreeMap<String, String>>);

#[async_trait::async_trait]
impl SecretProvider for TestSecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    async fn delete_secret(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}
#[async_trait::async_trait]
impl GatewayEndpoints for NoGateway {
    async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
        Err(AgentError::Authentication(
            "gateway sign-in required: no gateway session is stored".to_string(),
        ))
    }
}

fn disabled_definition(name: &str, command: &str) -> McpServerDefinition {
    McpServerDefinition {
        name: name.to_string(),
        command: Some(command.to_string()),
        args: Vec::new(),
        env: BTreeSet::new(),
        env_values: BTreeMap::new(),
        env_from: Vec::new(),
        cwd: None,
        url: None,
        bearer_token_env: None,
        gateway_endpoint: None,
        request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        enabled: false,
        plugin: None,
        launch: None,
    }
}

fn http_definition(name: &str, url: &str) -> McpServerDefinition {
    McpServerDefinition {
        name: name.to_string(),
        command: None,
        args: Vec::new(),
        env: BTreeSet::new(),
        env_values: BTreeMap::new(),
        env_from: Vec::new(),
        cwd: None,
        url: Some(url.to_string()),
        bearer_token_env: None,
        gateway_endpoint: None,
        request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        enabled: true,
        plugin: None,
        launch: None,
    }
}

fn gateway_definition(name: &str, slug: &str) -> McpServerDefinition {
    McpServerDefinition {
        name: name.to_string(),
        command: None,
        args: Vec::new(),
        env: BTreeSet::new(),
        env_values: BTreeMap::new(),
        env_from: Vec::new(),
        cwd: None,
        url: None,
        bearer_token_env: None,
        gateway_endpoint: Some(slug.to_string()),
        request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
        enabled: true,
        plugin: None,
        launch: None,
    }
}

async fn test_runtime() -> (Arc<McpRuntime>, Arc<dyn Store>, tempfile::TempDir) {
    test_runtime_with_gateway(Arc::new(NoGateway)).await
}

async fn test_runtime_with_gateway(
    gateway: Arc<dyn GatewayEndpoints>,
) -> (Arc<McpRuntime>, Arc<dyn Store>, tempfile::TempDir) {
    test_runtime_with(gateway, Arc::new(crate::managed_policy::NoOsPolicy)).await
}

async fn test_runtime_with(
    gateway: Arc<dyn GatewayEndpoints>,
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
) -> (Arc<McpRuntime>, Arc<dyn Store>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("mcp.db").display()
        ))
        .await
        .unwrap(),
    );
    (
        Arc::new(McpRuntime::new(
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(TestSecrets::default()),
            gateway,
            Arc::new(crate::managed_policy::ProvisionedPolicyFile::in_data_dir(
                directory.path(),
            )),
            os_policy,
        )),
        store,
        directory,
    )
}

/// The explicit-unmount memory end to end: a settings save that removes
/// a gateway mount records the slug, auto-mount never resurrects it, and
/// a manual remount clears the record so it stays remounted. (Signed
/// out, the mount persists degraded — exactly what lets this run without
/// a live gateway.)
#[tokio::test]
async fn an_explicit_unmount_is_remembered_and_never_auto_remounted() {
    let (runtime, store, _directory) = test_runtime().await;
    assert!(runtime
        .auto_mount_gateway_endpoints(&["docs".to_string()])
        .await
        .unwrap());
    let saved = saved_definitions(&store).await;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "docs");
    assert_eq!(saved[0].gateway_endpoint.as_deref(), Some("docs"));
    assert!(saved[0].enabled);

    // The user unmounts: a complete settings save without the mount.
    runtime
        .replace(McpServersConfig {
            servers: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .get_setting(GATEWAY_ENDPOINT_UNMOUNTS_KEY)
            .await
            .unwrap()
            .unwrap(),
        serde_json::json!(["docs"])
    );

    // Auto-mount refuses to fight the recorded intent.
    assert!(!runtime
        .auto_mount_gateway_endpoints(&["docs".to_string()])
        .await
        .unwrap());
    assert!(runtime.info().await.servers.is_empty());

    // A manual remount clears the memory.
    runtime
        .replace(McpServersConfig {
            servers: vec![gateway_definition("docs", "docs")],
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .get_setting(GATEWAY_ENDPOINT_UNMOUNTS_KEY)
            .await
            .unwrap()
            .unwrap(),
        serde_json::json!([])
    );
    // Already mounted: nothing to add, nothing rewritten.
    assert!(!runtime
        .auto_mount_gateway_endpoints(&["docs".to_string()])
        .await
        .unwrap());
}

/// An entitled slug colliding with a configured server name derives the
/// same suffixed namespace the desktop's mount toggle would, instead of
/// failing validation on the duplicate.
#[tokio::test]
async fn auto_mount_suffixes_a_name_a_manual_server_already_took() {
    let (runtime, _store, _directory) = test_runtime().await;
    runtime
        .replace(McpServersConfig {
            servers: vec![disabled_definition("docs", "/usr/local/bin/docs-mcp")],
        })
        .await
        .unwrap();

    assert!(runtime
        .auto_mount_gateway_endpoints(&["docs".to_string()])
        .await
        .unwrap());
    let info = runtime.info().await;
    let names: Vec<&str> = info
        .servers
        .iter()
        .map(|server| server.definition.name.as_str())
        .collect();
    assert_eq!(names, ["docs", "docs_2"]);
    assert_eq!(
        info.servers[1].definition.gateway_endpoint.as_deref(),
        Some("docs")
    );
}

#[test]
fn parses_a_bounded_stdio_server_configuration() {
    let config = parse(
        r#"{
                "servers": [{
                    "name": "private_docs",
                    "command": "/usr/local/bin/docs-mcp",
                    "args": ["--stdio"],
                    "env": ["LOG_LEVEL"],
                    "env_values": {"LOG_LEVEL": "info"},
                    "env_from": ["DOCS_TOKEN"],
                    "cwd": "/srv/docs",
                    "request_timeout_ms": 2500
                }]
            }"#,
    )
    .unwrap();
    let server = &config.0[0];
    assert_eq!(server.name, "private_docs");
    assert_eq!(server.command.as_deref(), Some("/usr/local/bin/docs-mcp"));
    assert_eq!(server.args, ["--stdio"]);
    assert!(server.env.contains("LOG_LEVEL"));
    assert_eq!(server.env_values.get("LOG_LEVEL").unwrap(), "info");
    assert_eq!(server.env_from, ["DOCS_TOKEN"]);
    assert_eq!(server.cwd.as_deref(), Some(Path::new("/srv/docs")));
    assert_eq!(server.request_timeout_ms, 2500);
    assert!(server.enabled);
}

#[test]
fn defaults_to_an_isolated_environment_and_sixty_second_timeout() {
    let config = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
    let server = &config.0[0];
    assert!(server.args.is_empty());
    assert!(server.env.is_empty());
    assert!(server.env_from.is_empty());
    assert_eq!(server.request_timeout_ms, 60_000);
    let command = server.build_command(&BTreeMap::new()).unwrap();
    assert!(command.as_std().get_envs().next().is_none());
}

#[test]
fn rejects_environment_inheritance_and_unknown_fields() {
    let error = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs","inherit_env":true}]}"#)
        .err()
        .unwrap();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn rejects_duplicate_names_unsafe_strings_and_timeouts() {
    let duplicate = parse(
        r#"{"servers":[
                {"name":"docs","command":"/bin/one"},
                {"name":"docs","command":"/bin/two"}
            ]}"#,
    )
    .err()
    .unwrap();
    assert!(duplicate.to_string().contains("duplicated"));

    let nul = parse("{\"servers\":[{\"name\":\"docs\",\"command\":\"bad\\u0000command\"}]}")
        .err()
        .unwrap();
    assert!(nul.to_string().contains("must not contain NUL"));

    let timeout =
        parse(r#"{"servers":[{"name":"docs","command":"/bin/docs","request_timeout_ms":0}]}"#)
            .err()
            .unwrap();
    assert!(timeout.to_string().contains("request_timeout_ms"));
}

#[test]
fn rejects_ambiguous_or_invalid_environment_sources() {
    let duplicate = parse(
        r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env":["DOCS_TOKEN"],
                "env_from":["DOCS_TOKEN"]
            }]}"#,
    )
    .err()
    .unwrap();
    assert!(duplicate.to_string().contains("configured more than once"));

    let orphan = parse(
        r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env_values":{"DOCS_TOKEN":"literal"}
            }]}"#,
    )
    .err()
    .unwrap();
    assert!(orphan.to_string().contains("names no configured variable"));

    let invalid = parse(
        r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env_from":["BAD=NAME"]
            }]}"#,
    )
    .err()
    .unwrap();
    assert!(invalid
        .to_string()
        .contains("invalid environment variable name"));
}

#[test]
fn forwards_only_explicitly_selected_parent_environment_values() {
    let config = parse(
        r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env_from":["PATH"]
            }]}"#,
    )
    .unwrap();
    let command = config.0[0].build_command(&BTreeMap::new()).unwrap();
    let forwarded_path = command
        .as_std()
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value)
        .expect("PATH is selected for forwarding");
    assert_eq!(Some(forwarded_path), std::env::var_os("PATH").as_deref());
    assert!(command.as_std().get_envs().all(|(name, _)| name == "PATH"));
}

#[tokio::test]
async fn missing_selected_parent_environment_fails_before_spawn_without_a_value() {
    const MISSING: &str = "TIDEBREAK_TEST_MCP_ENV_FROM_MUST_NOT_EXIST_46F54489";
    assert!(std::env::var_os(MISSING).is_none());
    let config = parse(&format!(
        r#"{{"servers":[{{
                "name":"docs",
                "command":"/definitely/not/a/real/command",
                "env_from":["{MISSING}"]
            }}]}}"#
    ))
    .unwrap();
    let gateway: Arc<dyn GatewayEndpoints> = Arc::new(NoGateway);
    let error = config.0[0]
        .connect(&gateway, &BTreeMap::new())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains(MISSING));
    assert!(error.to_string().contains("is not set"));
    assert!(!error.to_string().contains("secret-value"));
}

#[test]
fn projected_diagnostics_are_fixed_or_name_only() {
    const MISSING: &str = "TIDEBREAK_TEST_MCP_DIAGNOSTIC_MISSING_13B2";
    assert!(std::env::var_os(MISSING).is_none());
    let config = parse(&format!(
        r#"{{"servers":[{{"name":"docs","command":"/bin/docs","env_from":["{MISSING}"]}}]}}"#
    ))
    .unwrap();
    let failure = AgentError::config("connect failed");
    let missing = connection_diagnostic(&config.0[0], &failure);
    assert!(missing.contains(MISSING));
    assert!(!missing.contains('\n'));

    let generic = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
    assert_eq!(
        connection_diagnostic(&generic.0[0], &failure),
        "Could not initialize this server. Check its executable, arguments, and working directory."
    );
}

#[tokio::test]
async fn concurrent_replacements_keep_durable_and_live_configuration_in_commit_order() {
    let (runtime, store, _directory) = test_runtime().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let first = {
        let runtime = runtime.clone();
        let entered = entered.clone();
        let release = release.clone();
        tokio::spawn(async move {
            runtime
                .replace_with_commit_pause(
                    McpServersConfig {
                        servers: vec![disabled_definition("first", "/bin/first")],
                    },
                    entered,
                    release,
                )
                .await
        })
    };
    entered.notified().await;
    let mut second = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            runtime
                .replace(McpServersConfig {
                    servers: vec![disabled_definition("second", "/bin/second")],
                })
                .await
        })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut second)
            .await
            .is_err(),
        "second replacement bypassed the fence"
    );
    release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    let saved = saved_definitions(&store).await;
    let live = runtime.info().await;
    assert_eq!(saved[0].name, "second");
    assert_eq!(live.servers[0].definition, saved[0]);
}

#[tokio::test]
async fn stale_supervisor_result_cannot_overwrite_a_replacement() {
    let (runtime, _store, _directory) = test_runtime().await;
    runtime
        .replace(McpServersConfig {
            servers: vec![disabled_definition("docs", "/bin/old")],
        })
        .await
        .unwrap();
    let old_epoch = runtime
        .state
        .lock()
        .await
        .servers
        .get("docs")
        .unwrap()
        .epoch;
    runtime
        .replace(McpServersConfig {
            servers: vec![disabled_definition("docs", "/bin/new")],
        })
        .await
        .unwrap();

    runtime
        .mark_degraded("docs", old_epoch, INITIAL_RECONNECT_BACKOFF)
        .await;
    let info = runtime.info().await;
    assert_eq!(
        info.servers[0].definition.command.as_deref(),
        Some("/bin/new")
    );
    assert_eq!(info.servers[0].health, McpHealth::Disabled);
    assert!(info.servers[0].diagnostic.is_none());
}

#[tokio::test]
async fn concurrent_reconnects_share_one_attempt_for_a_published_server() {
    let (runtime, _store, _directory) = test_runtime().await;
    let mut definition = disabled_definition("docs", "/usr/bin/true");
    definition.enabled = true;
    let definitions = vec![definition];
    let ids = ids_for(&definitions);
    runtime.replace_permissive(definitions, ids).await;
    let reconnect_lock = runtime
        .state
        .lock()
        .await
        .servers
        .get("docs")
        .unwrap()
        .reconnect_lock
        .clone();
    let held = reconnect_lock.lock().await;
    let first = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.reconnect("docs").await })
    };
    let second = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.reconnect("docs").await })
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        while Arc::strong_count(&reconnect_lock) < 4 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both reconnects should wait on the published server lock");
    drop(held);

    let first = first.await.unwrap();
    let second = second.await.unwrap();
    assert_ne!(
        first.is_err(),
        second.is_err(),
        "exactly one caller should perform the failed connection attempt"
    );
    assert_eq!(runtime.info().await.servers[0].health, McpHealth::Degraded);
}

#[test]
fn parses_a_streamable_http_server_configuration() {
    let config = parse(
        r#"{
                "servers": [{
                    "name": "gateway",
                    "url": "http://127.0.0.1:28081/mcp/tools",
                    "bearer_token_env": "GATEWAY_TOKEN",
                    "request_timeout_ms": 2500
                }]
            }"#,
    )
    .unwrap();
    let server = &config.0[0];
    assert_eq!(
        server.url.as_deref(),
        Some("http://127.0.0.1:28081/mcp/tools")
    );
    assert_eq!(server.bearer_token_env.as_deref(), Some("GATEWAY_TOKEN"));
    assert!(server.command.is_none());
}

#[test]
fn each_server_is_exactly_one_transport() {
    for extra in [
        r#""url":"http://127.0.0.1/mcp""#,
        r#""gateway_endpoint":"tools""#,
    ] {
        let both = parse(&format!(
            r#"{{"servers":[{{"name":"docs","command":"/bin/docs",{extra}}}]}}"#
        ))
        .err()
        .unwrap();
        assert!(both.to_string().contains("exactly one"), "{extra}: {both}");
    }

    let neither = parse(r#"{"servers":[{"name":"docs"}]}"#).err().unwrap();
    assert!(neither
        .to_string()
        .contains("command, a url, or a gateway endpoint"));
}

#[test]
fn transport_specific_fields_stay_with_their_transport() {
    let bearer_on_stdio =
        parse(r#"{"servers":[{"name":"docs","command":"/bin/docs","bearer_token_env":"TOKEN"}]}"#)
            .err()
            .unwrap();
    assert!(bearer_on_stdio.to_string().contains("only to url servers"));

    for (field, fragment) in [
        (r#""args":["--stdio"]"#, "args apply only"),
        (r#""env":["A"]"#, "environment applies only"),
        (r#""env_from":["TOKEN"]"#, "environment applies only"),
        (r#""cwd":"/srv""#, "cwd applies only"),
    ] {
        for transport in [
            r#""url":"http://127.0.0.1/mcp""#,
            r#""gateway_endpoint":"tools""#,
        ] {
            let error = parse(&format!(
                r#"{{"servers":[{{"name":"docs",{transport},{field}}}]}}"#
            ))
            .err()
            .unwrap();
            assert!(
                error.to_string().contains(fragment),
                "{transport} {field}: {error}"
            );
        }
    }

    // A gateway endpoint's bearer comes from the session, never from a
    // selected environment variable.
    let bearer_on_gateway = parse(
        r#"{"servers":[{"name":"docs","gateway_endpoint":"tools","bearer_token_env":"TOKEN"}]}"#,
    )
    .err()
    .unwrap();
    assert!(bearer_on_gateway.to_string().contains("signed-in session"));
}

#[test]
fn gateway_endpoint_slugs_follow_the_gateway_contract() {
    for slug in ["tools", "example-security_2"] {
        parse(&format!(
            r#"{{"servers":[{{"name":"docs","gateway_endpoint":"{slug}"}}]}}"#
        ))
        .unwrap();
    }
    let overlong = "a".repeat(128);
    for slug in ["", "has space", "path/../escape", "mcp:tools", &overlong] {
        let error = parse(&format!(
            r#"{{"servers":[{{"name":"docs","gateway_endpoint":"{slug}"}}]}}"#
        ))
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("gateway endpoint must be"),
            "{slug}: {error}"
        );
    }
}

#[tokio::test]
async fn signed_out_gateway_mounts_degrade_to_a_sign_in_diagnostic() {
    let (runtime, _store, _directory) = test_runtime().await;
    runtime
        .replace_permissive(
            vec![gateway_definition("tools", "tools")],
            ids_for(&[gateway_definition("tools", "tools")]),
        )
        .await;
    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some("Sign in to the model gateway to reconnect this server.")
    );

    // MCP mounts never reach the create_app roster — tool bindings are
    // retired (#1332) — so a configured-but-degraded gateway server reads
    // as "nothing bindable", not as a bindable app with a caveat.
    let state = runtime.state.lock().await;
    assert!(!state.definitions.is_empty());
    let roster = connected_app_roster(&[], &[], &[]);
    assert!(
        roster.contains("No rest_api connected apps are configured"),
        "{roster}"
    );
}

/// The roster's gateway section is where a model learns the ids a gateway
/// binding names, so it must spell out the binding shape, elide a long
/// catalog instead of pasting it into every tool description, and be absent
/// entirely without a gateway session — which is the same thing the door's
/// refusal says.
#[test]
fn the_create_app_roster_lists_gateway_apps_and_elides_long_catalogs() {
    let operation_ids: Vec<String> = (0..ROSTER_OPERATION_IDS + 5)
        .map(|index| format!("op{index}"))
        .collect();
    let roster = connected_app_roster(
        &[],
        &[],
        &[GatewayRosterApp {
            id: "app-incident".into(),
            name: "Incident API".into(),
            operation_ids: operation_ids.clone(),
        }],
    );
    assert!(
        roster.contains("app-incident — Incident API (gateway app)"),
        "{roster}"
    );
    assert!(roster.contains("\"gateway_app\": id"), "{roster}");
    assert!(roster.contains("op0"), "{roster}");
    assert!(roster.contains('…'), "{roster}");
    assert!(
        !roster.contains(operation_ids.last().unwrap().as_str()),
        "{roster}"
    );

    // No session, no section: the roster never implies a binding vocabulary
    // this profile could not resolve.
    let signed_out = connected_app_roster(&[], &[], &[]);
    assert!(!signed_out.contains("gateway app"), "{signed_out}");
}

/// The two non-sign-in gateway failures are different problems with
/// different fixes, and the diagnostic must say which one happened: a
/// refused resolution/token exchange (`AgentError::Config`, before any
/// wire I/O) is an entitlement problem, while a reached-or-unreachable
/// endpoint (any other class) is an endpoint problem.
#[tokio::test]
async fn gateway_diagnostics_separate_refused_access_from_endpoint_failures() {
    // The gateway refuses to mint `mcp:<slug>` access: no wire I/O ever
    // happened, so "check the endpoint" would send the user the wrong way.
    struct RefusedGateway;

    #[async_trait::async_trait]
    impl GatewayEndpoints for RefusedGateway {
        async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
            Err(AgentError::config(
                "model-gateway token request: the requested resource is not entitled",
            ))
        }
    }

    let (runtime, _store, _directory) = test_runtime_with_gateway(Arc::new(RefusedGateway)).await;
    let definitions = vec![gateway_definition("tools", "tools")];
    let ids = ids_for(&definitions);
    runtime.replace_permissive(definitions, ids).await;
    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some("Could not get access to this gateway endpoint. Check your entitlements for it.")
    );

    // Resolution succeeded but the endpoint itself answers 403: the wire
    // was reached, so entitlement language would be a lie.
    struct ResolvedGateway(std::net::SocketAddr);

    #[async_trait::async_trait]
    impl GatewayEndpoints for ResolvedGateway {
        async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
            Ok(GatewayEndpointAccess {
                url: format!("http://{}/mcp", self.0),
                bearer_token: "session-token".to_string(),
            })
        }
    }

    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(|| async { axum::http::StatusCode::FORBIDDEN }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (runtime, _store, _directory) =
        test_runtime_with_gateway(Arc::new(ResolvedGateway(address))).await;
    let definitions = vec![gateway_definition("tools", "tools")];
    let ids = ids_for(&definitions);
    runtime.replace_permissive(definitions, ids).await;
    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some(
            "Could not connect to this gateway endpoint. Check that it is reachable \
                 and allows this kind of access."
        )
    );
}

#[tokio::test]
async fn a_failing_gateway_mount_never_blocks_a_settings_replacement() {
    let (runtime, store, _directory) = test_runtime().await;
    // Saving a configuration that contains an unconnectable gateway
    // mount (signed out) plus an ordinary edit must persist both: the
    // mount degrades in place instead of rejecting the candidate.
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![
                gateway_definition("tools", "tools"),
                disabled_definition("docs", "/bin/docs"),
            ],
        })
        .await
        .unwrap();
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some("Sign in to the model gateway to reconnect this server.")
    );
    assert_eq!(info.servers[1].health, McpHealth::Disabled);
    assert_eq!(saved_definitions(&store).await.len(), 2);

    // A non-gateway failure keeps save-and-verify semantics: reject and
    // change nothing.
    let error = runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("dead", "http://127.0.0.1:1/mcp")],
        })
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("failed to start"));
    assert_eq!(
        saved_definitions(&store).await.len(),
        2,
        "rejected candidate must not persist"
    );
}

#[test]
fn rejects_invalid_http_urls() {
    for url in ["ftp://host/mcp", "http://user:secret@host/mcp", "not a url"] {
        let error = parse(&format!(
            r#"{{"servers":[{{"name":"docs","url":"{url}"}}]}}"#
        ))
        .err()
        .unwrap();
        assert!(!error.to_string().contains("secret"), "{url}: {error}");
    }
}

#[tokio::test]
async fn missing_selected_bearer_token_fails_by_name_without_a_value() {
    const MISSING: &str = "TIDEBREAK_TEST_MCP_BEARER_MUST_NOT_EXIST_8A31";
    assert!(std::env::var_os(MISSING).is_none());
    let mut definition = http_definition("gateway", "http://127.0.0.1:1/mcp");
    definition.bearer_token_env = Some(MISSING.to_string());
    let gateway: Arc<dyn GatewayEndpoints> = Arc::new(NoGateway);
    let error = definition
        .connect(&gateway, &BTreeMap::new())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains(MISSING));
    assert!(error.to_string().contains("is not set"));

    let diagnostic = connection_diagnostic(&definition, &error);
    assert!(diagnostic.contains(MISSING));
    assert!(!diagnostic.contains('\n'));
}

#[test]
fn http_diagnostics_are_fixed_strings_without_the_url() {
    let definition = http_definition("gateway", "http://127.0.0.1:9/mcp");
    let diagnostic = connection_diagnostic(&definition, &AgentError::config("connect failed"));
    assert_eq!(
        diagnostic,
        "Could not connect to this server. Check its URL and credentials."
    );
}

async fn serve_fake_http_mcp() -> std::net::SocketAddr {
    use axum::http::HeaderMap;
    use axum::routing::post;

    async fn handler(
        headers: HeaderMap,
        body: String,
    ) -> ([(&'static str, &'static str); 1], String) {
        // The config layer resolved the selected variable into the header.
        let expected = format!("Bearer {}", std::env::var("PATH").unwrap());
        assert_eq!(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(expected.as_str())
        );
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = request.get("id").cloned().unwrap_or_default();
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => serde_json::json!({
                "protocolVersion": tidebreak_mcp::PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "config-fixture", "version": "1"}
            }),
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": "lookup",
                    "description": "Look something up",
                    "inputSchema": {"type": "object"},
                    "_meta": {"ui": {"resourceUri": "ui://fixture/app.html"}}
                }]
            }),
            "resources/read" => serde_json::json!({
                "contents": [{
                    "uri": "ui://fixture/app.html",
                    "mimeType": "text/html;profile=mcp-app",
                    "text": "<html>fixture view</html>"
                }]
            }),
            _ => serde_json::json!({}),
        };
        (
            [("content-type", "application/json")],
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        )
    }

    let app = axum::Router::new().route("/mcp", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

#[tokio::test]
async fn replaces_with_a_streamable_http_server_and_mounts_its_tools() {
    let address = serve_fake_http_mcp().await;
    let (runtime, _store, _directory) = test_runtime().await;
    let mut definition = http_definition("gateway", &format!("http://{address}/mcp"));
    // PATH always exists, so the selected-name path is exercised for real
    // without mutating the test process environment.
    definition.bearer_token_env = Some("PATH".to_string());
    runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();

    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Healthy);
    assert_eq!(info.servers[0].tool_count, 1);
    assert!(runtime.snapshot().get("mcp__gateway__lookup").is_some());

    // Even a healthy, connected server contributes nothing bindable to
    // the create_app roster: tool bindings are retired (#1332).
    {
        let roster = connected_app_roster(&[], &[], &[]);
        assert!(!roster.contains("mcp__gateway__"), "{roster}");
        assert!(
            roster.contains("No rest_api connected apps are configured"),
            "{roster}"
        );
    }

    // The declared view was prefetched at connect and is served from
    // memory, keyed by the configured namespace and declared URI.
    let view = runtime
        .ui_view_document("gateway", "ui://fixture/app.html")
        .await
        .expect("declared view is prefetched");
    assert_eq!(view.html, "<html>fixture view</html>");
    assert_eq!(view.mime_type.as_deref(), Some("text/html;profile=mcp-app"));

    assert!(runtime
        .ui_view_document("gateway", "ui://fixture/other.html")
        .await
        .is_none());
    assert!(runtime
        .ui_view_document("unknown", "ui://fixture/app.html")
        .await
        .is_none());
}

/// The mid-process flip: a manual server that was healthy when the policy
/// was open must not keep serving tools for the rest of the process. The
/// decision is re-read live, and so is the effect — its client is dropped,
/// its tools leave the registry, and it reports the managed diagnostic.
#[tokio::test]
async fn a_running_manual_server_is_torn_down_when_policy_flips_managed() {
    let address = serve_fake_http_mcp().await;
    let (runtime, _store, directory) = test_runtime().await;
    let mut definition = http_definition("gateway", &format!("http://{address}/mcp"));
    definition.bearer_token_env = Some("PATH".to_string());
    runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();
    assert_eq!(runtime.info().await.servers[0].health, McpHealth::Healthy);
    assert!(runtime.snapshot().get("mcp__gateway__lookup").is_some());

    // The profile becomes managed with the child already connected — an
    // MDM push, or deep-link pairing mid-session.
    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(directory.path()),
        "https://corp.gateway",
    )
    .unwrap();
    assert!(runtime.enforce_manual_lockdown().await);

    assert!(
        runtime.snapshot().get("mcp__gateway__lookup").is_none(),
        "a locked server must stop serving tools to new turns"
    );
    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Disabled);
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some(MANAGED_DISABLED_DIAGNOSTIC)
    );
    assert_eq!(info.servers[0].tool_count, 0);
    // Idempotent: a second sweep has nothing left to take down.
    assert!(!runtime.enforce_manual_lockdown().await);
}

/// Managed lockdown at the runtime boundary: persisted manual servers stay
/// on file but never connect — disabled with a legible reason rather than
/// silently deleted — while gateway mounts still resolve, and the
/// host-environment boot file, the one channel the lockdown exists to
/// close, is ignored outright.
#[tokio::test]
async fn managed_policy_forces_manual_servers_down_and_ignores_the_boot_file() {
    let (runtime, store, directory) = test_runtime().await;
    let mut manual = disabled_definition("private_docs", "/bin/docs");
    manual.enabled = true;
    seed_records(&store, &[manual, gateway_definition("tools", "tools")]).await;
    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(directory.path()),
        "https://corp.gateway",
    )
    .unwrap();

    runtime
        .initialize(ConfiguredMcpServers::default())
        .await
        .unwrap();
    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Disabled);
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some(MANAGED_DISABLED_DIAGNOSTIC)
    );
    assert!(
        info.servers[0].definition.enabled,
        "the stored definition is untouched, so lifting the policy restores it"
    );
    assert!(runtime
        .snapshot()
        .get("mcp__private_docs__lookup")
        .is_none());
    // The gateway mount is the sanctioned path and still attempts its
    // session-backed connection (signed out here, so it degrades).
    assert_eq!(info.servers[1].health, McpHealth::Degraded);
    assert_eq!(
        info.servers[1].diagnostic.as_deref(),
        Some("Sign in to the model gateway to reconnect this server.")
    );
    assert!(runtime.reconnect("private_docs").await.is_err());

    // A fresh profile whose only configuration is the boot file: managed,
    // so the file is inert and nothing is configured or persisted.
    let (runtime, store, directory) = test_runtime().await;
    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(directory.path()),
        "https://corp.gateway",
    )
    .unwrap();
    let boot = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
    runtime.initialize(boot).await.unwrap();
    assert!(runtime.info().await.servers.is_empty());
    assert!(store.list_connected_apps().await.unwrap().is_empty());
}

/// The org's `AllowLocalMcpServers` opt-in narrows the managed lockdown
/// to remote manual servers. A local stdio definition is the user's again
/// — the runtime attempts its child (the fixture command doesn't exist,
/// so it degrades) instead of forcing it down — and its edits are
/// admitted, while a `url` server stays forced down with the managed
/// diagnostic and adding one is still refused.
#[tokio::test]
async fn allow_local_mcp_scopes_the_lockdown_to_remote_transports() {
    struct ManagedAllowingLocal;

    impl crate::managed_policy::OsPolicySource for ManagedAllowingLocal {
        fn gateway_url(&self) -> Result<Option<String>> {
            Ok(Some("https://corp.gateway".to_string()))
        }
        fn allow_local_mcp_servers(&self) -> Result<Option<bool>> {
            Ok(Some(true))
        }
    }

    let (runtime, store, _directory) =
        test_runtime_with(Arc::new(NoGateway), Arc::new(ManagedAllowingLocal)).await;
    let mut local = disabled_definition("local_docs", "/bin/docs");
    local.enabled = true;
    let remote = http_definition("remote", "http://127.0.0.1:9/mcp");
    seed_records(&store, &[local.clone(), remote.clone()]).await;

    runtime
        .initialize(ConfiguredMcpServers::default())
        .await
        .unwrap();
    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
    assert_ne!(
        info.servers[0].diagnostic.as_deref(),
        Some(MANAGED_DISABLED_DIAGNOSTIC)
    );
    assert_eq!(info.servers[1].health, McpHealth::Disabled);
    assert_eq!(
        info.servers[1].diagnostic.as_deref(),
        Some(MANAGED_DISABLED_DIAGNOSTIC)
    );

    // The admission check draws the same line: a body adding another
    // remote server is refused by name, while one that only edits the
    // stdio definition (disabling it) lands.
    let extra_remote = http_definition("extra_remote", "http://127.0.0.1:9/mcp");
    let outcome = runtime
        .replace_under_policy(
            McpServersConfig {
                servers: vec![local.clone(), remote.clone(), extra_remote],
            },
            ManualLockdown::RemoteManual,
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        McpReplaceOutcome::RefusedManual(refused) if refused == ["extra_remote"]
    ));

    local.enabled = false;
    let outcome = runtime
        .replace_under_policy(
            McpServersConfig {
                servers: vec![local, remote],
            },
            ManualLockdown::RemoteManual,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, McpReplaceOutcome::Replaced(_)));
}

/// The v:2 canonical-form invariants that keep consent honest: derived
/// from definition fields only, never a value oracle, covering the
/// namespace (which decides what mounted names a grant reaches), and
/// indifferent to toggles that don't change what the user consented to.
#[test]
fn fingerprints_derive_from_fields_cover_the_namespace_and_leak_no_values() {
    let mut definition = disabled_definition("docs", "/bin/docs");
    definition.env.insert("TOKEN".into());
    definition
        .env_values
        .insert("TOKEN".into(), "secret-a".into());
    let baseline = definition_fingerprint(&definition);

    let mut toggled = definition.clone();
    toggled.enabled = true;
    toggled.request_timeout_ms += 1;
    assert_eq!(
        definition_fingerprint(&toggled),
        baseline,
        "enabling or re-timing is not a change of what the user consented to"
    );

    let mut rotated = definition.clone();
    rotated.env_values.insert("TOKEN".into(), "secret-b".into());
    assert_eq!(
        definition_fingerprint(&rotated),
        baseline,
        "environment values never enter the canonical form"
    );

    let stored_only = disabled_definition("docs", "/bin/docs");
    let mut stored_only = stored_only;
    stored_only.env.insert("TOKEN".into());
    assert_eq!(
        definition_fingerprint(&stored_only),
        baseline,
        "moving the values into the secret store leaves every grant pinned \
             to this definition still matching"
    );

    let mut renamed = definition.clone();
    renamed.name = "docs2".into();
    assert_ne!(
        definition_fingerprint(&renamed),
        baseline,
        "app-keyed grants no longer key by name, so the namespace is \
             part of what a grant pins"
    );

    let mut swapped = definition.clone();
    swapped.command = Some("/bin/other".into());
    assert_ne!(definition_fingerprint(&swapped), baseline);
}

/// The whole point of the change: a value the user typed into Settings
/// lands in the secret store, and nothing that leaves this process — the
/// persisted record or the projection the renderer reads — carries it.
#[tokio::test]
async fn environment_values_reach_the_secret_store_and_nothing_else() {
    const VALUE: &str = "sk-not-a-real-key-2f1c";
    let (runtime, store, _directory) = test_runtime().await;
    let mut definition = disabled_definition("docs", "/bin/docs");
    definition.env.insert("DOCS_TOKEN".to_string());
    definition
        .env_values
        .insert("DOCS_TOKEN".to_string(), VALUE.to_string());
    runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();

    // The projection the renderer reads, serialized exactly as the route
    // sends it.
    let projected = serde_json::to_string(&runtime.info().await).unwrap();
    assert!(!projected.contains(VALUE), "{projected}");
    assert!(projected.contains("DOCS_TOKEN"), "{projected}");

    // The durable record.
    let record = &saved_records(&store).await[0];
    let stored = serde_json::to_string(&record.definition).unwrap();
    assert!(!stored.contains(VALUE), "{stored}");
    assert_eq!(record.definition["env"], serde_json::json!(["DOCS_TOKEN"]));

    // And where it did go.
    let secret = runtime
        .secrets()
        .get_secret(&env_secret_key(record.id))
        .await
        .unwrap()
        .expect("the value is in the secret store");
    assert_eq!(secret, format!(r#"{{"DOCS_TOKEN":"{VALUE}"}}"#));
}

/// A save that leaves a value blank keeps the stored one; dropping the
/// name takes the value with it. Without this, editing any other field of
/// a server would silently wipe its credentials.
#[tokio::test]
async fn a_blank_value_keeps_the_stored_one_and_removing_a_name_drops_it() {
    let (runtime, store, _directory) = test_runtime().await;
    let mut definition = disabled_definition("docs", "/bin/docs");
    definition.env.insert("DOCS_TOKEN".to_string());
    definition
        .env_values
        .insert("DOCS_TOKEN".to_string(), "first".to_string());
    runtime
        .replace(McpServersConfig {
            servers: vec![definition.clone()],
        })
        .await
        .unwrap();
    let id = saved_records(&store).await[0].id;

    // The renderer round-trips the definition it was given, which carries
    // names only — no `env_values` at all.
    let mut retimed = definition.clone();
    retimed.env_values.clear();
    retimed.request_timeout_ms += 1;
    runtime
        .replace(McpServersConfig {
            servers: vec![retimed],
        })
        .await
        .unwrap();
    assert_eq!(
        runtime
            .stored_env(id)
            .await
            .get("DOCS_TOKEN")
            .map(String::as_str),
        Some("first")
    );

    let mut cleared = definition.clone();
    cleared.env.clear();
    cleared.env_values.clear();
    runtime
        .replace(McpServersConfig {
            servers: vec![cleared],
        })
        .await
        .unwrap();
    assert!(runtime.stored_env(id).await.is_empty());
}

/// Definitions persisted before the values moved out carry them in
/// cleartext. Boot migrates them into the secret store and rewrites the
/// record, and the definition fingerprint — what every app grant is
/// pinned to — comes out unchanged, so no grant is invalidated.
#[tokio::test]
async fn a_legacy_record_migrates_its_cleartext_values_without_moving_the_fingerprint() {
    const VALUE: &str = "legacy-secret-9a4d";
    let (runtime, store, _directory) = test_runtime().await;
    let expected = {
        let mut definition = disabled_definition("docs", "/bin/docs");
        definition.env.insert("DOCS_TOKEN".to_string());
        definition_fingerprint(&definition)
    };
    // The pre-migration shape, written straight to the store.
    let now = chrono::Utc::now();
    let id = ConnectedAppId::new();
    let legacy = serde_json::json!({
        "name": "docs",
        "command": "/bin/docs",
        "args": [],
        "env": {"DOCS_TOKEN": VALUE},
        "env_from": [],
        "cwd": null,
        "url": null,
        "bearer_token_env": null,
        "gateway_endpoint": null,
        "request_timeout_ms": DEFAULT_REQUEST_TIMEOUT_MS,
        "enabled": false,
    });
    store
        .replace_connected_apps(
            ConnectedAppKind::McpServer,
            &[ConnectedApp {
                id,
                name: "docs".to_string(),
                kind: ConnectedAppKind::McpServer,
                definition: legacy,
                created_at: now,
                updated_at: now,
            }],
        )
        .await
        .unwrap();

    runtime
        .initialize(ConfiguredMcpServers::default())
        .await
        .unwrap();

    let record = &saved_records(&store).await[0];
    assert_eq!(record.id, id, "the record keeps its identity");
    assert_eq!(record.definition["env"], serde_json::json!(["DOCS_TOKEN"]));
    assert!(!serde_json::to_string(&record.definition)
        .unwrap()
        .contains(VALUE));
    assert_eq!(
        runtime
            .stored_env(id)
            .await
            .get("DOCS_TOKEN")
            .map(String::as_str),
        Some(VALUE)
    );
    assert_eq!(
        runtime.app_fingerprints().await[&id].fingerprint,
        expected,
        "the canonical form only ever saw the names, so grants survive"
    );
}

#[test]
fn debug_projection_redacts_argument_and_literal_environment_values() {
    let mut definition = disabled_definition("docs", "/bin/docs");
    definition.args = vec!["argument-secret".to_string()];
    definition.env.insert("TOKEN".to_string());
    definition
        .env_values
        .insert("TOKEN".to_string(), "literal-secret".to_string());
    let debug = format!("{definition:?}");
    assert!(!debug.contains("argument-secret"));
    assert!(!debug.contains("literal-secret"));
    assert!(debug.contains("TOKEN"));
}

/// A plugin catalog with a fixed set of sources, for the runtime tests.
struct FixedPluginCatalog(Vec<(String, std::path::PathBuf, std::path::PathBuf)>);

#[async_trait::async_trait]
impl crate::plugin_mcp::PluginMcpCatalog for FixedPluginCatalog {
    async fn sources(&self) -> Vec<crate::plugin_mcp::PluginMcpSource> {
        self.0
            .iter()
            .map(|(plugin, root, data)| crate::plugin_mcp::PluginMcpSource {
                plugin: plugin.clone(),
                root: root.clone(),
                data: data.clone(),
                config: tidebreak_code_execution::PluginMcpConfig {
                    servers: vec![tidebreak_code_execution::McpServer {
                        name: "local".to_owned(),
                        transport: tidebreak_code_execution::McpTransport::Stdio(
                            tidebreak_code_execution::McpStdioServer {
                                command: "./serve".to_owned(),
                                args: Vec::new(),
                                env: BTreeMap::new(),
                                cwd: None,
                            },
                        ),
                    }],
                },
            })
            .collect()
    }
}

/// Contract: a plugin-sourced server is a manual server as far as managed
/// policy is concerned. Derived servers bypass `PUT /mcp/servers` entirely,
/// so if the lockdown were keyed off that route a managed profile could be
/// handed arbitrary local subprocesses and remote endpoints by installing a
/// plugin — the exact channel the lockdown exists to close.
#[tokio::test]
async fn managed_policy_locks_plugin_sourced_servers_like_manual_ones() {
    let (runtime, store, directory) = test_runtime().await;
    let root = directory.path().join("pkg");
    let data = directory.path().join("data");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    runtime.set_plugin_catalog(Arc::new(FixedPluginCatalog(vec![(
        "toolbox".to_owned(),
        root,
        data,
    )])));
    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(directory.path()),
        "https://corp.gateway",
    )
    .unwrap();

    assert!(runtime.reconcile_plugin_servers().await);
    let info = runtime.info().await;
    assert_eq!(info.servers.len(), 1);
    assert_eq!(
        info.servers[0].definition.plugin.as_deref(),
        Some("toolbox")
    );
    assert_eq!(info.servers[0].health, McpHealth::Disabled);
    assert_eq!(
        info.servers[0].diagnostic.as_deref(),
        Some(MANAGED_DISABLED_DIAGNOSTIC),
        "a plugin server carries the same diagnostic a user-typed one does"
    );
    assert_eq!(info.servers[0].tool_count, 0);
    // Nothing about a derived server reaches durable configuration.
    assert!(saved_records(&store).await.is_empty());
}
