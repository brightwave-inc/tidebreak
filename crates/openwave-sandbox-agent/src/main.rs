//! Container entrypoint for the OpenWave sandbox agent.
//!
//! Binds the supervisor's transport listener and starts the agent loop. The
//! agent blocks until the host dials in and completes the attach handshake — an
//! attached-only run cannot take a model step without its host, which is the
//! model proxy — then runs to a submitted result over the reverse channel. The
//! process keeps serving after the result is submitted so the host holds the
//! connection and drains the event stream; teardown is host-driven.
//!
//! Configuration is by environment, so the image needs no arguments:
//!
//! - `OPENWAVE_SANDBOX_LISTEN` — the address the listener binds (default
//!   `0.0.0.0:8080`, the port the container publishes).
//! - `OPENWAVE_TRANSPORT_SECRET` — the per-run transport secret the host minted
//!   and injected. The supervisor requires an inbound attach to present it before
//!   it installs the connection. If it is absent the sandbox fails closed: it
//!   still binds and answers, but refuses every attach rather than serving one
//!   unauthenticated.
//! - `OPENWAVE_SANDBOX_TASK` — the delegated task. In the full design the host
//!   delivers the task in the run-init payload after the handle commits; until
//!   that init frame is wired, the entrypoint reads it from the environment.

use std::env;

use openwave_sandbox_agent::{run_agent, Supervisor};
use openwave_sandbox_protocol::{reverse::Capability, SandboxRun, TransportSecret};

/// Default listener address; the container publishes this port.
const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
/// Environment variable carrying the per-run transport secret into the container.
/// Must match the name the backend injects (see `sandbox_docker`'s
/// `TRANSPORT_SECRET_ENV`).
const TRANSPORT_SECRET_ENV: &str = "OPENWAVE_TRANSPORT_SECRET";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("openwave-sandbox-agent: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let listen = env::var("OPENWAVE_SANDBOX_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_owned());
    let task = env::var("OPENWAVE_SANDBOX_TASK")
        .unwrap_or_else(|_| "Summarize what this sandbox agent can do.".to_owned());

    // The per-run secret the host must present to attach. Fail closed: with none
    // configured the run holds `None`, so every attach is refused rather than
    // served unauthenticated. A supervisor with no secret must not serve.
    let expected_secret = match env::var(TRANSPORT_SECRET_ENV) {
        Ok(secret) if !secret.is_empty() => Some(TransportSecret::new(secret)),
        _ => {
            eprintln!(
                "openwave-sandbox-agent: no {TRANSPORT_SECRET_ENV} configured; \
                 refusing every attach (fail closed)"
            );
            None
        }
    };

    // Attached-only: the only granted reverse capability is host-proxied model
    // inference. No model credential lives in the container.
    let run = SandboxRun::new([Capability::ModelInference], expected_secret);
    let supervisor = Supervisor::bind(&listen, run.clone()).await?;
    eprintln!(
        "openwave-sandbox-agent listening on {}",
        supervisor.local_addr()?
    );

    // The agent loop is a separate future from the supervisor's listener: it
    // waits for the host to attach, then runs to a submitted result.
    tokio::spawn(async move {
        match run_agent(run, task).await {
            Ok(answer) => eprintln!("openwave-sandbox-agent: submitted result: {answer}"),
            Err(error) => eprintln!("openwave-sandbox-agent: run failed: {error}"),
        }
    });

    // Keep serving so the host can drain the event stream and drive teardown.
    supervisor.serve().await;
    Ok(())
}
