//! Container entrypoint for the Tidebreak sandbox agent.
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
//! - `TIDEBREAK_SANDBOX_LISTEN` — the address the listener binds (default
//!   `0.0.0.0:8080`, the port the container publishes).
//! - `TIDEBREAK_TRANSPORT_SECRET` — the per-run transport secret the host minted
//!   and injected. The supervisor requires an inbound attach to present it before
//!   it installs the connection. If it is absent the sandbox fails closed: it
//!   still binds and answers, but refuses every attach rather than serving one
//!   unauthenticated.
//! - `TIDEBREAK_SANDBOX_WORKSPACE` — the agent's in-container workspace directory,
//!   the root the `exec` and filesystem tools are scoped to (default
//!   `/workspace`, provisioned in the container image).
//! - `TIDEBREAK_SANDBOX_LIFETIME_CAP_SECS` — an absolute lifetime cap. When it
//!   elapses the entrypoint exits, which stops the container.
//! - `TIDEBREAK_SANDBOX_IDLE_TIMEOUT_SECS` — an idle timeout reset by authenticated
//!   host keepalives and run activity. When it elapses the entrypoint exits.
//!
//! # Why the cap is enforced here
//!
//! This process is the container's only top-level process — PID 1, or the sole
//! child of the runtime's init when the host provisions with one — so its exit is
//! the container's exit. That is the only place an absolute cap can be enforced and still hold when the
//! host is what failed: a host-side timer dies with the host, and the host dying
//! mid-run is exactly the case that strands a container. The host's orphan sweep
//! reclaims containers a *living* host has lost track of; this cap covers the
//! host that never comes back.
//!
//! The idle watchdog reclaims an abandoned container promptly, while the
//! absolute cap remains independent and ends even a host-owned run that is
//! genuinely wedged forever.

use std::env;
use std::time::Duration;

use tidebreak_sandbox_agent::{run_agent, EgressProxy, EgressProxyConfig, Supervisor};
use tidebreak_sandbox_protocol::{reverse::Capability, SandboxRun, TransportSecret};

/// Default listener address; the container publishes this port.
const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
/// Environment variable carrying the per-run transport secret into the container.
/// Must match the name the backend injects (see `sandbox_docker`'s
/// `TRANSPORT_SECRET_ENV`).
const TRANSPORT_SECRET_ENV: &str = "TIDEBREAK_TRANSPORT_SECRET";

/// Default in-container workspace root for the sandbox-resident tool surface.
const DEFAULT_WORKSPACE: &str = "/workspace";

/// Environment variable carrying the absolute lifetime cap, in seconds. Must
/// match the name the backend injects (see `sandbox_docker`'s
/// `LIFETIME_CAP_ENV`).
const LIFETIME_CAP_ENV: &str = "TIDEBREAK_SANDBOX_LIFETIME_CAP_SECS";
/// Environment variable carrying the idle timeout, in seconds.
const IDLE_TIMEOUT_ENV: &str = "TIDEBREAK_SANDBOX_IDLE_TIMEOUT_SECS";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("tidebreak-sandbox-agent: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // The same binary is the image's second face: `egress-proxy` serves the
    // sandbox's network boundary from the one dual-homed container instead of
    // running the agent loop. It honors the same lifetime cap, so a proxy whose
    // host never returns dies on its own exactly like a stranded sandbox.
    if env::args().nth(1).as_deref() == Some("egress-proxy") {
        return run_egress_proxy().await;
    }
    let listen = env::var("TIDEBREAK_SANDBOX_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_owned());
    let workspace =
        env::var("TIDEBREAK_SANDBOX_WORKSPACE").unwrap_or_else(|_| DEFAULT_WORKSPACE.to_owned());

    // The per-run secret the host must present to attach. Fail closed: with none
    // configured the run holds `None`, so every attach is refused rather than
    // served unauthenticated. A supervisor with no secret must not serve.
    let expected_secret = match env::var(TRANSPORT_SECRET_ENV) {
        Ok(secret) if !secret.is_empty() => Some(TransportSecret::new(secret)),
        _ => {
            eprintln!(
                "tidebreak-sandbox-agent: no {TRANSPORT_SECRET_ENV} configured; \
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
        "tidebreak-sandbox-agent listening on {}",
        supervisor.local_addr()?
    );

    // The agent loop is a separate future from the supervisor's listener: it
    // waits for the host to attach and deliver the run init — the task never
    // rides the environment, so a sandbox reclaimed before its handle committed
    // never executed anything — then runs to a submitted result.
    let agent_run = run.clone();
    tokio::spawn(async move {
        let init = agent_run.init().await;
        match run_agent(agent_run, init.task, workspace).await {
            Ok(answer) => eprintln!("tidebreak-sandbox-agent: submitted result: {answer}"),
            Err(error) => eprintln!("tidebreak-sandbox-agent: run failed: {error}"),
        }
    });

    // Keep serving so the host can drain the event stream and drive teardown —
    // but no longer than the lifetime cap, whose whole purpose is to end a run
    // whose host will never drive that teardown.
    let cap = positive_duration(env::var(LIFETIME_CAP_ENV).ok().as_deref());
    let idle = positive_duration(env::var(IDLE_TIMEOUT_ENV).ok().as_deref());
    let activity = run.activity();
    tokio::select! {
        () = supervisor.serve() => {}
        () = lifetime_elapsed(cap) => {
            let secs = cap.unwrap_or_default().as_secs();
            eprintln!("tidebreak-sandbox-agent: lifetime cap of {secs}s reached; exiting");
        }
        () = idle_elapsed(idle, activity) => {
            let secs = idle.unwrap_or_default().as_secs();
            eprintln!("tidebreak-sandbox-agent: idle timeout of {secs}s reached; exiting");
        }
    }
    Ok(())
}

/// Serve the egress-proxy mode until the lifetime cap (if any) elapses.
async fn run_egress_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let config = EgressProxyConfig::from_env();
    if config.policy.denies_everything() {
        eprintln!("tidebreak-sandbox-agent egress-proxy: policy denies all egress");
    }
    let proxy = EgressProxy::bind(config).await?;
    eprintln!(
        "tidebreak-sandbox-agent egress-proxy listening on {} (relay: {})",
        proxy.egress_addr()?,
        match proxy.relay_addr() {
            Some(addr) => addr?.to_string(),
            None => "off".to_owned(),
        }
    );
    let cap = positive_duration(env::var(LIFETIME_CAP_ENV).ok().as_deref());
    tokio::select! {
        () = proxy.serve() => {}
        () = lifetime_elapsed(cap) => {
            let secs = cap.unwrap_or_default().as_secs();
            eprintln!("tidebreak-sandbox-agent egress-proxy: lifetime cap of {secs}s reached; exiting");
        }
    }
    Ok(())
}

/// The configured cap, if the environment names a usable one.
///
/// Absent, unparseable, and zero all mean "no cap": a container must not die on
/// arrival because a cap was mistyped, and a run with no cap configured is no
/// worse off than before one existed.
fn positive_duration(configured: Option<&str>) -> Option<Duration> {
    configured?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
}

/// Completes after one uninterrupted idle period. Every activity generation
/// resets the deadline; with no timeout it never completes.
async fn idle_elapsed(timeout: Option<Duration>, mut activity: tokio::sync::watch::Receiver<u64>) {
    let Some(timeout) = timeout else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        tokio::select! {
            () = tokio::time::sleep(timeout) => return,
            changed = activity.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

/// Completes when the cap elapses. With no cap it never completes, so the
/// `select!` above reduces to serving until teardown.
async fn lifetime_elapsed(cap: Option<Duration>) {
    match cap {
        Some(cap) => tokio::time::sleep(cap).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_or_zero_cap_leaves_the_sandbox_uncapped() {
        assert_eq!(
            positive_duration(Some(" 900 ")),
            Some(Duration::from_secs(900))
        );
        assert_eq!(positive_duration(None), None);
        assert_eq!(positive_duration(Some("")), None);
        assert_eq!(positive_duration(Some("soon")), None);
        assert_eq!(positive_duration(Some("-1")), None);
        // The dangerous misreading: zero must not mean "expire immediately".
        assert_eq!(positive_duration(Some("0")), None);
    }

    /// The cap must fire on its own, and only when configured. A paused clock
    /// proves both without spending the wall time: tokio auto-advances to the
    /// next deadline, and a future with no deadline never becomes ready.
    #[tokio::test(start_paused = true)]
    async fn the_cap_ends_the_run_only_when_one_is_configured() {
        lifetime_elapsed(Some(Duration::from_secs(4 * 60 * 60))).await;

        let uncapped = lifetime_elapsed(None);
        tokio::pin!(uncapped);
        tokio::select! {
            () = &mut uncapped => panic!("an uncapped sandbox must never self-terminate"),
            () = tokio::time::sleep(Duration::from_secs(365 * 24 * 60 * 60)) => {}
        }
    }

    #[tokio::test(start_paused = true)]
    async fn activity_resets_idle_but_not_the_absolute_cap() {
        let (activity, receiver) = tokio::sync::watch::channel(0u64);
        let idle = idle_elapsed(Some(Duration::from_secs(10)), receiver);
        tokio::pin!(idle);
        let absolute = tokio::time::sleep(Duration::from_secs(25));
        tokio::pin!(absolute);

        tokio::time::advance(Duration::from_secs(9)).await;
        activity.send_modify(|generation| *generation += 1);
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(tokio::time::timeout(Duration::ZERO, &mut idle)
            .await
            .is_err());

        tokio::time::advance(Duration::from_secs(7)).await;
        absolute.await;
        assert!(tokio::time::timeout(Duration::ZERO, &mut idle)
            .await
            .is_err());

        tokio::time::advance(Duration::from_secs(3)).await;
        idle.await;
    }
}
