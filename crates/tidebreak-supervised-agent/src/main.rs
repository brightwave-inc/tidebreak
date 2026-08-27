//! The `tidebreak-supervised-agent` binary.
//!
//! Assembly order matters and each failure has a distinct voice:
//!
//! 1. Resolve the environment contract; a missing or unusable input exits
//!    with its own code before anything runs.
//! 2. Probe the selected engine on this image and reconcile the requested
//!    reasoning effort against its ladder, so bootstrap can report what
//!    actually applied.
//! 3. Bootstrap: wait for outbound trust, clone the declared repositories,
//!    and collect the lifecycle events describing that work.
//! 4. Hand everything to the driver, which reports those events on its first
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
    gateway_inference_from_env, HarnessEngine, HarnessEngineSpec,
};
use tidebreak_supervised_agent::inputs::{resolve, RawInputs};
use tidebreak_supervised_agent::trust::TrustOptions;
use tidebreak_supervised_agent::wip::WipContext;
use tidebreak_supervised_agent::{bootstrap, effort, EXIT_MISSING_INPUT};

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

    let registry = builtin_registry();
    let Some(adapter) = registry.get(inputs.engine) else {
        eprintln!("no adapter is registered for engine {}", inputs.engine);
        return EXIT_MISSING_INPUT;
    };

    let host = HostEnv::from_process();
    let probe = adapter.probe(&host).await;
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

    let mut driver = Driver::new(Control::new(&inputs.control_url), engine, &inputs)
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
