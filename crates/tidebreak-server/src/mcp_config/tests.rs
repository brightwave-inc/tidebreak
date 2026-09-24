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
        Err(AgentError::SignInRequired(
            "no gateway session is stored".to_string(),
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
        oauth: false,
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
        oauth: false,
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
        oauth: false,
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

/// A saved mount Tidebreak could not load still mounts its endpoint, and a
/// skipped record still holds its name. Auto-mount mounts neither the
/// endpoint again nor a server under that name, and the endpoint it can
/// mount still lands.
#[tokio::test]
async fn auto_mount_leaves_skipped_records_their_endpoint_and_name() {
    let (runtime, store, _directory) = test_runtime().await;
    let now = chrono::Utc::now();
    let record = |name: &str, definition: serde_json::Value| ConnectedApp {
        id: ConnectedAppId::new(),
        name: name.to_string(),
        kind: ConnectedAppKind::McpServer,
        definition,
        created_at: now,
        updated_at: now,
    };
    // A mount of the docs endpoint, written by a newer build.
    let mount = record(
        "docs",
        serde_json::json!({
            "name": "docs",
            "gateway_endpoint": "docs",
            "transport": "streamable_http"
        }),
    );
    // Another newer record, under the name the tools endpoint would take.
    let named = record(
        "tools",
        serde_json::json!({
            "name": "tools",
            "command": "/bin/tools",
            "transport": "stdio"
        }),
    );
    store
        .replace_connected_apps(ConnectedAppKind::McpServer, &[mount, named])
        .await
        .unwrap();
    runtime
        .initialize(ConfiguredMcpServers::default())
        .await
        .unwrap()
        .connect()
        .await;

    assert!(runtime
        .auto_mount_gateway_endpoints(&["docs".to_string(), "tools".to_string()])
        .await
        .unwrap());
    let info = runtime.info().await;
    let mounted: Vec<(&str, Option<&str>)> = info
        .servers
        .iter()
        .map(|server| {
            (
                server.definition.name.as_str(),
                server.definition.gateway_endpoint.as_deref(),
            )
        })
        .collect();
    assert_eq!(mounted, [("tools_2", Some("tools"))]);
    let skipped: Vec<String> = runtime
        .skipped_servers()
        .await
        .into_iter()
        .map(|skipped| skipped.name)
        .collect();
    assert_eq!(skipped, ["docs", "tools"]);
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

#[tokio::test]
async fn defaults_to_an_isolated_environment_and_sixty_second_timeout() {
    let config = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
    let server = &config.0[0];
    assert!(server.args.is_empty());
    assert!(server.env.is_empty());
    assert!(server.env_from.is_empty());
    assert_eq!(server.request_timeout_ms, 60_000);
    let command = server.build_command(&BTreeMap::new()).await.unwrap();
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

#[tokio::test]
async fn forwards_only_explicitly_selected_parent_environment_values() {
    let config = parse(
        r#"{"servers":[{
                "name":"docs",
                "command":"/bin/docs",
                "env_from":["PATH"]
            }]}"#,
    )
    .unwrap();
    let command = config.0[0].build_command(&BTreeMap::new()).await.unwrap();
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
        .connect(&gateway, &BTreeMap::new(), None)
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
    assert!(missing.contains("Parent environment variable"));
    assert!(missing.contains("shell you start Tidebreak from"));
    assert!(!missing.contains('\n'));

    let generic = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
    assert_eq!(
        connection_diagnostic(&generic.0[0], &failure),
        "Could not initialize this server. Check its executable, arguments, and working directory."
    );

    let not_found = AgentError::config(
        "Command not found: \"npx\" is not on the host PATH. Searched: /opt/homebrew/bin.",
    );
    assert_eq!(
        connection_diagnostic(&generic.0[0], &not_found),
        "Command not found: \"npx\" is not on the host PATH. Searched: /opt/homebrew/bin."
    );
    let not_exec =
        AgentError::config("Not executable: /tmp/npx exists but is not executable by this user.");
    assert!(connection_diagnostic(&generic.0[0], &not_exec).starts_with("Not executable:"));
    let denied = AgentError::config("Permission denied: cannot execute /tmp/npx.");
    assert!(connection_diagnostic(&generic.0[0], &denied).starts_with("Permission denied:"));
    let protocol =
        AgentError::msg("MCP client error: Protocol negotiation failed (not with MCP JSON-RPC).");
    assert!(
        connection_diagnostic(&generic.0[0], &protocol).starts_with("Protocol negotiation failed")
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

async fn parked(runtime: &McpRuntime, name: &str) -> Option<ReconnectPark> {
    runtime
        .state
        .lock()
        .await
        .servers
        .get(name)
        .unwrap()
        .reconnect
        .parked
}

/// Retrying a signed-out gateway mount cannot succeed before the next
/// sign-in, so the supervisor stops retrying it (and logging each attempt)
/// until one happens. A manual reconnect still tries.
#[tokio::test]
async fn a_signed_out_gateway_mount_waits_for_a_sign_in_instead_of_retrying() {
    let (runtime, _store, _directory) = test_runtime().await;
    let definitions = vec![gateway_definition("tools", "tools")];
    runtime
        .replace_permissive(definitions.clone(), ids_for(&definitions))
        .await;
    assert_eq!(parked(&runtime, "tools").await, Some(ReconnectPark::SignIn));
    assert!(runtime
        .supervised_servers(ManualLockdown::Open)
        .await
        .is_empty());

    assert!(runtime.reconnect("tools").await.is_err());
    assert_eq!(parked(&runtime, "tools").await, Some(ReconnectPark::SignIn));
    assert_eq!(runtime.info().await.servers[0].health, McpHealth::Degraded);

    runtime.gateway_session_changed().await;
    assert_eq!(parked(&runtime, "tools").await, None);
    let supervised = runtime.supervised_servers(ManualLockdown::Open).await;
    assert_eq!(supervised.len(), 1);
    assert_eq!(supervised[0].0, "tools");
    assert_eq!(supervised[0].2, INITIAL_RECONNECT_BACKOFF);
}

/// A missing parent environment variable cannot appear while Tidebreak runs,
/// so that server stops retrying too. A sign-in does not wake it.
#[tokio::test]
async fn a_server_missing_its_environment_stops_retrying() {
    const MISSING: &str = "TIDEBREAK_TEST_MCP_PARKED_ENV_MUST_NOT_EXIST_7C21";
    assert!(std::env::var_os(MISSING).is_none());
    let (runtime, _store, _directory) = test_runtime().await;
    let mut definition = http_definition("docs", "https://mcp.example.test/mcp");
    definition.bearer_token_env = Some(MISSING.to_string());
    let definitions = vec![definition];
    runtime
        .replace_permissive(definitions.clone(), ids_for(&definitions))
        .await;

    assert_eq!(
        parked(&runtime, "docs").await,
        Some(ReconnectPark::Configuration)
    );
    runtime.gateway_session_changed().await;
    assert_eq!(
        parked(&runtime, "docs").await,
        Some(ReconnectPark::Configuration)
    );
    assert!(runtime
        .supervised_servers(ManualLockdown::Open)
        .await
        .is_empty());
}

/// A server that keeps failing the same way logs that failure once; a new
/// failure logs again, and each failure lengthens the wait.
#[test]
fn a_repeated_reconnect_failure_is_reported_once() {
    let mut reconnect = super::runtime::Reconnect::default();
    assert!(reconnect.failed(None, "Server error: HTTP 503"));
    assert!(!reconnect.failed(None, "Server error: HTTP 503"));
    assert!(reconnect.failed(None, "Timed out after 10000 ms"));
    assert!(reconnect.failed(
        Some(ReconnectPark::SignIn),
        "Sign in to the model gateway to reconnect this server."
    ));
    assert_eq!(reconnect.parked, Some(ReconnectPark::SignIn));
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
        .connect(&gateway, &BTreeMap::new(), None)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains(MISSING));
    assert!(error.to_string().contains("is not set"));

    let diagnostic = connection_diagnostic(&definition, &error);
    assert!(diagnostic.contains(MISSING));
    assert!(diagnostic.contains("Bearer-token environment variable"));
    assert!(diagnostic.contains("shell you start Tidebreak from"));
    assert!(!diagnostic.contains('\n'));
}

#[test]
fn unclassified_http_failures_keep_the_generic_fallback() {
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
        .unwrap()
        .connect()
        .await;
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
    runtime.initialize(boot).await.unwrap().connect().await;
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
        .unwrap()
        .connect()
        .await;
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
        .unwrap()
        .connect()
        .await;

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

async fn verify_fails(definition: McpServerDefinition) -> String {
    let (runtime, _store, _directory) = test_runtime().await;
    runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .expect_err("verification must fail")
        .to_string()
}

async fn serve_http_response(
    status: axum::http::StatusCode,
    content_type: &'static str,
    body: &'static [u8],
) -> std::net::SocketAddr {
    use axum::body::Body;
    use axum::routing::post;

    let body = body.to_vec();
    let app = axum::Router::new().route(
        "/mcp",
        post(move || {
            let body = body.clone();
            async move {
                axum::response::Response::builder()
                    .status(status)
                    .header(axum::http::header::CONTENT_TYPE, content_type)
                    .body(Body::from(body))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

#[tokio::test]
async fn verify_classifies_dns_resolution_failure() {
    let error = verify_fails(http_definition(
        "docs",
        "http://this-host-does-not-exist.invalid/mcp",
    ))
    .await;
    assert!(
        error.contains("DNS resolution failed (this-host-does-not-exist.invalid)"),
        "{error}"
    );
    assert!(!error.contains("Could not connect to this server. Check its URL and credentials."));
}

#[tokio::test]
async fn verify_classifies_tls_handshake_failure() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
                let _ = tokio::io::AsyncWriteExt::write_all(
                    &mut stream,
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n",
                )
                .await;
            });
        }
    });
    let error = verify_fails(http_definition(
        "docs",
        &format!("https://127.0.0.1:{}/mcp", address.port()),
    ))
    .await;
    assert!(error.contains("TLS handshake failed ("), "{error}");
    assert!(!error.contains("Could not connect to this server. Check its URL and credentials."));
}

#[tokio::test]
async fn verify_classifies_http_authentication_failure() {
    let address =
        serve_http_response(axum::http::StatusCode::UNAUTHORIZED, "text/plain", b"").await;
    let error = verify_fails(http_definition("docs", &format!("http://{address}/mcp"))).await;
    assert!(
        error.contains("Authentication failed (401 Unauthorized)"),
        "{error}"
    );
}

#[tokio::test]
async fn verify_classifies_http_forbidden_as_authentication() {
    let address = serve_http_response(axum::http::StatusCode::FORBIDDEN, "text/plain", b"").await;
    let error = verify_fails(http_definition("docs", &format!("http://{address}/mcp"))).await;
    assert!(
        error.contains("Authentication failed (403 Forbidden)"),
        "{error}"
    );
}

#[tokio::test]
async fn verify_classifies_http_404_as_wrong_path() {
    let address = serve_http_response(axum::http::StatusCode::NOT_FOUND, "text/plain", b"").await;
    let error = verify_fails(http_definition("docs", &format!("http://{address}/mcp"))).await;
    assert!(error.contains("Wrong path (404 Not Found)"), "{error}");
}

#[tokio::test]
async fn verify_classifies_http_5xx_as_server_error() {
    let address = serve_http_response(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "text/plain",
        b"",
    )
    .await;
    let error = verify_fails(http_definition("docs", &format!("http://{address}/mcp"))).await;
    assert!(
        error.contains("Server error (500 Internal Server Error)"),
        "{error}"
    );
}

#[tokio::test]
async fn verify_classifies_missing_bearer_token_and_says_where_to_set_it() {
    const MISSING: &str = "TIDEBREAK_TEST_MCP_VERIFY_BEARER_MISSING_C0FFEE";
    assert!(std::env::var_os(MISSING).is_none());
    let mut definition = http_definition("docs", "http://127.0.0.1:1/mcp");
    definition.bearer_token_env = Some(MISSING.to_string());
    let error = verify_fails(definition).await;
    assert!(
        error.contains("Bearer-token environment variable"),
        "{error}"
    );
    assert!(error.contains(MISSING), "{error}");
    assert!(error.contains("shell you start Tidebreak from"), "{error}");
    assert!(error.contains("does not read a .env file"), "{error}");
}

#[tokio::test]
async fn verify_classifies_protocol_negotiation_and_quotes_first_bytes() {
    let address = serve_http_response(
        axum::http::StatusCode::OK,
        "text/html",
        b"<html>not-mcp</html>",
    )
    .await;
    let error = verify_fails(http_definition("docs", &format!("http://{address}/mcp"))).await;
    assert!(error.contains("Protocol negotiation failed"), "{error}");
    assert!(error.contains("not with MCP JSON-RPC"), "{error}");
    assert!(error.contains("<html>not-mcp</html>"), "{error}");
}

#[tokio::test]
async fn verify_classifies_timeout_after_configured_milliseconds() {
    let app = axum::Router::new().route(
        "/mcp",
        axum::routing::post(|| async { std::future::pending::<axum::http::StatusCode>().await }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut definition = http_definition("docs", &format!("http://{address}/mcp"));
    definition.request_timeout_ms = 80;
    let error = verify_fails(definition).await;
    assert!(error.contains("Timed out after 80 ms."), "{error}");
}

#[test]
fn stdio_relative_path_is_refused() {
    let error =
        super::stdio::resolve_stdio_command_on_path("bin/npx", std::ffi::OsStr::new("/bin"))
            .expect_err("relative paths with separators must be refused");
    let diagnostic = error.diagnostic();
    assert!(
        diagnostic.contains("Relative executable path"),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("-y"), "{diagnostic}");
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_bare_npx_resolves_on_overridden_host_path() {
    let directory = tempfile::tempdir().unwrap();
    let npx = directory.path().join("npx");
    std::fs::write(&npx, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).unwrap();
    super::stdio::override_host_path(Some(directory.path().as_os_str().to_os_string()));
    let _guard = super::stdio::HostPathGuard;
    let resolved = super::stdio::resolve_stdio_command("npx").await.unwrap();
    assert_eq!(resolved, npx);

    let definition = parse(r#"{"servers":[{"name":"docs","command":"npx"}]}"#)
        .unwrap()
        .0
        .remove(0);
    assert_eq!(definition.command.as_deref(), Some("npx"));
    let command = definition.build_command(&BTreeMap::new()).await.unwrap();
    assert_eq!(command.as_std().get_program(), npx.as_os_str());

    let missing = super::stdio::resolve_stdio_command("definitely-not-npx-9f3a")
        .await
        .expect_err("missing name must fail");
    let diagnostic = missing.to_string();
    assert!(diagnostic.contains("Command not found"), "{diagnostic}");
    assert!(
        diagnostic.contains(&directory.path().display().to_string()),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("@beeper"), "{diagnostic}");
}

#[tokio::test]
async fn verify_classifies_stdio_command_not_found() {
    let mut definition = disabled_definition("docs", "/definitely-not-an-mcp-binary-9f3a");
    definition.enabled = true;
    let error = verify_fails(definition).await;
    assert!(
        error.contains("Process failed to launch: command not found."),
        "{error}"
    );
}

#[tokio::test]
async fn verify_classifies_stdio_exit_code_and_first_stderr_line() {
    let mut definition = disabled_definition("docs", "/bin/sh");
    definition.enabled = true;
    definition.args = vec![
        "-c".to_string(),
        "printf 'mcp-launch-stderr\\n' >&2; exit 7".to_string(),
    ];
    let error = verify_fails(definition).await;
    assert!(
        error.contains("Process failed to launch: exit code 7."),
        "{error}"
    );
    assert!(
        error.contains("First stderr line: mcp-launch-stderr."),
        "{error}"
    );
}

#[tokio::test]
async fn successful_verify_reports_tool_count_and_enabled_for_new_turns() {
    let address = serve_fake_http_mcp().await;
    let (runtime, _store, _directory) = test_runtime().await;
    let mut definition = http_definition("docs", &format!("http://{address}/mcp"));
    definition.bearer_token_env = Some("PATH".to_string());
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();
    assert_eq!(info.servers[0].health, McpHealth::Healthy);
    assert_eq!(info.servers[0].tool_count, 1);
    assert!(info.servers[0].definition.enabled);
    assert_eq!(info.servers[0].diagnostic, None);

    let mut disabled = http_definition("idle", "http://127.0.0.1:1/mcp");
    disabled.enabled = false;
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![disabled],
        })
        .await
        .unwrap();
    assert_eq!(info.servers[0].health, McpHealth::Disabled);
    assert!(!info.servers[0].definition.enabled);
    assert_eq!(info.servers[0].tool_count, 0);
}

// ---------------------------------------------------------------------------
// OAuth sign-in for a remote server whose saved definition lacks the flag
// ---------------------------------------------------------------------------

/// How the fake authorization server answers the browser.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FakeConsent {
    Approve,
    Deny,
}

/// What a fake server declares. The defaults are a well-behaved server.
#[derive(Clone)]
struct FakeOAuthOptions {
    registration: bool,
    consent: FakeConsent,
    /// The protected resource's `resource`; `None` declares the MCP URL.
    resource: Option<String>,
    /// The authorization server's `issuer`; `None` declares the fake's own.
    issuer: Option<String>,
    pkce_methods: Vec<&'static str>,
}

impl Default for FakeOAuthOptions {
    fn default() -> Self {
        Self {
            registration: true,
            consent: FakeConsent::Approve,
            resource: None,
            issuer: None,
            pkce_methods: vec!["S256"],
        }
    }
}

/// A remote MCP server that requires an OAuth sign-in, shaped the way the MCP
/// authorization specification describes and Vercel's server behaves: the
/// MCP endpoint answers an unauthenticated request with a `401` whose
/// challenge names protected-resource metadata, which names an authorization
/// server offering dynamic client registration, PKCE, and resource
/// indicators. Everything lives on one loopback origin, and every fake issues
/// its own access token, so a test can tell whose token reached whom.
struct FakeOAuthServer {
    origin: String,
    options: FakeOAuthOptions,
    access_token: String,
    /// Registered client ids and their one redirect URI.
    clients: std::sync::Mutex<BTreeMap<String, String>>,
    /// Issued codes, with the PKCE challenge, redirect, and resource they
    /// were issued for.
    codes: std::sync::Mutex<BTreeMap<String, (String, String, String)>>,
    /// The bearer on every request to the MCP endpoint, empty for none.
    mcp_bearers: std::sync::Mutex<Vec<String>>,
    /// Bearers the MCP endpoint saw on tool calls.
    tool_call_bearers: std::sync::Mutex<Vec<String>>,
    /// The resource indicator on each refresh the token endpoint granted.
    refresh_resources: std::sync::Mutex<Vec<String>>,
    /// When not zero, the status the token endpoint answers a refresh with.
    refresh_status: std::sync::atomic::AtomicU16,
    /// When not zero, the status the authorization-server metadata answers.
    metadata_status: std::sync::atomic::AtomicU16,
    /// How long an authorized `initialize` takes, in milliseconds.
    initialize_delay_ms: std::sync::atomic::AtomicU64,
}

impl FakeOAuthServer {
    async fn serve(options: FakeOAuthOptions) -> Arc<Self> {
        use axum::routing::{get, post};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = Arc::new(Self {
            origin: format!("http://{address}"),
            options,
            access_token: format!("fake-access-{}", address.port()),
            clients: Default::default(),
            codes: Default::default(),
            mcp_bearers: Default::default(),
            tool_call_bearers: Default::default(),
            refresh_resources: Default::default(),
            refresh_status: Default::default(),
            metadata_status: Default::default(),
            initialize_delay_ms: Default::default(),
        });
        let app = axum::Router::new()
            .route("/mcp", post(Self::mcp))
            .route(
                "/.well-known/oauth-protected-resource",
                get(Self::protected_resource),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                get(Self::authorization_server),
            )
            .route("/register", post(Self::register))
            .route("/authorize", get(Self::authorize))
            .route("/token", post(Self::token))
            .with_state(Arc::clone(&server));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        server
    }

    async fn approving() -> Arc<Self> {
        Self::serve(FakeOAuthOptions::default()).await
    }

    fn mcp_url(&self) -> String {
        format!("{}/mcp", self.origin)
    }

    fn mcp_bearers(&self) -> Vec<String> {
        self.mcp_bearers.lock().unwrap().clone()
    }

    async fn mcp(
        axum::extract::State(server): axum::extract::State<Arc<Self>>,
        headers: axum::http::HeaderMap,
        body: String,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;

        let bearer = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        server
            .mcp_bearers
            .lock()
            .unwrap()
            .push(bearer.unwrap_or_default().to_string());
        if bearer != Some(server.access_token.as_str()) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                [(
                    "www-authenticate",
                    format!(
                        "Bearer error=\"invalid_token\", error_description=\"No authorization \
                         provided\", resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
                        server.origin
                    ),
                )],
                r#"{"error":"invalid_token"}"#,
            )
                .into_response();
        }
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        let Some(id) = request.get("id").cloned() else {
            return axum::http::StatusCode::ACCEPTED.into_response();
        };
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => {
                let delay = server
                    .initialize_delay_ms
                    .load(std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(delay)).await;
                serde_json::json!({
                    "protocolVersion": tidebreak_mcp::PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "oauth-fixture", "version": "1"}
                })
            }
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": "list_projects",
                    "description": "List projects",
                    "inputSchema": {"type": "object"}
                }]
            }),
            "tools/call" => {
                server
                    .tool_call_bearers
                    .lock()
                    .unwrap()
                    .push(bearer.unwrap_or_default().to_string());
                serde_json::json!({
                    "content": [{"type": "text", "text": "two projects"}],
                    "isError": false
                })
            }
            _ => serde_json::json!({}),
        };
        (
            [("content-type", "application/json")],
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        )
            .into_response()
    }

    async fn protected_resource(
        axum::extract::State(server): axum::extract::State<Arc<Self>>,
    ) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "resource": server.options.resource.clone().unwrap_or_else(|| server.mcp_url()),
            "authorization_servers": [server.origin],
            "scopes_supported": ["projects:read"]
        }))
    }

    async fn authorization_server(
        axum::extract::State(server): axum::extract::State<Arc<Self>>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;

        let status = server
            .metadata_status
            .load(std::sync::atomic::Ordering::SeqCst);
        if status != 0 {
            return axum::http::StatusCode::from_u16(status)
                .unwrap()
                .into_response();
        }
        let mut metadata = serde_json::json!({
            "issuer": server.options.issuer.clone().unwrap_or_else(|| server.origin.clone()),
            "authorization_endpoint": format!("{}/authorize", server.origin),
            "token_endpoint": format!("{}/token", server.origin),
            "code_challenge_methods_supported": server.options.pkce_methods
        });
        if server.options.registration {
            metadata["registration_endpoint"] =
                serde_json::json!(format!("{}/register", server.origin));
        }
        axum::Json(metadata).into_response()
    }

    async fn register(
        axum::extract::State(server): axum::extract::State<Arc<Self>>,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> axum::Json<serde_json::Value> {
        assert_eq!(body["token_endpoint_auth_method"], "none");
        let redirect = body["redirect_uris"][0].as_str().unwrap().to_string();
        let mut clients = server.clients.lock().unwrap();
        let client_id = format!("fake-client-{}", clients.len() + 1);
        clients.insert(client_id.clone(), redirect);
        axum::Json(serde_json::json!({"client_id": client_id}))
    }

    async fn authorize(
        axum::extract::State(server): axum::extract::State<Arc<Self>>,
        axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;

        let redirect = &query["redirect_uri"];
        assert_eq!(
            server.clients.lock().unwrap().get(&query["client_id"]),
            Some(redirect),
            "the redirect must be the one this client registered"
        );
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["scope"], "projects:read");
        let state = &query["state"];
        let location = match server.options.consent {
            FakeConsent::Deny => format!("{redirect}?error=access_denied&state={state}"),
            FakeConsent::Approve => {
                let code = format!("code-{}", uuid::Uuid::new_v4());
                server.codes.lock().unwrap().insert(
                    code.clone(),
                    (
                        query["code_challenge"].clone(),
                        redirect.clone(),
                        query["resource"].clone(),
                    ),
                );
                format!("{redirect}?code={code}&state={state}")
            }
        };
        (axum::http::StatusCode::FOUND, [("location", location)]).into_response()
    }

    async fn token(
        axum::extract::State(server): axum::extract::State<Arc<Self>>,
        axum::Form(form): axum::Form<BTreeMap<String, String>>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        use base64::Engine as _;
        use sha2::Digest as _;

        let tokens = axum::Json(serde_json::json!({
            "access_token": server.access_token,
            "token_type": "Bearer",
            "expires_in": 3600,
            "refresh_token": "fake-refresh-token-3521",
            "scope": "projects:read"
        }));
        if form["grant_type"] == "refresh_token" {
            let status = server
                .refresh_status
                .load(std::sync::atomic::Ordering::SeqCst);
            if status != 0 {
                return axum::http::StatusCode::from_u16(status)
                    .unwrap()
                    .into_response();
            }
            assert_eq!(form["refresh_token"], "fake-refresh-token-3521");
            server
                .refresh_resources
                .lock()
                .unwrap()
                .push(form.get("resource").cloned().unwrap_or_default());
            return tokens.into_response();
        }
        assert_eq!(form["grant_type"], "authorization_code");
        let Some((challenge, redirect, resource)) =
            server.codes.lock().unwrap().remove(&form["code"])
        else {
            return (axum::http::StatusCode::BAD_REQUEST, "invalid_grant").into_response();
        };
        let verified = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(form["code_verifier"].as_bytes()));
        assert_eq!(
            verified, challenge,
            "PKCE verifier must match the challenge"
        );
        assert_eq!(form["redirect_uri"], redirect);
        assert_eq!(form["resource"], resource);
        assert_eq!(
            resource,
            server.mcp_url(),
            "the resource indicator names the server"
        );
        tokens.into_response()
    }
}

/// A runtime whose OAuth client admits the fake's loopback origin.
async fn oauth_test_runtime() -> (Arc<McpRuntime>, Arc<dyn Store>, tempfile::TempDir) {
    let (runtime, store, directory) = test_runtime().await;
    runtime.admit_loopback_oauth_for_tests();
    (runtime, store, directory)
}

/// Play the person's browser: open the authorization page, then follow the
/// authorization server's redirect to the loopback callback.
async fn complete_browser_sign_in(page: &str) {
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap();
    let redirect = browser.get(page).send().await.unwrap();
    assert_eq!(redirect.status(), reqwest::StatusCode::FOUND);
    let callback = redirect.headers()["location"].to_str().unwrap().to_string();
    let landed = browser.get(&callback).send().await.unwrap();
    assert!(landed.status().is_success(), "{}", landed.status());
}

/// Poll `info` until `done` holds, so a test can wait on the background half
/// of a sign-in without sleeping a fixed time.
async fn info_when(runtime: &McpRuntime, done: impl Fn(&McpServerInfo) -> bool) -> McpServersInfo {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let info = runtime.info().await;
        if done(&info.servers[0]) {
            return info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the sign-in did not settle: {:?}",
            info.servers[0]
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn oauth_state(server: &McpServerInfo) -> Option<crate::mcp_oauth_runtime::McpOAuthState> {
    server.oauth_status.as_ref().map(|status| status.state)
}

/// Save `name` at `fake`'s URL, sign in through the browser, and wait until
/// its tools load.
async fn sign_in_to(runtime: &Arc<McpRuntime>, name: &str, fake: &FakeOAuthServer) {
    runtime
        .replace(McpServersConfig {
            servers: vec![http_definition(name, &fake.mcp_url())],
        })
        .await
        .expect("a server that asks for a sign-in still saves");
    let status = runtime.oauth_connect(name).await.unwrap();
    complete_browser_sign_in(&status.pending_authorization_url.unwrap()).await;
    info_when(runtime, |server| server.health == McpHealth::Healthy).await;
}

/// Issue #3521: an imported remote server, saved without the OAuth flag,
/// answered `401` and the save failed with "Authentication failed (401
/// Unauthorized)" and no way to sign in. Now the challenge leads to a visible
/// sign-in-required state and Connect, the browser's return stores the
/// session, and the tools load and are called with the token.
#[tokio::test]
async fn an_imported_server_that_asks_for_oauth_signs_in_and_loads_its_tools() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    // Exactly what the import builds for `{"url": "..."}`: the flag is off.
    let definition = http_definition("vercel", &fake.mcp_url());
    assert!(!definition.oauth);

    let info = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .expect("a server that asks for a sign-in still saves");
    let server = &info.servers[0];
    assert_eq!(server.health, McpHealth::Degraded);
    assert_eq!(oauth_state(server), Some(McpOAuthState::NotConnected));
    // The row names where Connect goes before the person selects it.
    assert_eq!(
        server
            .oauth_status
            .as_ref()
            .and_then(|status| status.sign_in_host.as_deref()),
        Some("127.0.0.1")
    );
    let diagnostic = server.diagnostic.as_deref().unwrap();
    assert!(diagnostic.contains("needs you to sign in"), "{diagnostic}");
    assert!(!diagnostic.contains("401"), "{diagnostic}");
    assert!(!diagnostic.contains("127.0.0.1"), "{diagnostic}");
    // Retrying cannot help until someone signs in, and a model-gateway
    // sign-in is not that.
    assert_eq!(
        parked(&runtime, "vercel").await,
        Some(ReconnectPark::Authorization)
    );
    runtime.gateway_session_changed().await;
    assert_eq!(
        parked(&runtime, "vercel").await,
        Some(ReconnectPark::Authorization)
    );

    // Connect answers at once with the page the desktop opens.
    let status = runtime.oauth_connect("vercel").await.unwrap();
    assert_eq!(status.state, McpOAuthState::Authorizing);
    let page = status.pending_authorization_url.clone().unwrap();
    assert!(
        page.starts_with(&format!("{}/authorize?", fake.origin)),
        "{page}"
    );
    let pending = runtime.info().await;
    assert_eq!(
        pending.servers[0].oauth_status.as_ref(),
        Some(&status),
        "the pending page stays available to reopen"
    );

    complete_browser_sign_in(&page).await;
    let info = info_when(&runtime, |server| server.health == McpHealth::Healthy).await;
    let server = &info.servers[0];
    assert_eq!(oauth_state(server), Some(McpOAuthState::Connected));
    assert_eq!(server.tool_count, 1);
    assert_eq!(server.diagnostic, None);
    assert_eq!(parked(&runtime, "vercel").await, None);

    // Stored the way the OAuth path stores sessions: in the credential store
    // under the record id, bound to this server's URL, never in the
    // definition or the projection.
    let record = saved_records(&store).await.remove(0);
    let secrets = runtime.secrets();
    let token = secrets
        .get_secret(&crate::connectors::oauth_token_secret_key(record.id))
        .await
        .unwrap()
        .expect("the session is stored");
    assert!(token.contains(&fake.access_token));
    let registration = secrets
        .get_secret(&crate::connectors::oauth_client_secret_key(record.id))
        .await
        .unwrap()
        .expect("the registration is stored");
    let registration: crate::connectors::ClientRegistration =
        serde_json::from_str(&registration).unwrap();
    assert_eq!(
        registration.server_url.as_deref(),
        Some(fake.mcp_url().as_str())
    );
    assert!(!record.definition.to_string().contains(&fake.access_token));
    assert!(!serde_json::to_string(&info)
        .unwrap()
        .contains(&fake.access_token));
    assert!(
        !saved_definitions(&store).await[0].oauth,
        "nothing rewrote the saved flag"
    );

    // A tool call presents the session's token.
    let output = runtime
        .snapshot()
        .get("mcp__vercel__list_projects")
        .expect("the server's tools are mounted")
        .execute(
            &tidebreak_core::ToolCtx::new_legacy_workspace(
                tidebreak_core::SessionId::new(),
                None,
                std::path::PathBuf::from("unused-by-mcp"),
            ),
            serde_json::json!({}),
        )
        .await
        .unwrap();
    assert_eq!(output.content, "two projects");
    assert_eq!(
        fake.tool_call_bearers.lock().unwrap().as_slice(),
        std::slice::from_ref(&fake.access_token)
    );

    // Disconnect clears the session, and the server asks for a sign-in again.
    let status = runtime.oauth_disconnect("vercel").await.unwrap();
    assert_eq!(status.state, McpOAuthState::NotConnected);
    assert!(secrets
        .get_secret(&crate::connectors::oauth_token_secret_key(record.id))
        .await
        .unwrap()
        .is_none());
    let info = runtime.info().await;
    assert_eq!(
        oauth_state(&info.servers[0]),
        Some(McpOAuthState::NotConnected)
    );
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
    assert!(runtime
        .snapshot()
        .get("mcp__vercel__list_projects")
        .is_none());
}

/// Between the browser's return and the reconnect, the server must read as
/// signed in and connecting: never as a rejected or missing sign-in, which
/// would stop the panel's polling on a Reconnect button that starts a second
/// sign-in.
#[tokio::test]
async fn a_finished_sign_in_goes_straight_from_waiting_to_connected() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    // A slow authorized `initialize` holds the reconnect open long enough to
    // be seen.
    fake.initialize_delay_ms
        .store(600, std::sync::atomic::Ordering::SeqCst);
    let (runtime, _store, _directory) = oauth_test_runtime().await;
    runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("vercel", &fake.mcp_url())],
        })
        .await
        .unwrap();
    let status = runtime.oauth_connect("vercel").await.unwrap();
    complete_browser_sign_in(&status.pending_authorization_url.unwrap()).await;

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let info = runtime.info().await;
        let server = &info.servers[0];
        seen.push((oauth_state(server), server.health));
        if server.health == McpHealth::Healthy {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{seen:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        seen.iter().all(|(state, _)| matches!(
            state,
            Some(McpOAuthState::Authorizing | McpOAuthState::Connected)
        )),
        "{seen:?}"
    );
    assert!(
        seen.contains(&(Some(McpOAuthState::Connected), McpHealth::Reconnecting)),
        "{seen:?}"
    );
}

/// Finding: after a URL edit, server A's session went to server B. The
/// record keeps its id when the name stays, so the session must be bound to
/// the URL and cleared when the URL changes.
#[tokio::test]
async fn editing_a_servers_url_never_sends_its_session_to_the_new_address() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let a = FakeOAuthServer::approving().await;
    let b = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    sign_in_to(&runtime, "docs", &a).await;
    let id = saved_records(&store).await[0].id;

    runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("docs", &b.mcp_url())],
        })
        .await
        .expect("the edited server asks for its own sign-in and saves");
    assert_eq!(saved_records(&store).await[0].id, id, "the id is kept");
    assert!(
        !b.mcp_bearers().contains(&a.access_token),
        "{:?}",
        b.mcp_bearers()
    );
    assert!(b.mcp_bearers().iter().all(String::is_empty));
    let secrets = runtime.secrets();
    for key in [
        crate::connectors::oauth_token_secret_key(id),
        crate::connectors::oauth_client_secret_key(id),
    ] {
        assert!(secrets.get_secret(&key).await.unwrap().is_none(), "{key}");
    }
    let info = runtime.info().await;
    assert_eq!(
        oauth_state(&info.servers[0]),
        Some(McpOAuthState::NotConnected)
    );

    // A later reconnect and a tool call find nothing to send either.
    let _ = runtime.reconnect("docs").await;
    assert!(b.mcp_bearers().iter().all(String::is_empty));
}

/// Finding: removing a server left its session behind for a later server of
/// the same name. Removal clears it, and the re-added server starts over.
#[tokio::test]
async fn a_removed_server_takes_its_session_with_it() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let a = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    sign_in_to(&runtime, "docs", &a).await;
    let first = saved_records(&store).await[0].id;

    runtime
        .replace(McpServersConfig {
            servers: Vec::new(),
        })
        .await
        .unwrap();
    let secrets = runtime.secrets();
    for key in [
        crate::connectors::oauth_token_secret_key(first),
        crate::connectors::oauth_client_secret_key(first),
    ] {
        assert!(secrets.get_secret(&key).await.unwrap().is_none(), "{key}");
    }

    let before = a.mcp_bearers().len();
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("docs", &a.mcp_url())],
        })
        .await
        .unwrap();
    assert_eq!(
        oauth_state(&info.servers[0]),
        Some(McpOAuthState::NotConnected)
    );
    assert!(a.mcp_bearers()[before..].iter().all(String::is_empty));
}

/// A stored session is presented only to the exact URL it was issued for.
/// One bound elsewhere, or written before sessions were bound, is never
/// sent, even when nothing has cleared it yet.
#[tokio::test]
async fn a_session_bound_to_another_url_is_never_presented() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let a = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("docs", &a.mcp_url())],
        })
        .await
        .unwrap();
    let id = saved_records(&store).await[0].id;
    let vault = crate::connectors::McpOAuthCredentialVault::new(runtime.secrets(), id);
    for server_url in [Some("https://elsewhere.example/mcp".to_string()), None] {
        vault
            .save_registration(&crate::connectors::ClientRegistration {
                client_id: "fake-client".to_string(),
                client_secret: None,
                registration_access_token: None,
                registration_client_uri: None,
                token_endpoint: Some(format!("{}/token", a.origin)),
                scopes: Vec::new(),
                resource: None,
                server_url,
                sign_in_host: None,
            })
            .await
            .unwrap();
        vault
            .save(&crate::connectors::McpOAuthCredentials {
                // A's real token: it would be accepted if it were sent.
                access_token: a.access_token.clone(),
                refresh_token: None,
                expires_at_unix: u64::MAX / 2,
                scope: None,
            })
            .await
            .unwrap();
        let before = a.mcp_bearers().len();
        assert!(runtime.reconnect("docs").await.is_err());
        assert!(a.mcp_bearers()[before..].iter().all(String::is_empty));
        assert_eq!(
            oauth_state(&runtime.info().await.servers[0]),
            Some(McpOAuthState::NotConnected)
        );
    }
}

/// Finding: a token service outage parked a signed-in server as needing a
/// sign-in. A refresh the service does not answer keeps the session, keeps
/// the usual retry, and says it is temporary.
#[tokio::test]
async fn a_token_service_outage_keeps_the_session_and_retries() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    sign_in_to(&runtime, "vercel", &fake).await;
    let id = saved_records(&store).await[0].id;
    let vault = crate::connectors::McpOAuthCredentialVault::new(runtime.secrets(), id);
    // Age the access token so the next connection must refresh it.
    let mut credentials = vault.load().await.unwrap().unwrap();
    credentials.expires_at_unix = 1;
    vault.save(&credentials).await.unwrap();
    fake.refresh_status
        .store(503, std::sync::atomic::Ordering::SeqCst);

    let before = fake.mcp_bearers().len();
    let error = runtime
        .reconnect("vercel")
        .await
        .expect_err("the refresh failed")
        .to_string();
    assert!(error.contains("Sign-in service unavailable"), "{error}");
    // It did not connect without the token and read the `401` as a sign-in.
    assert_eq!(fake.mcp_bearers().len(), before);
    assert_eq!(parked(&runtime, "vercel").await, None);
    let info = runtime.info().await;
    assert_eq!(
        oauth_state(&info.servers[0]),
        Some(McpOAuthState::Connected)
    );
    assert!(info.servers[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .starts_with("Sign-in service unavailable"));
    assert!(vault.load().await.unwrap().unwrap().refresh_token.is_some());

    // The service answers again, and the next retry refreshes and connects.
    fake.refresh_status
        .store(0, std::sync::atomic::Ordering::SeqCst);
    runtime.reconnect("vercel").await.unwrap();
    assert_eq!(runtime.info().await.servers[0].health, McpHealth::Healthy);
    assert_eq!(*fake.refresh_resources.lock().unwrap(), [fake.mcp_url()]);
}

/// Finding: a sign-in service outage read as "Sign-in not supported" and
/// stopped the retries. It is temporary: the server saves, says the service
/// did not answer, keeps retrying, and asks for a sign-in once it answers.
#[tokio::test]
async fn a_sign_in_service_outage_is_temporary_not_unsupported() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    fake.metadata_status
        .store(503, std::sync::atomic::Ordering::SeqCst);
    let (runtime, _store, _directory) = oauth_test_runtime().await;
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("vercel", &fake.mcp_url())],
        })
        .await
        .expect("a server that asks for a sign-in saves while its service is down");
    let status = info.servers[0].oauth_status.clone().unwrap();
    assert_eq!(status.state, McpOAuthState::NotConnected);
    assert!(
        status.error.as_deref().unwrap().contains("did not answer"),
        "{status:?}"
    );
    assert_eq!(parked(&runtime, "vercel").await, None);

    fake.metadata_status
        .store(0, std::sync::atomic::Ordering::SeqCst);
    let _ = runtime.reconnect("vercel").await;
    let status = runtime.info().await.servers[0]
        .oauth_status
        .clone()
        .unwrap();
    assert_eq!(status.state, McpOAuthState::NotConnected);
    assert_eq!(status.error, None);
    assert_eq!(
        parked(&runtime, "vercel").await,
        Some(ReconnectPark::Authorization)
    );
}

/// Metadata that describes another server, names another issuer, or rules
/// out S256 is not used, and the save says which.
#[tokio::test]
async fn sign_in_metadata_that_fails_the_specification_checks_is_refused() {
    let cases = [
        (
            FakeOAuthOptions {
                resource: Some("https://elsewhere.example/mcp".to_string()),
                ..FakeOAuthOptions::default()
            },
            crate::connectors::OAuthUnsupported::ResourceMismatch,
        ),
        (
            FakeOAuthOptions {
                issuer: Some("https://issuer.elsewhere.example".to_string()),
                ..FakeOAuthOptions::default()
            },
            crate::connectors::OAuthUnsupported::IssuerMismatch,
        ),
        (
            FakeOAuthOptions {
                pkce_methods: vec!["plain"],
                ..FakeOAuthOptions::default()
            },
            crate::connectors::OAuthUnsupported::NoS256,
        ),
    ];
    for (options, reason) in cases {
        let fake = FakeOAuthServer::serve(options).await;
        let (runtime, _store, _directory) = oauth_test_runtime().await;
        let error = runtime
            .replace(McpServersConfig {
                servers: vec![http_definition("vercel", &fake.mcp_url())],
            })
            .await
            .expect_err("a sign-in Tidebreak does not trust fails the save")
            .to_string();
        assert!(error.contains(reason.reason()), "{reason:?}: {error}");
        assert!(fake.clients.lock().unwrap().is_empty(), "{reason:?}");
    }
}

/// Cancel stops a sign-in that waits on the browser: its page no longer
/// lands, nothing is stored, and the server asks for a sign-in again.
#[tokio::test]
async fn cancel_stops_a_waiting_sign_in() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("vercel", &fake.mcp_url())],
        })
        .await
        .unwrap();
    let page = runtime
        .oauth_connect("vercel")
        .await
        .unwrap()
        .pending_authorization_url
        .unwrap();
    let status = runtime.oauth_cancel("vercel").await.unwrap();
    assert_eq!(status.state, McpOAuthState::NotConnected);
    assert_eq!(status.error, None);

    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .unwrap();
    let callback = browser.get(&page).send().await.unwrap().headers()["location"]
        .to_str()
        .unwrap()
        .to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while browser.get(&callback).send().await.is_ok() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the canceled sign-in still listens"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let id = saved_records(&store).await[0].id;
    assert!(runtime
        .secrets()
        .get_secret(&crate::connectors::oauth_token_secret_key(id))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        oauth_state(&runtime.info().await.servers[0]),
        Some(McpOAuthState::NotConnected)
    );
}

/// Declining on the authorization page lands in a visible, retryable state
/// that says what happened, not in a silent failure.
#[tokio::test]
async fn a_declined_sign_in_says_so_and_offers_another_try() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::serve(FakeOAuthOptions {
        consent: FakeConsent::Deny,
        ..FakeOAuthOptions::default()
    })
    .await;
    let (runtime, _store, _directory) = oauth_test_runtime().await;
    runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("vercel", &fake.mcp_url())],
        })
        .await
        .unwrap();
    let status = runtime.oauth_connect("vercel").await.unwrap();
    complete_browser_sign_in(&status.pending_authorization_url.unwrap()).await;

    let info = info_when(&runtime, |server| {
        oauth_state(server) != Some(McpOAuthState::Authorizing)
    })
    .await;
    let status = info.servers[0].oauth_status.clone().unwrap();
    assert_eq!(status.state, McpOAuthState::AccessDenied);
    assert!(
        status
            .error
            .as_deref()
            .unwrap()
            .contains("canceled or denied"),
        "{status:?}"
    );
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
}

/// A server that asks for OAuth but offers no way for Tidebreak to register
/// is refused with a plain reason, not with the `401` it answered.
#[tokio::test]
async fn a_server_without_client_registration_says_sign_in_is_unsupported() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::serve(FakeOAuthOptions {
        registration: false,
        ..FakeOAuthOptions::default()
    })
    .await;
    let (runtime, _store, _directory) = oauth_test_runtime().await;
    let error = runtime
        .replace(McpServersConfig {
            servers: vec![http_definition("vercel", &fake.mcp_url())],
        })
        .await
        .expect_err("a sign-in Tidebreak cannot complete fails the save")
        .to_string();
    assert!(
        error.contains("Tidebreak cannot complete its sign-in"),
        "{error}"
    );
    assert!(error.contains("dynamic client registration"), "{error}");
    assert!(!error.contains("401"), "{error}");

    // At boot it degrades instead, says why, and stops retrying.
    let definitions = vec![http_definition("vercel", &fake.mcp_url())];
    runtime
        .replace_permissive(definitions.clone(), ids_for(&definitions))
        .await;
    let info = runtime.info().await;
    let status = info.servers[0].oauth_status.clone().unwrap();
    assert_eq!(status.state, McpOAuthState::Unsupported);
    assert!(status
        .error
        .as_deref()
        .unwrap()
        .contains("dynamic client registration"));
    assert_eq!(
        parked(&runtime, "vercel").await,
        Some(ReconnectPark::Authorization)
    );
    let connect = runtime.oauth_connect("vercel").await.unwrap();
    assert_eq!(connect.state, McpOAuthState::Unsupported);
}

/// A server configured with a static bearer keeps that path: a `401` means
/// the token is wrong, so there is no sign-in to offer.
#[tokio::test]
async fn a_static_bearer_server_never_offers_a_sign_in() {
    let fake = FakeOAuthServer::approving().await;
    let (runtime, _store, _directory) = oauth_test_runtime().await;
    let mut definition = http_definition("vercel", &fake.mcp_url());
    // PATH always exists and is never the fake's token.
    definition.bearer_token_env = Some("PATH".to_string());
    let error = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .expect_err("a rejected static token fails the save")
        .to_string();
    assert!(
        error.contains("Authentication failed (401 Unauthorized)"),
        "{error}"
    );
    assert!(runtime.info().await.servers.is_empty());
}

// ---------------------------------------------------------------------------
// Boot: saved servers publish without a network wait and connect in the
// background, and a saved record that cannot load is skipped, not fatal.
// ---------------------------------------------------------------------------

/// A Streamable HTTP MCP server with one tool that answers nothing until
/// `release` turns true, so a connection to it waits as long as a test wants.
async fn serve_held_http_mcp(
    tool: &'static str,
    release: tokio::sync::watch::Receiver<bool>,
) -> std::net::SocketAddr {
    use axum::routing::post;

    let app = axum::Router::new().route(
        "/mcp",
        post(move |body: String| {
            let mut release = release.clone();
            async move {
                let _ = release.wait_for(|released| *released).await;
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let id = request.get("id").cloned().unwrap_or_default();
                let result = match request["method"].as_str().unwrap_or_default() {
                    "initialize" => serde_json::json!({
                        "protocolVersion": tidebreak_mcp::PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "held-fixture", "version": "1"}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [{
                            "name": tool,
                            "description": "Look something up",
                            "inputSchema": {"type": "object"}
                        }]
                    }),
                    _ => serde_json::json!({}),
                };
                (
                    [("content-type", "application/json")],
                    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

/// The listed server named `name`.
fn listed<'a>(info: &'a McpServersInfo, name: &str) -> &'a McpServerInfo {
    info.servers
        .iter()
        .find(|server| server.definition.name == name)
        .unwrap_or_else(|| panic!("{name} is listed"))
}

/// Poll `info` until `done` holds for the whole list.
async fn servers_when(
    runtime: &McpRuntime,
    done: impl Fn(&McpServersInfo) -> bool,
) -> McpServersInfo {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let info = runtime.info().await;
        if done(&info) {
            return info;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the servers did not settle: {:?}",
            info.servers
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A saved server that is slow to answer holds up neither boot nor the
/// server beside it. `initialize` publishes both as connecting without
/// touching the network, each publishes its tools the moment it is up, and
/// work that starts meanwhile waits a few seconds at most. The wait is one
/// deadline measured from boot, not a fresh wait for each caller: once it
/// passes, nothing waits, and each caller learns which server is still
/// connecting.
#[tokio::test]
async fn a_slow_saved_server_holds_up_neither_boot_nor_its_neighbors() {
    let (release_slow, held) = tokio::sync::watch::channel(false);
    let slow = serve_held_http_mcp("slow_lookup", held).await;
    let (_open, open) = tokio::sync::watch::channel(true);
    let fast = serve_held_http_mcp("fast_lookup", open).await;
    let (runtime, store, _directory) = test_runtime().await;
    seed_records(
        &store,
        &[
            http_definition("slow", &format!("http://{slow}/mcp")),
            http_definition("fast", &format!("http://{fast}/mcp")),
        ],
    )
    .await;

    let boot = tokio::time::timeout(
        Duration::from_secs(5),
        runtime.initialize(ConfiguredMcpServers::default()),
    )
    .await
    .expect("loading the saved servers waits on no server")
    .unwrap();
    let info = runtime.info().await;
    assert!(
        info.servers
            .iter()
            .all(|server| server.health == McpHealth::Initializing),
        "{:?}",
        info.servers
    );
    assert!(runtime.snapshot().get("mcp__fast__fast_lookup").is_none());

    let connecting = tokio::spawn(boot.connect());
    let info = servers_when(&runtime, |info| {
        listed(info, "fast").health == McpHealth::Healthy
    })
    .await;
    assert_eq!(listed(&info, "slow").health, McpHealth::Initializing);
    assert!(runtime.snapshot().get("mcp__fast__fast_lookup").is_some());

    // Work that starts now waits for the slow server, but not for long.
    let started = std::time::Instant::now();
    let view = runtime.tools_after_boot().await;
    assert!(
        started.elapsed() < BOOT_TOOLS_WAIT + Duration::from_secs(2),
        "a turn waited {:?} on a server that never answered",
        started.elapsed()
    );
    assert!(view.registry.get("mcp__fast__fast_lookup").is_some());
    assert!(view.registry.get("mcp__slow__slow_lookup").is_none());
    assert_eq!(view.connecting, ["slow"]);

    // The deadline has passed, so the next caller does not wait again.
    let started = std::time::Instant::now();
    let view = runtime.tools_after_boot().await;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "a second caller waited {:?} after the boot deadline passed",
        started.elapsed()
    );
    assert_eq!(view.connecting, ["slow"]);
    let before = view.fingerprint;
    assert_eq!(*runtime.tool_changes().borrow(), before);

    release_slow.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(15), connecting)
        .await
        .expect("boot connections settle once the server answers")
        .unwrap();
    let started = std::time::Instant::now();
    let view = runtime.tools_after_boot().await;
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(view.registry.get("mcp__slow__slow_lookup").is_some());
    assert!(view.connecting.is_empty());
    // The change reached anyone watching for one.
    assert_ne!(view.fingerprint, before);
    assert_eq!(*runtime.tool_changes().borrow(), view.fingerprint);
    assert_eq!(
        listed(&runtime.info().await, "slow").health,
        McpHealth::Healthy
    );
}

/// The gateway's apps reach `create_app`'s roster while a saved server is
/// still connecting after boot, and a server that connects later keeps them.
/// The roster read used to wait for every boot connection, and a connection
/// that landed after an add wrote the roster without the gateway's apps.
#[tokio::test]
async fn the_gateway_roster_arrives_before_a_slow_server_and_stays() {
    struct CreateApp;

    #[async_trait::async_trait]
    impl tidebreak_core::Tool for CreateApp {
        fn spec(&self) -> tidebreak_core::ToolSpec {
            tidebreak_core::ToolSpec {
                name: tidebreak_core::local_app::CREATE_APP_TOOL.into(),
                description: "Create an app.".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> tidebreak_core::ApprovalClass {
            tidebreak_core::ApprovalClass::Sensitive
        }

        async fn execute(
            &self,
            _ctx: &tidebreak_core::ToolCtx,
            _args: serde_json::Value,
        ) -> Result<tidebreak_core::ToolOutput> {
            Ok(tidebreak_core::ToolOutput::text(""))
        }
    }

    struct RosterGateway;

    #[async_trait::async_trait]
    impl GatewayEndpoints for RosterGateway {
        async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
            Err(AgentError::SignInRequired(
                "no gateway session is stored".to_string(),
            ))
        }

        async fn entitled_app_catalogs(&self) -> Vec<GatewayRosterApp> {
            vec![GatewayRosterApp {
                id: "app-incident".to_string(),
                name: "Incident API".to_string(),
                operation_ids: vec!["listIncidents".to_string()],
            }]
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("mcp.db").display()
        ))
        .await
        .unwrap(),
    );
    let runtime = Arc::new(McpRuntime::new(
        Arc::new(ToolRegistry::new().with(Box::new(CreateApp))),
        store.clone(),
        Arc::new(TestSecrets::default()),
        Arc::new(RosterGateway),
        Arc::new(crate::managed_policy::ProvisionedPolicyFile::in_data_dir(
            directory.path(),
        )),
        Arc::new(crate::managed_policy::NoOsPolicy),
    ));
    let roster = |runtime: &McpRuntime| {
        runtime
            .snapshot()
            .specs()
            .into_iter()
            .find(|spec| spec.name == tidebreak_core::local_app::CREATE_APP_TOOL)
            .expect("create_app is registered")
            .description
    };
    let (release_slow, held) = tokio::sync::watch::channel(false);
    let slow = serve_held_http_mcp("slow_lookup", held).await;
    seed_records(
        &store,
        &[http_definition("slow", &format!("http://{slow}/mcp"))],
    )
    .await;

    let boot = runtime
        .initialize(ConfiguredMcpServers::default())
        .await
        .unwrap();
    let connecting = tokio::spawn(boot.connect());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !roster(&runtime).contains("app-incident") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the gateway roster waited for the slow server: {}",
            roster(&runtime)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        listed(&runtime.info().await, "slow").health,
        McpHealth::Initializing
    );

    // An add publishes while the slow server is still connecting.
    let mut later = http_definition("later", "https://mcp.example.test/mcp");
    later.enabled = false;
    runtime
        .add_server(later, ManualLockdown::Open)
        .await
        .unwrap();
    release_slow.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(15), connecting)
        .await
        .expect("boot connections settle once the server answers")
        .unwrap();
    assert_eq!(
        listed(&runtime.info().await, "slow").health,
        McpHealth::Healthy
    );
    assert!(
        roster(&runtime).contains("app-incident"),
        "a later connection dropped the gateway roster: {}",
        roster(&runtime)
    );
}

/// A saved record that does not decode, or decodes but fails validation,
/// used to fail boot. Now it is skipped with a reason that quotes no value,
/// the other servers load, and the record stays on file: a save writes it
/// back unchanged, its name stays taken, and only removing it deletes it,
/// with what was stored under its id.
#[tokio::test]
async fn a_saved_record_that_cannot_load_is_skipped_and_kept_on_file() {
    let (runtime, store, _directory) = test_runtime().await;
    let now = chrono::Utc::now();
    let record = |name: &str, definition: serde_json::Value| ConnectedApp {
        id: ConnectedAppId::new(),
        name: name.to_string(),
        kind: ConnectedAppKind::McpServer,
        definition,
        created_at: now,
        updated_at: now,
    };
    let good = record(
        "docs",
        serde_json::to_value(disabled_definition("docs", "/bin/docs")).unwrap(),
    );
    // Written by a newer build: a setting this one does not know.
    let newer = record(
        "newer",
        serde_json::json!({
            "name": "newer",
            "url": "https://mcp.example.test/mcp",
            "transport": "streamable_http"
        }),
    );
    // A value of the wrong type, which must never reach the reason.
    let mangled = record(
        "mangled",
        serde_json::json!({"name": "mangled", "command": "/bin/tool", "args": "--token=do-not-echo"}),
    );
    // Decodes, but names two transports at once.
    let invalid = record(
        "invalid",
        serde_json::json!({
            "name": "invalid",
            "command": "/bin/tool",
            "url": "https://mcp.example.test/mcp"
        }),
    );
    store
        .replace_connected_apps(
            ConnectedAppKind::McpServer,
            &[good, newer.clone(), mangled.clone(), invalid.clone()],
        )
        .await
        .unwrap();
    runtime
        .secrets()
        .set_secret(&env_secret_key(newer.id), r#"{"TOKEN":"stored"}"#)
        .await
        .unwrap();

    runtime
        .initialize(ConfiguredMcpServers::default())
        .await
        .expect("a record that cannot load does not fail boot")
        .connect()
        .await;

    let info = runtime.info().await;
    assert_eq!(info.servers.len(), 1);
    assert_eq!(info.servers[0].definition.name, "docs");
    let skipped = runtime.skipped_servers().await;
    let names: Vec<&str> = skipped
        .iter()
        .map(|skipped| skipped.name.as_str())
        .collect();
    assert_eq!(names, ["newer", "mangled", "invalid"]);
    assert_eq!(skipped[0].id, newer.id);
    assert!(
        skipped[0].reason.contains("\"transport\""),
        "{}",
        skipped[0].reason
    );
    assert!(
        !skipped[1].reason.contains("do-not-echo"),
        "{}",
        skipped[1].reason
    );
    assert!(
        skipped[2]
            .reason
            .contains("must configure exactly one of command, url, or gateway endpoint"),
        "{}",
        skipped[2].reason
    );

    // A save writes the skipped records back exactly as they were.
    runtime
        .replace(McpServersConfig {
            servers: vec![
                disabled_definition("docs", "/bin/docs"),
                disabled_definition("other", "/bin/other"),
            ],
        })
        .await
        .unwrap();
    let saved = saved_records(&store).await;
    for kept in [&newer, &mangled, &invalid] {
        let stored = saved
            .iter()
            .find(|stored| stored.id == kept.id)
            .unwrap_or_else(|| panic!("a save deleted the skipped record {}", kept.name));
        assert_eq!(stored.name, kept.name);
        assert_eq!(stored.definition, kept.definition);
    }

    // Its name stays taken until the record goes.
    let error = runtime
        .replace(McpServersConfig {
            servers: vec![disabled_definition("newer", "/bin/newer")],
        })
        .await
        .expect_err("a skipped record still holds its name")
        .to_string();
    assert!(error.contains("remove it under Connected apps"), "{error}");

    assert!(runtime.remove_skipped(newer.id).await.unwrap());
    assert!(!runtime.remove_skipped(newer.id).await.unwrap());
    assert!(saved_records(&store)
        .await
        .iter()
        .all(|stored| stored.id != newer.id));
    assert!(runtime
        .secrets()
        .get_secret(&env_secret_key(newer.id))
        .await
        .unwrap()
        .is_none());
    assert_eq!(runtime.skipped_servers().await.len(), 2);
    runtime
        .replace(McpServersConfig {
            servers: vec![disabled_definition("newer", "/bin/newer")],
        })
        .await
        .expect("the name is free once the record is removed");
}

/// A secret store that refuses every write, as a locked or denied keychain
/// does.
#[derive(Default)]
struct RefusingSecrets(TestSecrets);

#[async_trait::async_trait]
impl SecretProvider for RefusingSecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        self.0.get_secret(key).await
    }
    async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
        Err(AgentError::msg("the keychain refused the write"))
    }
    async fn delete_secret(&self, key: &str) -> Result<()> {
        self.0.delete_secret(key).await
    }
}

/// A record from before environment values moved into the credential store
/// migrates at boot. When the store refuses the values, that failure used to
/// stop boot. Now the one server is skipped rather than started without its
/// credentials, its record keeps the values for the next boot, and the other
/// servers load.
#[tokio::test]
async fn a_legacy_record_that_cannot_migrate_is_skipped_not_fatal() {
    let directory = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("mcp.db").display()
        ))
        .await
        .unwrap(),
    );
    let runtime = Arc::new(McpRuntime::new(
        Arc::new(ToolRegistry::new()),
        store.clone(),
        Arc::new(RefusingSecrets::default()),
        Arc::new(NoGateway),
        Arc::new(crate::managed_policy::ProvisionedPolicyFile::in_data_dir(
            directory.path(),
        )),
        Arc::new(crate::managed_policy::NoOsPolicy),
    ));
    let now = chrono::Utc::now();
    let legacy = ConnectedApp {
        id: ConnectedAppId::new(),
        name: "legacy".to_string(),
        kind: ConnectedAppKind::McpServer,
        definition: serde_json::json!({
            "name": "legacy",
            "command": "/bin/legacy",
            "env": {"LEGACY_TOKEN": "cleartext-value"},
            "enabled": false,
        }),
        created_at: now,
        updated_at: now,
    };
    let current = ConnectedApp {
        id: ConnectedAppId::new(),
        name: "docs".to_string(),
        kind: ConnectedAppKind::McpServer,
        definition: serde_json::to_value(disabled_definition("docs", "/bin/docs")).unwrap(),
        created_at: now,
        updated_at: now,
    };
    store
        .replace_connected_apps(ConnectedAppKind::McpServer, &[legacy.clone(), current])
        .await
        .unwrap();

    runtime
        .initialize(ConfiguredMcpServers::default())
        .await
        .expect("a refused migration does not fail boot")
        .connect()
        .await;

    let info = runtime.info().await;
    assert_eq!(info.servers.len(), 1);
    assert_eq!(info.servers[0].definition.name, "docs");
    let skipped = runtime.skipped_servers().await;
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].name, "legacy");
    assert!(
        skipped[0].reason.contains("credential store"),
        "{}",
        skipped[0].reason
    );
    let stored = saved_records(&store)
        .await
        .into_iter()
        .find(|stored| stored.id == legacy.id)
        .expect("the record stays on file");
    assert_eq!(
        stored.definition, legacy.definition,
        "the record keeps its values for the next boot"
    );
}

/// One MCP Apps view that never arrives holds a server's tools back for the
/// prefetch bound at most, rather than for the request timeout.
#[tokio::test]
async fn a_view_that_never_arrives_is_left_out_within_the_bound() {
    use axum::routing::post;

    async fn handler(body: String) -> ([(&'static str, &'static str); 1], String) {
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = request.get("id").cloned().unwrap_or_default();
        let result = match request["method"].as_str().unwrap_or_default() {
            "initialize" => serde_json::json!({
                "protocolVersion": tidebreak_mcp::PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "views-fixture", "version": "1"}
            }),
            "tools/list" => serde_json::json!({
                "tools": [
                    {
                        "name": "stuck",
                        "inputSchema": {"type": "object"},
                        "_meta": {"ui": {"resourceUri": "ui://fixture/stuck.html"}}
                    },
                    {
                        "name": "quick",
                        "inputSchema": {"type": "object"},
                        "_meta": {"ui": {"resourceUri": "ui://fixture/quick.html"}}
                    }
                ]
            }),
            "resources/read" if request["params"]["uri"] == "ui://fixture/stuck.html" => {
                std::future::pending::<()>().await;
                unreachable!("a pending future never resolves")
            }
            "resources/read" => serde_json::json!({
                "contents": [{
                    "uri": request["params"]["uri"],
                    "mimeType": "text/html",
                    "text": "<html>quick</html>"
                }]
            }),
            _ => serde_json::json!({}),
        };
        (
            [("content-type", "application/json")],
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        )
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/mcp", post(handler)))
            .await
            .unwrap();
    });
    let client = tidebreak_mcp::McpClient::connect_http_with_timeouts(
        "views",
        &format!("http://{address}/mcp"),
        None,
        Duration::from_secs(10),
        Duration::from_secs(60),
    )
    .await
    .unwrap();

    let started = std::time::Instant::now();
    let views = super::types::prefetch_views(&client, Duration::from_millis(300)).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a stuck view held the prefetch for {:?}",
        started.elapsed()
    );
    assert!(!views.contains_key("ui://fixture/stuck.html"));
}

/// Adding a server connects only it. A configured server that is down fails
/// a save, which connects every server again, but it neither blocks an add
/// nor is reconnected by one. The same endpoint added twice stays one server,
/// a taken name gets a free variant, and managed policy refuses the add.
#[tokio::test]
async fn adding_a_server_connects_only_the_new_one() {
    let (_open, open) = tokio::sync::watch::channel(true);
    let docs = serve_held_http_mcp("lookup", open.clone()).await;
    let more = serve_held_http_mcp("search", open).await;
    let (runtime, store, _directory) = test_runtime().await;
    let mut broken = disabled_definition("broken", "/nonexistent/tidebreak-test-mcp");
    broken.enabled = true;
    let definitions = vec![broken];
    runtime
        .replace_permissive(definitions.clone(), ids_for(&definitions))
        .await;
    let epoch = runtime.state.lock().await.servers["broken"].epoch;

    let McpAddOutcome::Added { name, info } = runtime
        .add_server(
            http_definition("docs", &format!("http://{docs}/mcp")),
            ManualLockdown::Open,
        )
        .await
        .expect("a server that is down elsewhere does not block the add")
    else {
        panic!("an open profile admits the add");
    };
    assert_eq!(name, "docs");
    assert_eq!(listed(&info, "docs").health, McpHealth::Healthy);
    assert_eq!(listed(&info, "broken").health, McpHealth::Degraded);
    assert_eq!(
        runtime.state.lock().await.servers["broken"].epoch,
        epoch,
        "the add did not reconnect the other server"
    );
    assert!(runtime.snapshot().get("mcp__docs__lookup").is_some());
    let saved: Vec<String> = saved_records(&store)
        .await
        .into_iter()
        .map(|record| record.name)
        .collect();
    assert_eq!(saved, ["broken", "docs"]);

    let McpAddOutcome::Added { name, .. } = runtime
        .add_server(
            http_definition("docs_again", &format!("http://{docs}/mcp/")),
            ManualLockdown::Open,
        )
        .await
        .unwrap()
    else {
        panic!("an open profile admits the add");
    };
    assert_eq!(name, "docs", "the endpoint is already configured");
    assert_eq!(saved_records(&store).await.len(), 2);

    let McpAddOutcome::Added { name, .. } = runtime
        .add_server(
            http_definition("docs", &format!("http://{more}/mcp")),
            ManualLockdown::Open,
        )
        .await
        .unwrap()
    else {
        panic!("an open profile admits the add");
    };
    assert_eq!(name, "docs_2");

    let refused = runtime
        .add_server(
            http_definition("locked", "https://mcp.example.test/mcp"),
            ManualLockdown::AllManual,
        )
        .await
        .unwrap();
    assert!(matches!(refused, McpAddOutcome::RefusedManual));
    assert_eq!(saved_records(&store).await.len(), 3);
}

/// A server added from the directory that asks for an OAuth sign-in is saved
/// with "sign in required" and parked, so the existing sign-in flow takes it
/// from there. One whose sign-in Tidebreak can never complete saves nothing.
#[tokio::test]
async fn an_added_server_that_asks_for_a_sign_in_saves_and_signs_in() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    let McpAddOutcome::Added { name, info } = runtime
        .add_server(
            http_definition("vercel", &fake.mcp_url()),
            ManualLockdown::Open,
        )
        .await
        .expect("a server that asks for a sign-in saves")
    else {
        panic!("an open profile admits the add");
    };
    let server = listed(&info, &name);
    assert_eq!(server.health, McpHealth::Degraded);
    assert_eq!(oauth_state(server), Some(McpOAuthState::NotConnected));
    assert_eq!(
        parked(&runtime, &name).await,
        Some(ReconnectPark::Authorization)
    );
    assert_eq!(saved_records(&store).await.len(), 1);

    let status = runtime.oauth_connect(&name).await.unwrap();
    complete_browser_sign_in(&status.pending_authorization_url.unwrap()).await;
    info_when(&runtime, |server| server.health == McpHealth::Healthy).await;

    let unsupported = FakeOAuthServer::serve(FakeOAuthOptions {
        registration: false,
        ..FakeOAuthOptions::default()
    })
    .await;
    let error = match runtime
        .add_server(
            http_definition("legacy", &unsupported.mcp_url()),
            ManualLockdown::Open,
        )
        .await
    {
        Ok(_) => panic!("a sign-in Tidebreak cannot complete refuses the add"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("dynamic client registration"), "{error}");
    assert_eq!(saved_records(&store).await.len(), 1);
}

/// Each directory entry saves a definition that passes the same validation a
/// settings save applies, so an add never fails on the entry itself.
#[test]
fn every_directory_server_saves_a_valid_definition() {
    let definitions: Vec<McpServerDefinition> = crate::mcp_directory::directory()
        .servers
        .iter()
        .map(|entry| entry.definition())
        .collect();
    assert!(!definitions.is_empty());
    validate_servers(&definitions).unwrap();
}
