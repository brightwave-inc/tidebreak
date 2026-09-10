//! The `tidebreak-supervised-agent` binary.
//!
//! Assembly order matters and each failure has a distinct voice:
//!
//! 1. Resolve the environment contract; a missing or unusable input exits
//!    with its own code before anything runs.
//! 2. Probe the selected engine on this image.
//! 3. Register a managed engine and require the exact runtime acknowledgment.
//! 4. Reconcile reasoning effort, prepare outbound trust, clone the declared repositories,
//!    and collect the lifecycle events describing that work.
//! 5. Hand everything to the driver, which reports those events on its first
//!    poll and then runs the turn loop until the endpoint stops the run.
//!
//! Failures before the driver starts print to stderr — the pod log is the
//! only witness that early. Once the driver is polling, failures also reach
//! the supervising endpoint as lifecycle events.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tidebreak_harness::builtin_registry;
use tidebreak_harness::HostEnv;
use tidebreak_supervised_agent::control::Control;
use tidebreak_supervised_agent::drive::Driver;
use tidebreak_supervised_agent::harness_engine::{
    gateway_inference_from_env, GatewayInference, HarnessEngine, HarnessEngineSpec,
};
use tidebreak_supervised_agent::inputs::{resolve, Inputs, RawInputs};
use tidebreak_supervised_agent::trust::TrustOptions;
use tidebreak_supervised_agent::wip::WipContext;
use tidebreak_supervised_agent::{
    bootstrap, effort, registration, EXIT_CONTROL_FATAL, EXIT_MISSING_INPUT,
};

#[tokio::main]
async fn main() {
    std::process::exit(run().await);
}

async fn run() -> i32 {
    let inputs = match resolve(RawInputs::from_env()) {
        Ok(inputs) => inputs,
        Err(error) => {
            eprintln!("{error}");
            return error.code;
        }
    };
    // Part of the environment contract: a placeholder credential without a
    // gateway URL is a broken pod, better refused here than at the first
    // turn's inference request.
    let gateway_inference = match gateway_inference_from_env() {
        Ok(inference) => inference,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_MISSING_INPUT;
        }
    };

    run_inputs(inputs, gateway_inference, HostEnv::from_process()).await
}

async fn run_inputs(
    inputs: Inputs,
    gateway_inference: Option<GatewayInference>,
    host: HostEnv,
) -> i32 {
    let registry = builtin_registry();
    let Some(adapter) = registry.get(inputs.engine) else {
        eprintln!("no adapter is registered for engine {}", inputs.engine);
        return EXIT_MISSING_INPUT;
    };

    let probe = adapter.probe(&host).await;
    let embedded_engine = match inputs
        .embedded_engine
        .as_ref()
        .map(|expected| registration::from_probe(expected, inputs.engine, &probe))
        .transpose()
    {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_MISSING_INPUT;
        }
    };
    let session_id = embedded_engine
        .as_ref()
        .map(|identity| identity.engine_session_id)
        .unwrap_or_else(tidebreak_core::SessionId::new);
    let control = Control::new(&inputs.control_url).with_embedded_engine(embedded_engine);
    if let Err(error) = control.register().await {
        eprintln!("{error}");
        return EXIT_CONTROL_FATAL;
    }
    // Resolving the ladder can shell out to the engine's model catalog, so
    // only do it when there is a request to reconcile.
    let effective = match inputs.reasoning_effort.as_deref() {
        Some(requested) => {
            let ladder = effort::ladder(adapter.as_ref(), &probe, inputs.model.as_deref()).await;
            effort::reconcile(Some(requested), &ladder)
        }
        None => None,
    };

    let trust_options = TrustOptions::from_env();
    // Bootstrap blocks on the trust sidecar and on git; keep the runtime's
    // workers free while it does.
    let bootstrap = {
        let inputs = inputs.clone();
        let trust_options = trust_options.clone();
        let joined = tokio::task::spawn_blocking(move || {
            bootstrap::run(
                &inputs,
                &trust_options,
                effective.map(tidebreak_core::ReasoningEffort::as_str),
            )
        })
        .await;
        match joined {
            Ok(Ok(bootstrap)) => bootstrap,
            Ok(Err(error)) => {
                eprintln!("{error}");
                return 1;
            }
            Err(error) => {
                eprintln!("the bootstrap task failed: {error}");
                return 1;
            }
        }
    };

    let workdir = canonical(&bootstrap.workdir);
    let workspace = canonical(&inputs.workspace);
    // The engine may read the workspace around its worktree when the run
    // cloned into a subdirectory of it.
    let allowed_read_roots = if workspace == workdir {
        Vec::new()
    } else {
        vec![workspace]
    };

    let trust_env: Vec<(OsString, OsString)> = bootstrap
        .trust
        .environment()
        .iter()
        .map(|(name, path)| (OsString::from(name), path.as_os_str().to_owned()))
        .collect();

    let engine = HarnessEngine::new(HarnessEngineSpec {
        session_id,
        adapter,
        probe,
        model: inputs.model.clone(),
        reasoning_effort: effective,
        worktree: workdir.clone(),
        allowed_read_roots,
        trust_env,
        gateway_inference,
    });

    let wip = if !inputs.forge_push_denied && !bootstrap.clones.is_empty() {
        Some(
            WipContext::capture(
                inputs
                    .sandbox_id
                    .clone()
                    .unwrap_or_else(|| "local".to_owned()),
                inputs.incarnation,
                &bootstrap.clones,
                bootstrap.trust.clone(),
            )
            .await,
        )
    } else {
        None
    };

    let mut driver = Driver::new(control, engine, &inputs)
        .preload_events(bootstrap.events)
        .with_workdir(workdir);
    if let Some(wip) = wip {
        driver = driver.with_wip(wip);
    }
    match driver.run().await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{}", error.message);
            error.code
        }
    }
}

/// Resolves symlinks so path comparisons and git commands agree; a path that
/// cannot be canonicalized is used as declared.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tidebreak_core::{HarnessKind, SessionId};

    #[tokio::test]
    async fn missing_registration_acknowledgment_stops_before_catalog_bootstrap_and_launch() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("codex");
        let calls = dir.path().join("calls");
        let log = calls.to_string_lossy().replace('\'', "'\\''");
        std::fs::write(&binary, format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\ncase \"$1\" in\n--version) echo 'codex-cli 0.147.0';;\nlogin) exit 1;;\n*) exit 71;;\nesac\n"
        )).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let observed = received.clone();
        let app = axum::Router::new().route(
            "/supervisor/poll",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                observed.lock().unwrap().push(body);
                async { axum::Json(serde_json::json!({})) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let session = SessionId::new();
        let workspace = dir.path().join("must-not-create");
        let inputs = resolve(RawInputs {
            task: Some("inspect the repository".into()),
            workspace: Some(workspace.to_string_lossy().into_owned()),
            engine: Some("codex".into()),
            embedded_engine: Some(
                serde_json::json!({"engine":"codex", "engine_session_id":session}).to_string(),
            ),
            supervisor_endpoint: Some(endpoint),
            reasoning_effort: Some("high".into()),
            repository_url: Some("https://github.com/example/project".into()),
            ..RawInputs::default()
        })
        .unwrap();
        let host = HostEnv::from_process().with_declared_env(vec![
            (
                OsString::from("PATH"),
                OsString::from(format!("{}:/usr/bin:/bin", dir.path().display())),
            ),
            (OsString::from("HOME"), dir.path().as_os_str().to_owned()),
        ]);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_inputs(inputs, None, host),
        )
        .await
        .unwrap();
        assert_eq!(result, EXIT_CONTROL_FATAL);
        assert!(
            !workspace.exists(),
            "bootstrap must not run without acknowledgment"
        );
        let calls = std::fs::read_to_string(calls).unwrap();
        assert!(
            calls
                .lines()
                .all(|line| line == "--version" || line == "login status"),
            "only probe commands may run: {calls}"
        );
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(
            received[0]["embedded_engine"]["engine_session_id"],
            session.to_string()
        );
        assert_eq!(
            received[0]["embedded_engine"]["engine"],
            HarnessKind::Codex.as_str()
        );
        server.abort();
    }
}
