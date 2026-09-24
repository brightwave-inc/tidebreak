use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tidebreak_core::connected_app::{ConnectedApp, ConnectedAppKind};
use tidebreak_core::id::ConnectedAppId;
use tidebreak_core::{AgentError, Result, SecretProvider, Store, ToolRegistry};

use super::*;

use super::stdio::CommandApproval;
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
/// [`TestSecrets`] with one armed write: deleting `trigger` first writes
/// `value` under `target`, the way a live connection that refreshes its
/// token in the middle of a replacement does.
#[derive(Default)]
struct HookedSecrets {
    inner: TestSecrets,
    hook: std::sync::Mutex<Option<(String, String, String)>>,
}

impl HookedSecrets {
    fn arm(&self, trigger: String, target: String, value: String) {
        *self.hook.lock().unwrap() = Some((trigger, target, value));
    }
}

#[async_trait::async_trait]
impl SecretProvider for HookedSecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        self.inner.get_secret(key).await
    }
    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        self.inner.set_secret(key, value).await
    }
    async fn delete_secret(&self, key: &str) -> Result<()> {
        let fired = {
            let mut hook = self.hook.lock().unwrap();
            match hook.as_ref() {
                Some((trigger, _, _)) if trigger == key => hook.take(),
                _ => None,
            }
        };
        if let Some((_, target, value)) = fired {
            self.inner.set_secret(&target, &value).await?;
        }
        self.inner.delete_secret(key).await
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
        approved_executable: None,
        url: None,
        bearer_token_env: None,
        bearer_token_stored: false,
        bearer_token_value: None,
        headers: Default::default(),
        header_values: Default::default(),
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
        approved_executable: None,
        url: Some(url.to_string()),
        bearer_token_env: None,
        bearer_token_stored: false,
        bearer_token_value: None,
        headers: Default::default(),
        header_values: Default::default(),
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
        approved_executable: None,
        url: None,
        bearer_token_env: None,
        bearer_token_stored: false,
        bearer_token_value: None,
        headers: Default::default(),
        header_values: Default::default(),
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
    test_runtime_with_secrets(gateway, os_policy, Arc::new(TestSecrets::default())).await
}

async fn test_runtime_with_secrets(
    gateway: Arc<dyn GatewayEndpoints>,
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    secrets: Arc<dyn SecretProvider>,
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
            secrets,
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
    let _path = super::stdio::HostPathGuard::set(Some("/opt/tools/bin:/usr/bin".into())).await;
    let command = server
        .build_command(&BTreeMap::new(), CommandApproval::Optional)
        .await
        .unwrap();
    // Nothing ambient beyond the two names every child is given.
    let mut names: Vec<_> = command
        .as_std()
        .get_envs()
        .map(|(name, _)| name.to_os_string())
        .collect();
    names.sort();
    let expected: Vec<std::ffi::OsString> = if std::env::var_os("HOME").is_some() {
        vec!["HOME".into(), "PATH".into()]
    } else {
        vec!["PATH".into()]
    };
    assert_eq!(names, expected);
}

/// SET-02: a child started with no PATH could not find the interpreter a
/// script names, so even an absolute path to `npx` failed to find `node`,
/// and with no HOME it had no cache. Every user-configured stdio child gets
/// the host search PATH its command was resolved on, and HOME.
#[tokio::test]
async fn a_stdio_child_is_given_the_host_search_path_and_home() {
    let config = parse(r#"{"servers":[{"name":"docs","command":"/bin/docs"}]}"#).unwrap();
    let search_path = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";
    let _path = super::stdio::HostPathGuard::set(Some(search_path.into())).await;
    let command = config.0[0]
        .build_command(&BTreeMap::new(), CommandApproval::Optional)
        .await
        .unwrap();
    let env: BTreeMap<String, String> = command
        .as_std()
        .get_envs()
        .filter_map(|(name, value)| {
            Some((
                name.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    assert_eq!(env.get("PATH").map(String::as_str), Some(search_path));
    assert_eq!(
        env.get("HOME").map(String::as_str),
        std::env::var("HOME").ok().as_deref()
    );

    // They reach the process, not only the builder: a script that prints
    // them sees both.
    let script = parse(
        r#"{"servers":[{"name":"echo","command":"/bin/sh","args":["-c","printf '%s|%s' \"$PATH\" \"$HOME\""]}]}"#,
    )
    .unwrap();
    let output = script.0[0]
        .build_command(&BTreeMap::new(), CommandApproval::Optional)
        .await
        .unwrap()
        .output()
        .await
        .unwrap();
    let printed = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        printed,
        format!(
            "{search_path}|{}",
            std::env::var("HOME").unwrap_or_default()
        )
    );
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
    let _path = super::stdio::HostPathGuard::set(Some("/opt/seeded/bin".into())).await;
    let command = config.0[0]
        .build_command(&BTreeMap::new(), CommandApproval::Optional)
        .await
        .unwrap();
    let forwarded_path = command
        .as_std()
        .get_envs()
        .filter(|(name, _)| *name == "PATH")
        .last()
        .and_then(|(_, value)| value)
        .expect("PATH is selected for forwarding");
    // A name the definition forwards itself replaces the default.
    assert_eq!(Some(forwarded_path), std::env::var_os("PATH").as_deref());
    assert!(command
        .as_std()
        .get_envs()
        .all(|(name, _)| name == "PATH" || name == "HOME"));
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
        .connect(
            &gateway,
            &BTreeMap::new(),
            &Default::default(),
            None,
            CommandApproval::Optional,
        )
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
        .connect(
            &gateway,
            &BTreeMap::new(),
            &Default::default(),
            None,
            CommandApproval::Optional,
        )
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

/// A replacement whose server fails to start changes no stored credential:
/// not the environment values it would rewrite or drop, and not the sign-in
/// it would clear. The import route says nothing changed on this promise.
#[tokio::test]
async fn a_replacement_that_fails_to_start_leaves_stored_credentials_alone() {
    let (runtime, store, _directory) = test_runtime().await;
    let mut docs = disabled_definition("docs", "/bin/docs");
    docs.env.insert("DOCS_TOKEN".to_string());
    docs.env_values
        .insert("DOCS_TOKEN".to_string(), "first".to_string());
    runtime
        .replace(McpServersConfig {
            servers: vec![docs.clone()],
        })
        .await
        .unwrap();
    let id = saved_records(&store).await[0].id;
    let secrets = runtime.secrets();
    let client_key = crate::connectors::oauth_client_secret_key(id);
    let token_key = crate::connectors::oauth_token_secret_key(id);
    secrets
        .set_secret(&client_key, "registration-placeholder")
        .await
        .unwrap();
    secrets
        .set_secret(&token_key, "session-placeholder")
        .await
        .unwrap();

    let mut rewritten = docs.clone();
    rewritten
        .env_values
        .insert("DOCS_TOKEN".to_string(), "second".to_string());
    let dead = http_definition("dead", "http://127.0.0.1:1/mcp");
    // One candidate rewrites the stored value; the other drops the server.
    for servers in [vec![rewritten, dead.clone()], vec![dead]] {
        let error = runtime
            .replace(McpServersConfig { servers })
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("failed to start"), "{error}");
        assert!(!error.to_string().contains(CREDENTIALS_NOT_RESTORED));
        assert_eq!(
            runtime
                .stored_env(id)
                .await
                .get("DOCS_TOKEN")
                .map(String::as_str),
            Some("first")
        );
        assert_eq!(
            secrets.get_secret(&client_key).await.unwrap().as_deref(),
            Some("registration-placeholder")
        );
        assert_eq!(
            secrets.get_secret(&token_key).await.unwrap().as_deref(),
            Some("session-placeholder")
        );
    }
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
    let _guard =
        super::stdio::HostPathGuard::set(Some(directory.path().as_os_str().to_os_string())).await;
    let resolved = super::stdio::resolve_stdio_command("npx").await.unwrap();
    assert_eq!(resolved, npx);

    let definition = parse(r#"{"servers":[{"name":"docs","command":"npx"}]}"#)
        .unwrap()
        .0
        .remove(0);
    assert_eq!(definition.command.as_deref(), Some("npx"));
    let command = definition
        .build_command(&BTreeMap::new(), CommandApproval::Optional)
        .await
        .unwrap();
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
    /// Whether a refresh issues a new refresh token, spending the old one.
    rotate_refresh: std::sync::atomic::AtomicBool,
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
            rotate_refresh: Default::default(),
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
            if server
                .rotate_refresh
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                return axum::Json(serde_json::json!({
                    "access_token": server.access_token,
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "refresh_token": "fake-refresh-token-rotated",
                    "scope": "projects:read"
                }))
                .into_response();
            }
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

/// Review finding: a failed replacement put back the session its own
/// connection had just refreshed. Under refresh-token rotation the token it
/// put back was already spent, which signed the person out while the import
/// said nothing changed. A key that holds something newer than the
/// replacement's own write now stays.
#[tokio::test]
async fn a_failed_replacement_keeps_the_session_its_connection_refreshed() {
    let fake = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    sign_in_to(&runtime, "vercel", &fake).await;
    let id = saved_records(&store).await[0].id;
    let vault = crate::connectors::McpOAuthCredentialVault::new(runtime.secrets(), id);
    // Age the access token so the replacement's connection must refresh it.
    let mut credentials = vault.load().await.unwrap().unwrap();
    credentials.expires_at_unix = 1;
    vault.save(&credentials).await.unwrap();
    fake.rotate_refresh
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let error = runtime
        .replace(McpServersConfig {
            servers: vec![
                http_definition("vercel", &fake.mcp_url()),
                http_definition("dead", "http://127.0.0.1:1/mcp"),
            ],
        })
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("failed to start"), "{error}");
    assert_eq!(fake.refresh_resources.lock().unwrap().len(), 1);
    assert_eq!(
        vault
            .load()
            .await
            .unwrap()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("fake-refresh-token-rotated")
    );
}

/// Review finding: a live connection refreshed its token while a
/// replacement was reconciling sign-ins, before the replacement connected
/// anything. The replacement read what it had written only after that, so it
/// took the rotated token for its own write, and when it failed it put the
/// spent token back. It now records each write from its own values as it
/// makes it, and a key it never wrote stays as the live connection left it.
#[tokio::test]
async fn a_token_a_live_connection_rotates_during_reconcile_survives_a_failed_replacement() {
    let fake = FakeOAuthServer::approving().await;
    let secrets = Arc::new(HookedSecrets::default());
    let (runtime, store, _directory) = test_runtime_with_secrets(
        Arc::new(NoGateway),
        Arc::new(crate::managed_policy::NoOsPolicy),
        secrets.clone(),
    )
    .await;
    runtime.admit_loopback_oauth_for_tests();
    sign_in_to(&runtime, "vercel", &fake).await;
    // A server the replacement below drops, so reconcile clears its sign-in.
    runtime
        .replace(McpServersConfig {
            servers: vec![
                http_definition("vercel", &fake.mcp_url()),
                disabled_definition("gone", "/bin/gone"),
            ],
        })
        .await
        .unwrap();
    let records = saved_records(&store).await;
    let id_of = |name: &str| {
        records
            .iter()
            .find(|record| record.name == name)
            .map(|record| record.id)
            .unwrap()
    };
    let (vercel, gone) = (id_of("vercel"), id_of("gone"));
    let rotated = serde_json::to_string(&crate::connectors::McpOAuthCredentials {
        access_token: fake.access_token.clone(),
        refresh_token: Some("rotated-by-a-live-connection".to_string()),
        expires_at_unix: u64::MAX / 2,
        scope: None,
    })
    .unwrap();
    // The rotation lands while reconcile clears the dropped server's
    // sign-in: after the replacement wrote the environment values, before
    // it connects anything.
    secrets.arm(
        crate::connectors::oauth_token_secret_key(gone),
        crate::connectors::oauth_token_secret_key(vercel),
        rotated,
    );

    let error = runtime
        .replace(McpServersConfig {
            servers: vec![
                http_definition("vercel", &fake.mcp_url()),
                http_definition("dead", "http://127.0.0.1:1/mcp"),
            ],
        })
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("failed to start"), "{error}");
    assert!(secrets.hook.lock().unwrap().is_none(), "the rotation ran");
    let vault = crate::connectors::McpOAuthCredentialVault::new(runtime.secrets(), vercel);
    assert_eq!(
        vault
            .load()
            .await
            .unwrap()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("rotated-by-a-live-connection")
    );
}

/// A failed replacement that cannot put a credential back says which
/// server's, so the import's result says exactly what changed.
#[tokio::test]
async fn a_credential_that_cannot_be_put_back_names_its_server() {
    let (runtime, store, _directory) = test_runtime().await;
    let mut docs = disabled_definition("docs", "/bin/docs");
    docs.env.insert("DOCS_TOKEN".to_string());
    docs.env_values
        .insert("DOCS_TOKEN".to_string(), "first".to_string());
    runtime
        .replace(McpServersConfig {
            servers: vec![docs.clone()],
        })
        .await
        .unwrap();
    let id = saved_records(&store).await[0].id;
    let mut journal = CredentialJournal::default();
    // The replacement wrote "second", and something else changed it since:
    // nothing to put back, and nothing to report.
    journal.record(
        "docs",
        &env_secret_key(id),
        Some("{\"DOCS_TOKEN\":\"first\"}".to_string()),
        Some("{\"DOCS_TOKEN\":\"second\"}".to_string()),
    );
    let kept = runtime
        .restore_credentials(
            journal,
            AgentError::config("external MCP server x failed to start"),
        )
        .await;
    assert!(
        !kept.to_string().contains(CREDENTIALS_NOT_RESTORED),
        "{kept}"
    );

    let failing = Arc::new(FailingWrites);
    let (runtime, _store, _directory) = test_runtime_with_secrets(
        Arc::new(NoGateway),
        Arc::new(crate::managed_policy::NoOsPolicy),
        failing.clone(),
    )
    .await;
    let mut journal = CredentialJournal::default();
    journal.record("docs", "mcp.docs.env_v1", Some("before".to_string()), None);
    let error = runtime
        .restore_credentials(
            journal,
            AgentError::config("external MCP server x failed to start"),
        )
        .await
        .to_string();
    assert!(error.contains(CREDENTIALS_NOT_RESTORED), "{error}");
    assert!(error.ends_with("of docs."), "{error}");
    assert!(
        !error.contains("configuration error: configuration error"),
        "{error}"
    );
}

/// A secret store that reads nothing and refuses every write.
#[derive(Default)]
struct FailingWrites;

#[async_trait::async_trait]
impl SecretProvider for FailingWrites {
    async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
        Ok(None)
    }
    async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
        Err(AgentError::Secret(
            "the keychain refused the write".to_string(),
        ))
    }
    async fn delete_secret(&self, _key: &str) -> Result<()> {
        Err(AgentError::Secret(
            "the keychain refused the delete".to_string(),
        ))
    }
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

/// A server configured with a static bearer from a variable keeps that path
/// until the person switches it. When the server refuses the token with a
/// `401` that names OAuth metadata, the save keeps the server and offers Use
/// OAuth, but not Connect: while the server carries a bearer it does not sign
/// in.
#[tokio::test]
async fn a_static_bearer_server_offers_use_oauth_but_not_connect() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    let (runtime, _store, _directory) = oauth_test_runtime().await;
    let mut definition = http_definition("vercel", &fake.mcp_url());
    // PATH always exists and is never the fake's token.
    definition.bearer_token_env = Some("PATH".to_string());
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .expect("a refused token that names OAuth metadata saves");
    let server = &info.servers[0];
    assert_eq!(server.health, McpHealth::Degraded);
    assert_eq!(oauth_state(server), Some(McpOAuthState::Available));
    let listed = runtime.info().await;
    assert_eq!(
        oauth_state(&listed.servers[0]),
        Some(McpOAuthState::Available)
    );
    let connect = runtime.oauth_connect("vercel").await.unwrap();
    assert_eq!(connect.state, McpOAuthState::Unsupported);
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

/// Review finding: a stdio server outlived the server stop, so Delete all
/// data could remove a folder that a server, or a helper it started, then
/// wrote back into. The stop now kills every stdio server with each process
/// it started.
#[tokio::test]
async fn killing_the_stdio_servers_stops_every_process_they_started() {
    let (runtime, _store, directory) = test_runtime().await;
    let log = directory.path().join("writes.log");
    // Answers initialize (id 1) and tools/list (id 2) in the order the
    // client sends them, after starting a helper that keeps writing.
    let script = format!(
        r#"( while true; do echo tick >> '{log}'; /bin/sleep 0.02; done ) &
read _initialize
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{version}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"writer","version":"1"}}}}}}'
read _initialized
read _list
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[]}}}}'
while read _line; do :; done
"#,
        log = log.display(),
        version = tidebreak_mcp::PROTOCOL_VERSION,
    );
    let mut definition = disabled_definition("writer", "/bin/sh");
    definition.args = vec!["-c".to_string(), script];
    definition.enabled = true;
    runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(&log).map_or(0, |metadata| metadata.len()) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the helper never wrote"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    runtime.kill_stdio_servers().await;
    // Give a stray writer time to show itself.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let settled = std::fs::read(&log).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        settled.len(),
        std::fs::read(&log).unwrap().len(),
        "something wrote after the kill"
    );
}

/// Review finding: a gateway endpoint auto-mount committed every running
/// definition, plugin-sourced ones included, so it saved a plugin's server
/// as an ordinary record that outlived the plugin. The mount now saves only
/// configured servers and leaves the plugin's server derived.
#[tokio::test]
async fn an_auto_mount_never_saves_a_plugin_server() {
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
    assert!(runtime.reconcile_plugin_servers().await);
    let plugin_name = runtime.info().await.servers[0].definition.name.clone();

    assert!(runtime
        .auto_mount_gateway_endpoints(&["tools".to_owned()])
        .await
        .unwrap());

    let saved = saved_records(&store).await;
    assert_eq!(
        saved
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>(),
        ["tools"],
        "only the gateway mount is saved"
    );
    let info = runtime.info().await;
    let running: Vec<(&str, bool)> = info
        .servers
        .iter()
        .map(|server| {
            (
                server.definition.name.as_str(),
                server.definition.plugin.is_some(),
            )
        })
        .collect();
    assert!(
        running.contains(&(plugin_name.as_str(), true)),
        "{running:?}"
    );
    assert!(running.contains(&("tools", false)), "{running:?}");
}

// ---------------------------------------------------------------------------
// Stored bearer tokens and custom headers for remote servers
// ---------------------------------------------------------------------------

/// A fixture value built at run time, so no credential-shaped literal sits
/// in the source.
fn fixture_value(label: &str) -> String {
    [label, "fixture", &uuid::Uuid::new_v4().simple().to_string()].join("-")
}

/// What one request to [`serve_credentialed_mcp`] carried.
#[derive(Clone, Debug, Default)]
struct SeenRequest {
    authorization: Option<String>,
    api_key: Option<String>,
}

type SeenRequests = Arc<std::sync::Mutex<Vec<SeenRequest>>>;

/// A loopback MCP server that records the `Authorization` and `X-Api-Key`
/// headers of every request and answers like a well-behaved server.
async fn serve_credentialed_mcp() -> (std::net::SocketAddr, SeenRequests) {
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::post;

    let seen: SeenRequests = Arc::default();
    let recorded = Arc::clone(&seen);
    let app = axum::Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, body: String| {
            let recorded = Arc::clone(&recorded);
            async move {
                let header = |name: &str| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string)
                };
                recorded.lock().unwrap().push(SeenRequest {
                    authorization: header("authorization"),
                    api_key: header("x-api-key"),
                });
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let Some(id) = request.get("id").cloned() else {
                    return axum::http::StatusCode::ACCEPTED.into_response();
                };
                let result = match request["method"].as_str().unwrap_or_default() {
                    "initialize" => serde_json::json!({
                        "protocolVersion": tidebreak_mcp::PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "credentialed-fixture", "version": "1"}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [{
                            "name": "lookup",
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
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, seen)
}

/// A remote server whose bearer token and one custom header live in the
/// credential store.
fn stored_credential_definition(
    name: &str,
    url: &str,
    bearer: Option<&str>,
    api_key: Option<&str>,
) -> McpServerDefinition {
    let mut definition = http_definition(name, url);
    definition.bearer_token_stored = true;
    definition.bearer_token_value = bearer.map(str::to_string);
    definition.headers.insert("X-Api-Key".to_string());
    if let Some(api_key) = api_key {
        definition
            .header_values
            .insert("X-Api-Key".to_string(), api_key.to_string());
    }
    definition
}

/// SET-04: a bearer token and a header value typed into Settings land in the
/// credential store, reach the server on every request, and appear nowhere
/// else: not in the saved record, not in any projection, not in a save's
/// answer. A later save that leaves them blank keeps them, and removing the
/// server deletes them.
#[tokio::test]
async fn stored_bearer_and_header_reach_the_server_and_nothing_else() {
    let (address, seen) = serve_credentialed_mcp().await;
    let (runtime, store, _directory) = test_runtime().await;
    let bearer = fixture_value("bearer");
    let api_key = fixture_value("header");
    let url = format!("http://{address}/mcp");
    let definition = stored_credential_definition("docs", &url, Some(&bearer), Some(&api_key));

    let info = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();
    assert_eq!(info.servers[0].health, McpHealth::Healthy, "{info:?}");
    let requests = seen.lock().unwrap().clone();
    assert!(!requests.is_empty());
    let expected_authorization = format!("Bearer {bearer}");
    for request in &requests {
        assert_eq!(
            request.authorization.as_deref(),
            Some(expected_authorization.as_str())
        );
        assert_eq!(request.api_key.as_deref(), Some(api_key.as_str()));
    }

    // Names only, everywhere a definition leaves this process.
    let answer = serde_json::to_string(&info).unwrap();
    let listing = serde_json::to_string(&runtime.info().await).unwrap();
    let record = saved_records(&store)
        .await
        .into_iter()
        .map(|record| record.definition.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    for surface in [&answer, &listing, &record] {
        assert!(!surface.contains(&bearer), "{surface}");
        assert!(!surface.contains(&api_key), "{surface}");
    }
    assert!(
        listing.contains("\"bearer_token_stored\":true"),
        "{listing}"
    );
    assert!(listing.contains("X-Api-Key"), "{listing}");
    let debug = format!("{:?}", runtime.definitions().await);
    assert!(
        !debug.contains(&bearer) && !debug.contains(&api_key),
        "{debug}"
    );

    // The values sit under the record's own key, bound to its origin.
    let id = saved_records(&store).await[0].id;
    let stored = runtime.stored_http(id).await;
    assert_eq!(stored.bearer.as_deref(), Some(bearer.as_str()));
    assert_eq!(stored.headers["X-Api-Key"], api_key);
    assert_eq!(stored.origin, super::types::http_origin(&url));
    assert!(!format!("{stored:?}").contains(&bearer));

    // Saving again with the values blank keeps what is stored.
    seen.lock().unwrap().clear();
    runtime
        .replace(McpServersConfig {
            servers: vec![stored_credential_definition("docs", &url, None, None)],
        })
        .await
        .unwrap();
    let requests = seen.lock().unwrap().clone();
    assert!(!requests.is_empty());
    assert!(requests
        .iter()
        .all(|request| request.api_key.as_deref() == Some(api_key.as_str())));
    assert_eq!(
        runtime.stored_http(id).await.bearer.as_deref(),
        Some(bearer.as_str())
    );

    // Removing the server takes its stored values with it.
    runtime
        .replace(McpServersConfig { servers: vec![] })
        .await
        .unwrap();
    assert_eq!(
        runtime
            .secrets()
            .get_secret(&http_secret_key(id))
            .await
            .unwrap(),
        None
    );
}

/// A stored value goes only to the origin it was entered for. Pointing the
/// server somewhere else without entering the values again drops them, and
/// the verify says what is missing instead of sending the old token.
#[tokio::test]
async fn moving_a_server_to_another_origin_never_carries_its_stored_values() {
    let (first, _) = serve_credentialed_mcp().await;
    let (second, seen_second) = serve_credentialed_mcp().await;
    let (runtime, store, _directory) = test_runtime().await;
    let bearer = fixture_value("bearer");
    let api_key = fixture_value("header");
    runtime
        .replace(McpServersConfig {
            servers: vec![stored_credential_definition(
                "docs",
                &format!("http://{first}/mcp"),
                Some(&bearer),
                Some(&api_key),
            )],
        })
        .await
        .unwrap();
    let id = saved_records(&store).await[0].id;

    let error = runtime
        .replace(McpServersConfig {
            servers: vec![stored_credential_definition(
                "docs",
                &format!("http://{second}/mcp"),
                None,
                None,
            )],
        })
        .await
        .expect_err("the moved server has no stored values for its new origin");
    let message = error.to_string();
    assert!(message.contains("Not stored:"), "{message}");
    assert!(!message.contains(&bearer), "{message}");
    assert!(
        seen_second.lock().unwrap().is_empty(),
        "nothing reached the new origin"
    );
    // The failed save put back what it changed.
    assert_eq!(
        runtime.stored_http(id).await.bearer.as_deref(),
        Some(bearer.as_str())
    );
}

/// A failed save rolls back the stored bearer and header values it wrote,
/// the way it rolls back environment values and sign-ins.
#[tokio::test]
async fn a_failed_save_puts_back_stored_http_values() {
    let (address, _) = serve_credentialed_mcp().await;
    let (runtime, store, _directory) = test_runtime().await;
    let url = format!("http://{address}/mcp");
    let first = fixture_value("first");
    runtime
        .replace(McpServersConfig {
            servers: vec![stored_credential_definition(
                "docs",
                &url,
                Some(&first),
                Some(&first),
            )],
        })
        .await
        .unwrap();
    let id = saved_records(&store).await[0].id;

    let second = fixture_value("second");
    let dead = http_definition("dead", "http://127.0.0.1:1/mcp");
    let error = runtime
        .replace(McpServersConfig {
            servers: vec![
                stored_credential_definition("docs", &url, Some(&second), Some(&second)),
                dead,
            ],
        })
        .await
        .expect_err("the dead server fails the save");
    assert!(error.to_string().contains("failed to start"), "{error}");
    assert!(!error.to_string().contains(&second), "{error}");
    let stored = runtime.stored_http(id).await;
    assert_eq!(stored.bearer.as_deref(), Some(first.as_str()));
    assert_eq!(stored.headers["X-Api-Key"], first);
}

/// Settings says a stored value is set only when the credential store holds
/// it for the server's origin, as it does not after an import on a new
/// computer, and the listing never carries the value itself.
#[tokio::test]
async fn the_listing_says_which_stored_values_are_set() {
    let (runtime, _store, _directory) = test_runtime().await;
    let url = "https://mcp.example.test/mcp";
    let mut imported = stored_credential_definition("docs", url, None, None);
    imported.enabled = false;
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![imported],
        })
        .await
        .unwrap();
    assert_eq!(
        info.servers[0].stored_credentials,
        Some(McpStoredCredentials {
            bearer: false,
            headers: Vec::new()
        })
    );

    let bearer = fixture_value("bearer");
    let api_key = fixture_value("header");
    let mut entered = stored_credential_definition("docs", url, Some(&bearer), Some(&api_key));
    entered.enabled = false;
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![entered],
        })
        .await
        .unwrap();
    assert_eq!(
        info.servers[0].stored_credentials,
        Some(McpStoredCredentials {
            bearer: true,
            headers: vec!["X-Api-Key".to_string()]
        })
    );
    let listing = serde_json::to_string(&runtime.info().await).unwrap();
    assert!(
        listing.contains(r#""stored_credentials":{"bearer":true,"headers":["X-Api-Key"]}"#),
        "{listing}"
    );
    assert!(!listing.contains(&bearer) && !listing.contains(&api_key));
}

/// SET-04: custom headers are a small, bounded set. A name the connection
/// owns, a hop-by-hop or framing field, an Authorization header, or one past
/// the cap is refused before anything is saved, naming the header, never a
/// value.
#[test]
fn disallowed_and_excess_headers_are_refused() {
    for name in [
        "Host",
        "Content-Length",
        "Transfer-Encoding",
        "Connection",
        "Keep-Alive",
        "Upgrade",
        "Mcp-Session-Id",
        "Content-Type",
    ] {
        let mut definition = http_definition("docs", "https://mcp.example.test/mcp");
        definition.headers.insert(name.to_string());
        let error = validate_servers(&[definition]).expect_err(name).to_string();
        assert!(error.contains("not allowed"), "{name}: {error}");
        assert!(error.contains(name), "{name}: {error}");
    }

    let mut authorization = http_definition("docs", "https://mcp.example.test/mcp");
    authorization.headers.insert("Authorization".to_string());
    let error = validate_servers(&[authorization]).unwrap_err().to_string();
    assert!(
        error.contains("bearer token under authentication"),
        "{error}"
    );

    let mut malformed = http_definition("docs", "https://mcp.example.test/mcp");
    malformed.headers.insert("X Api Key".to_string());
    assert!(validate_servers(&[malformed]).is_err());

    let mut too_many = http_definition("docs", "https://mcp.example.test/mcp");
    for index in 0..=MAX_HEADERS {
        too_many.headers.insert(format!("X-Custom-{index}"));
    }
    let error = validate_servers(&[too_many]).unwrap_err().to_string();
    assert!(error.contains("more than 8 custom headers"), "{error}");

    let mut duplicate = http_definition("docs", "https://mcp.example.test/mcp");
    duplicate.headers.insert("X-Api-Key".to_string());
    duplicate.headers.insert("x-api-key".to_string());
    assert!(validate_servers(&[duplicate])
        .unwrap_err()
        .to_string()
        .contains("more than once"));

    // A value that could smuggle header syntax never gets that far, and the
    // error does not echo it.
    let secret = fixture_value("value");
    let mut injected = http_definition("docs", "https://mcp.example.test/mcp");
    injected.headers.insert("X-Api-Key".to_string());
    injected
        .header_values
        .insert("X-Api-Key".to_string(), format!("{secret}\r\nHost: evil"));
    let error = validate_servers(&[injected]).unwrap_err().to_string();
    assert!(!error.contains(&secret), "{error}");

    // Headers, like a stored bearer, belong to url servers.
    let mut stdio = disabled_definition("docs", "/bin/docs");
    stdio.headers.insert("X-Api-Key".to_string());
    assert!(validate_servers(&[stdio]).is_err());
    let mut stdio = disabled_definition("docs", "/bin/docs");
    stdio.bearer_token_stored = true;
    assert!(validate_servers(&[stdio]).is_err());
}

/// A stored credential rides the same https rule as a bearer variable: it
/// may travel in cleartext only to a literal loopback address. And a server
/// authenticates one way at a time.
#[test]
fn stored_credentials_need_https_and_one_way_to_authenticate() {
    let mut cleartext = http_definition("docs", "http://remote.example/mcp");
    cleartext.bearer_token_stored = true;
    assert!(validate_servers(&[cleartext])
        .unwrap_err()
        .to_string()
        .contains("must use https"));
    let mut header = http_definition("docs", "http://remote.example/mcp");
    header.headers.insert("X-Api-Key".to_string());
    assert!(validate_servers(&[header]).is_err());
    let mut loopback = http_definition("docs", "http://127.0.0.1:9000/mcp");
    loopback.bearer_token_stored = true;
    loopback.headers.insert("X-Api-Key".to_string());
    assert!(validate_servers(&[loopback]).is_ok());

    let mut both = http_definition("docs", "https://mcp.example.test/mcp");
    both.bearer_token_stored = true;
    both.bearer_token_env = Some("MCP_TOKEN".to_string());
    assert!(validate_servers(&[both]).is_err());
    let mut oauth = http_definition("docs", "https://mcp.example.test/mcp");
    oauth.bearer_token_stored = true;
    oauth.oauth = true;
    assert!(validate_servers(&[oauth]).is_err());

    let mut prefixed = http_definition("docs", "https://mcp.example.test/mcp");
    prefixed.bearer_token_stored = true;
    prefixed.bearer_token_value = Some(format!("Bearer {}", fixture_value("token")));
    assert!(validate_servers(&[prefixed])
        .unwrap_err()
        .to_string()
        .contains("without the \"Bearer\" prefix"));

    // A value only travels with the setting that stores it.
    let mut orphan = http_definition("docs", "https://mcp.example.test/mcp");
    orphan.bearer_token_value = Some(fixture_value("token"));
    assert!(validate_servers(&[orphan]).is_err());
}

/// SET-03: a server configured with a static bearer that refuses it with a
/// `401` naming protected-resource metadata is saved instead of failing the
/// save, and its row offers Use OAuth with the host the sign-in opens.
/// Switching it to OAuth saves it waiting for a sign-in, and drops the
/// stored bearer the server refused.
#[tokio::test]
async fn a_refused_bearer_with_oauth_metadata_saves_and_offers_oauth() {
    use crate::mcp_oauth_runtime::McpOAuthState;

    let fake = FakeOAuthServer::approving().await;
    let (runtime, store, _directory) = oauth_test_runtime().await;
    let refused = fixture_value("refused");
    let mut definition = http_definition("vercel", &fake.mcp_url());
    definition.bearer_token_stored = true;
    definition.bearer_token_value = Some(refused.clone());

    let info = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .expect("a refused bearer that names OAuth metadata still saves");
    let server = &info.servers[0];
    assert_eq!(server.health, McpHealth::Degraded);
    let status = server
        .oauth_status
        .as_ref()
        .expect("the offer is projected");
    assert_eq!(status.state, McpOAuthState::Available);
    assert_eq!(status.sign_in_host.as_deref(), Some("127.0.0.1"));
    let diagnostic = server.diagnostic.as_deref().unwrap();
    assert!(diagnostic.contains("Use OAuth"), "{diagnostic}");
    assert!(!diagnostic.contains(&refused), "{diagnostic}");
    // The bearer reached the server; the refusal is what taught the offer.
    assert!(fake.mcp_bearers().contains(&refused));
    // Retrying the refused bearer cannot help until the settings change.
    assert_eq!(
        parked(&runtime, "vercel").await,
        Some(ReconnectPark::Configuration)
    );
    // Connect is for a server that signs in, which this one does not yet.
    let connect = runtime.oauth_connect("vercel").await.unwrap();
    assert_eq!(connect.state, McpOAuthState::Unsupported);
    let id = saved_records(&store).await[0].id;
    assert!(runtime.stored_http(id).await.bearer.is_some());

    // Use OAuth: the same server, switched to sign in.
    let mut switched = http_definition("vercel", &fake.mcp_url());
    switched.oauth = true;
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![switched],
        })
        .await
        .expect("an OAuth server waiting for a sign-in saves");
    assert_eq!(
        oauth_state(&info.servers[0]),
        Some(McpOAuthState::NotConnected)
    );
    assert_eq!(
        runtime
            .secrets()
            .get_secret(&http_secret_key(id))
            .await
            .unwrap(),
        None,
        "the refused bearer is gone"
    );
    let status = runtime.oauth_connect("vercel").await.unwrap();
    assert_eq!(status.state, McpOAuthState::Authorizing);
}

/// A `401` that names no OAuth metadata is what it always was for a server
/// with a bearer: the token is wrong, and the save fails saying so.
#[tokio::test]
async fn a_refused_bearer_without_oauth_metadata_still_fails_the_save() {
    let address =
        serve_http_response(axum::http::StatusCode::UNAUTHORIZED, "text/plain", b"").await;
    let (runtime, _store, _directory) = oauth_test_runtime().await;
    let mut definition = http_definition("docs", &format!("http://{address}/mcp"));
    definition.bearer_token_stored = true;
    definition.bearer_token_value = Some(fixture_value("token"));
    let error = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .expect_err("a plain 401 fails the save");
    assert!(
        error
            .to_string()
            .contains("Authentication failed (401 Unauthorized)"),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Security review of bare commands and stored values (#3573)
// ---------------------------------------------------------------------------

/// Write an executable `npx` into `directory` that prints `label`, and
/// return its path.
#[cfg(unix)]
fn write_printing_npx(directory: &Path, label: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let npx = directory.join("npx");
    std::fs::write(&npx, format!("#!/bin/sh\nprintf '%s' {label}\n")).unwrap();
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).unwrap();
    npx
}

/// Write an executable `npx` into `directory` that notes `label` in `ran`
/// each time it starts, then answers as an MCP server with no tools.
#[cfg(unix)]
fn write_mcp_npx(directory: &Path, label: &str, ran: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let npx = directory.join("npx");
    let script = format!(
        r#"#!/bin/sh
printf '%s\n' '{label}' >> '{ran}'
read _initialize
printf '%s\n' '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"{version}","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"{label}","version":"1"}}}}}}'
read _initialized
read _list
printf '%s\n' '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[]}}}}'
while read _line; do :; done
"#,
        ran = ran.display(),
        version = tidebreak_mcp::PROTOCOL_VERSION,
    );
    std::fs::write(&npx, script).unwrap();
    std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).unwrap();
    npx
}

/// What a definition's child prints, started the way a spawn starts it.
async fn spawned_output(definition: &McpServerDefinition, approval: CommandApproval) -> String {
    let output = definition
        .build_command(&BTreeMap::new(), approval)
        .await
        .unwrap()
        .output()
        .await
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Finding 1: the desktop's dialog showed the absolute path a bare `npx`
/// resolved to, but the definition kept only `npx` and every spawn resolved
/// it again, so a program that later appeared earlier on the search path
/// ran with no new approval. A bare command now starts only the program the
/// dialog approved, which its definition records, until a save approves
/// another.
#[cfg(unix)]
#[tokio::test]
async fn a_bare_command_starts_only_the_program_the_dialog_approved() {
    let approved = tempfile::tempdir().unwrap();
    let shadow = tempfile::tempdir().unwrap();
    let approved_npx = write_printing_npx(approved.path(), "approved");
    // The shadow directory comes first on the search path, and starts empty.
    let search_path = std::env::join_paths([shadow.path(), approved.path()]).unwrap();
    let _path = super::stdio::HostPathGuard::set(Some(search_path)).await;

    // What the dialog shows for a bare `npx`, and the desktop records.
    let shown = super::resolve_stdio_executable("npx").await.unwrap();
    assert_eq!(shown, approved_npx);
    let mut definition = parse(r#"{"servers":[{"name":"files","command":"npx"}]}"#)
        .unwrap()
        .0
        .remove(0);
    definition.approved_executable = Some(shown.to_string_lossy().into_owned());
    assert_eq!(
        spawned_output(&definition, CommandApproval::Required).await,
        "approved"
    );

    // Another `npx` appears earlier on the search path. It does not start,
    // whether or not this process has the dialog, and the refusal names
    // both programs.
    let shadow_npx = write_printing_npx(shadow.path(), "shadow");
    for approval in [CommandApproval::Required, CommandApproval::Optional] {
        let message = definition
            .build_command(&BTreeMap::new(), approval)
            .await
            .expect_err("nobody approved the new program")
            .to_string();
        assert!(message.contains("Needs approval:"), "{message}");
        assert!(
            message.contains(&shadow_npx.display().to_string()),
            "{message}"
        );
        assert!(
            message.contains(&approved_npx.display().to_string()),
            "{message}"
        );
    }

    // A save through the dialog approves the new program, and it runs.
    let shown = super::resolve_stdio_executable("npx").await.unwrap();
    assert_eq!(shown, shadow_npx);
    definition.approved_executable = Some(shown.to_string_lossy().into_owned());
    assert_eq!(
        spawned_output(&definition, CommandApproval::Required).await,
        "shadow"
    );
}

/// Where the desktop's dialog guards local commands, a bare command with no
/// approved program does not start. An absolute command names its program
/// and needs none. Without the dialog, as in the CLI or on a self-hosted
/// server, a bare name runs what it resolves to, as before.
#[cfg(unix)]
#[tokio::test]
async fn a_bare_command_without_an_approved_program_runs_only_without_the_dialog() {
    let directory = tempfile::tempdir().unwrap();
    let npx = write_printing_npx(directory.path(), "found");
    let _path =
        super::stdio::HostPathGuard::set(Some(directory.path().as_os_str().to_owned())).await;
    let bare = parse(r#"{"servers":[{"name":"files","command":"npx"}]}"#)
        .unwrap()
        .0
        .remove(0);
    let message = bare
        .build_command(&BTreeMap::new(), CommandApproval::Required)
        .await
        .expect_err("the dialog approved no program")
        .to_string();
    assert!(message.contains("Needs approval:"), "{message}");
    assert!(message.contains(&npx.display().to_string()), "{message}");
    assert_eq!(
        spawned_output(&bare, CommandApproval::Optional).await,
        "found"
    );

    let mut absolute = bare.clone();
    absolute.command = Some(npx.to_string_lossy().into_owned());
    assert_eq!(
        spawned_output(&absolute, CommandApproval::Required).await,
        "found"
    );
}

/// Finding 1 end to end on a desktop server: saved with the program the
/// dialog approved, the server runs it. Once another `npx` appears earlier
/// on the search path, a reconnect refuses to start it, the server reads as
/// needing approval with both paths named, and the supervisor stops
/// retrying it. A save that approves the new program runs that one.
#[cfg(unix)]
#[tokio::test]
async fn a_shadowed_command_needs_approval_until_a_save_approves_it() {
    let (runtime, _store, directory) = test_runtime().await;
    runtime.require_command_approval();
    let approved = tempfile::tempdir().unwrap();
    let shadow = tempfile::tempdir().unwrap();
    let ran = directory.path().join("ran.log");
    let approved_npx = write_mcp_npx(approved.path(), "approved", &ran);
    let search_path = std::env::join_paths([shadow.path(), approved.path()]).unwrap();
    let _path = super::stdio::HostPathGuard::set(Some(search_path)).await;

    let mut definition = disabled_definition("files", "npx");
    definition.enabled = true;
    let error = runtime
        .replace(McpServersConfig {
            servers: vec![definition.clone()],
        })
        .await
        .expect_err("the dialog approved no program");
    assert!(error.to_string().contains("Needs approval:"), "{error}");
    assert!(!ran.exists(), "nothing started");

    definition.approved_executable = Some(approved_npx.to_string_lossy().into_owned());
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![definition.clone()],
        })
        .await
        .unwrap();
    assert_eq!(info.servers[0].health, McpHealth::Healthy, "{info:?}");
    assert_eq!(std::fs::read_to_string(&ran).unwrap(), "approved\n");

    let shadow_npx = write_mcp_npx(shadow.path(), "shadow", &ran);
    let error = runtime
        .reconnect("files")
        .await
        .expect_err("nobody approved the new program");
    assert!(error.to_string().contains("Needs approval:"), "{error}");
    let info = runtime.info().await;
    assert_eq!(info.servers[0].health, McpHealth::Degraded);
    let diagnostic = info.servers[0].diagnostic.clone().unwrap();
    assert!(diagnostic.starts_with("Needs approval:"), "{diagnostic}");
    assert!(
        diagnostic.contains(&shadow_npx.display().to_string()),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&approved_npx.display().to_string()),
        "{diagnostic}"
    );
    assert!(
        runtime
            .supervised_servers(ManualLockdown::Open)
            .await
            .is_empty(),
        "the supervisor leaves a server that needs approval alone"
    );
    assert_eq!(
        std::fs::read_to_string(&ran).unwrap(),
        "approved\n",
        "the new program never started"
    );

    definition.approved_executable = Some(shadow_npx.to_string_lossy().into_owned());
    let info = runtime
        .replace(McpServersConfig {
            servers: vec![definition],
        })
        .await
        .unwrap();
    assert_eq!(info.servers[0].health, McpHealth::Healthy, "{info:?}");
    assert_eq!(std::fs::read_to_string(&ran).unwrap(), "approved\nshadow\n");
}

/// An approved program belongs only to a bare command, as an absolute path.
#[test]
fn an_approved_program_belongs_only_to_a_bare_command() {
    parse(
        r#"{"servers":[{"name":"files","command":"npx","approved_executable":"/opt/tools/bin/npx"}]}"#,
    )
    .unwrap();
    for (json, expected) in [
        (
            r#"{"servers":[{"name":"files","command":"/opt/tools/bin/npx","approved_executable":"/opt/tools/bin/npx"}]}"#,
            "bare name",
        ),
        (
            r#"{"servers":[{"name":"files","command":"npx","approved_executable":"bin/npx"}]}"#,
            "absolute path",
        ),
        (
            r#"{"servers":[{"name":"docs","url":"https://mcp.example.com/mcp","approved_executable":"/opt/tools/bin/npx"}]}"#,
            "only to command servers",
        ),
    ] {
        let error = parse(json).err().unwrap().to_string();
        assert!(error.contains(expected), "{error}");
    }
}

/// Finding 2: the PATH a child was given kept relative entries, although
/// resolution skips them. With `.` on the login PATH and a working directory
/// in a checkout, a script's `#!/usr/bin/env node` ran the checkout's `node`.
/// The forwarded PATH now keeps absolute directories only.
#[cfg(unix)]
#[tokio::test]
async fn the_forwarded_path_keeps_only_absolute_directories() {
    use std::os::unix::fs::PermissionsExt;

    let checkout = tempfile::tempdir().unwrap();
    let probe = "tidebreak-review-probe";
    let planted = checkout.path().join(probe);
    std::fs::write(&planted, "#!/bin/sh\nprintf checkout\n").unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
    let json = serde_json::json!({"servers": [{
        "name": "files",
        "command": "/usr/bin/env",
        "args": [probe],
        "cwd": checkout.path(),
    }]})
    .to_string();
    let config = parse(&json).unwrap();
    let _path =
        super::stdio::HostPathGuard::set(Some(".:node_modules/.bin:/usr/bin:/bin".into())).await;
    let mut command = config.0[0]
        .build_command(&BTreeMap::new(), CommandApproval::Optional)
        .await
        .unwrap();
    let path = command
        .as_std()
        .get_envs()
        .find(|(name, _)| *name == "PATH")
        .and_then(|(_, value)| value)
        .map(std::ffi::OsStr::to_os_string);
    assert_eq!(path, Some("/usr/bin:/bin".into()));
    let output = command.output().await.unwrap();
    assert_ne!(String::from_utf8_lossy(&output.stdout), "checkout");
    assert!(!output.status.success(), "{output:?}");
}

/// Finding 3: a definition that declares PATH or HOME in `env` with no
/// stored value, as after an import, still got the default, though the
/// desktop's dialog said it would not. A declared name never gets the
/// default: the child gets the stored value or none, and the dialog reads
/// the same list.
#[tokio::test]
async fn a_declared_path_or_home_never_gets_the_default() {
    let config =
        parse(r#"{"servers":[{"name":"docs","command":"/bin/docs","env":["PATH","HOME"]}]}"#)
            .unwrap();
    let _path = super::stdio::HostPathGuard::set(Some("/opt/host/bin".into())).await;
    let environment = |command: &tokio::process::Command| -> BTreeMap<String, String> {
        command
            .as_std()
            .get_envs()
            .filter_map(|(name, value)| {
                Some((
                    name.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect()
    };
    let unstored = config.0[0]
        .build_command(&BTreeMap::new(), CommandApproval::Optional)
        .await
        .unwrap();
    assert_eq!(environment(&unstored), BTreeMap::new());

    let stored = BTreeMap::from([("PATH".to_string(), "/opt/declared/bin".to_string())]);
    let command = config.0[0]
        .build_command(&stored, CommandApproval::Optional)
        .await
        .unwrap();
    assert_eq!(environment(&command), stored);

    assert!(super::defaulted_names(["PATH", "HOME"]).is_empty());
    assert_eq!(super::defaulted_names(["PATH"]), ["HOME"]);
}

