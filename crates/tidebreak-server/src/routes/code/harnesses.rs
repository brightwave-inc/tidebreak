use axum::extract::{Path, Query};

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::Json;

use super::types::{
    CodeHarnessInstallSnapshot, HarnessAuthMode, HarnessDoctorEntry, HarnessDoctorReport,
    HarnessModel, HarnessModelList, InstallHarnessQuery,
};
use crate::code::harness_label;
use crate::code::harness_llm::relay_covered;
use tidebreak_core::HarnessKind;

/// The doctor surface, served from the memoized probes (decision 0034).
pub async fn list_harnesses(code: ScopedCode) -> Result<Json<HarnessDoctorReport>, ServerError> {
    Ok(Json(doctor(&code).await?))
}

/// The explicit re-probe decision 0034 puts behind the doctor's refresh: drop
/// the memoized probes, then report what a cold read finds. This is how a
/// harness installed or signed into while the app runs becomes visible.
///
/// Refresh does not install. It used to `npm install` every pin, which spent
/// hundreds of megabytes and minutes on three engines the reader had not
/// asked for to make a fourth usable. An engine downloads when someone picks
/// it, or when they press Download on its doctor row.
pub async fn refresh_harnesses(code: ScopedCode) -> Result<Json<HarnessDoctorReport>, ServerError> {
    code.invalidate_probes();
    Ok(Json(doctor(&code).await?))
}

/// Start the pinned install of one engine ahead of a session create, and
/// report where that install stands.
///
/// Every surface that picks an engine calls this when it opens and when the
/// engine changes. A cold pin is minutes of `npm install`; on the create path
/// that is a silent stall, so it runs here instead and reports on
/// `WS /code/updates`. Answers immediately in every case — already installed,
/// already running, or now started — and never installs twice for one pin.
///
/// `?deliberate=true` marks a reader who pressed Download rather than a picker
/// warming its selection, which is the only case that retries a managed-Node
/// install that already failed.
pub async fn install_harness(
    code: ScopedCode,
    Path(kind): Path<HarnessKind>,
    Query(query): Query<InstallHarnessQuery>,
) -> Result<Json<CodeHarnessInstallSnapshot>, ServerError> {
    Ok(Json(code.start_harness_install(kind, query.deliberate)?))
}

/// Models this harness currently lists. Not on the doctor path.
pub async fn list_harness_models(
    code: ScopedCode,
    Path(kind): Path<HarnessKind>,
) -> Result<Json<HarnessModelList>, ServerError> {
    let adapter = code.adapter(kind)?;
    let probe = code.probe(adapter.as_ref()).await;
    if !probe.found {
        return Err(ServerError::unprocessable_kind(
            "harness_not_found",
            format!("{kind} is not installed"),
        ));
    }
    let listed = adapter.list_models(&probe).await;
    // An engine that states one ladder for every model says so directly. One
    // that states a ladder per row — Codex — has no single answer, so the
    // outer bound is the union of what its rows advertise.
    let mut reasoning_efforts = adapter.reasoning_efforts(&probe);
    if reasoning_efforts.is_empty() {
        reasoning_efforts = listed
            .iter()
            .flat_map(|model| model.reasoning_efforts.iter().copied())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    let models = listed
        .into_iter()
        .map(|model| HarnessModel {
            id: model.id,
            label: model.label,
            default: model.default,
            reasoning_efforts: model.reasoning_efforts,
            fast_mode: model.fast_mode,
        })
        .collect();
    Ok(Json(HarnessModelList {
        kind,
        models,
        reasoning_efforts,
    }))
}

async fn doctor(code: &ScopedCode) -> Result<HarnessDoctorReport, ServerError> {
    let sessions = code.list_sessions().await?;
    let sessions = &sessions;
    // On a gateway-hosted machine, engine inference rides the on-behalf-of
    // relay (decision 71) and the local sign-in probe answers the wrong
    // question: nobody can open a terminal there, and a signed-out engine is
    // a ready one. Everywhere else the probe decides, exactly as before.
    let hosted = code.harness_llm_relay_active();
    // Probe the kinds concurrently. A cold probe is a login shell plus a
    // version and an authentication subprocess, so a serial walk over the
    // harnesses is most of what this route costs on a cache miss.
    let harnesses = futures::future::join_all(HarnessKind::ALL.iter().filter_map(|kind| {
        let adapter = code.adapters().get(*kind)?;
        Some(async move {
            let probe = code.probe(adapter.as_ref()).await;
            let caps = adapter.capabilities(&probe);
            let unrecognized_event_count = sessions
                .iter()
                .filter(|session| session.harness_kind == *kind)
                .map(|session| session.unrecognized_event_count)
                .sum();
            let install_error = code.pin_install_error(*kind);
            let installable = tidebreak_harness::pin_for(*kind).is_some();
            let label = harness_label(*kind);
            let auth_mode = resolve_auth_mode(hosted, *kind, &probe);
            let remediation = if let Some(err) = install_error {
                format!("could not download the pinned {kind} binary: {err}")
            } else if auth_mode == HarnessAuthMode::HostedUnavailable {
                format!("{label} is not available on hosted machines yet.")
            } else if !probe.found && !installable {
                format!("this build ships no pinned {kind} binary to download")
            } else if auth_mode == HarnessAuthMode::GatewayRelay {
                // The relay carries each turn as the caller, so no local
                // sign-in stands between the reader and a session. A missing
                // binary is the lazy pin's download, not a fault.
                String::new()
            } else if auth_mode == HarnessAuthMode::GatewayManaged {
                // Credentials are already wired on this machine. Asking the
                // reader to sign in would send them after a login that
                // nothing here needs (issue 2749).
                String::new()
            } else if probe.found && probe.authenticated == Some(false) {
                format!("Sign in to {label} in your own terminal, then re-check.")
            } else if probe.found && probe.authenticated.is_none() {
                format!(
                    "Tidebreak could not verify the {label} sign-in. Sign in to {label} in your own terminal, then re-check."
                )
            } else {
                // A missing but installable engine has nothing for the reader
                // to repair: picking it downloads it.
                String::new()
            };
            HarnessDoctorEntry {
                kind: *kind,
                found: probe.found,
                installable,
                path: probe
                    .binary_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                version: probe.version,
                tier: kind.tier(),
                caps,
                commands: probe.commands,
                authenticated: probe.authenticated,
                auth_mode,
                remediation,
                stderr: probe.stderr,
                unrecognized_event_count,
                relaunch_composes_permission_mode: adapter
                    .relaunch_composes_permission_mode(),
            }
        })
    }))
    .await;
    Ok(HarnessDoctorReport { harnesses })
}

/// How a session of this engine authenticates on this machine.
///
/// The vendor login is one mode of three, not the question. A hosted machine
/// runs every covered engine through the relay (decision 71). Elsewhere, a
/// machine whose engines are pointed at a gateway carries inference on a
/// credential nobody logged in for: the engine's own login check reports
/// signed out, and the machine works. Reading that as "signed out" told
/// every picker to disable the engine (issue 2749), so a confirmed vendor
/// login answers first and an observed credential override answers next.
///
/// The observation is deliberately narrower than the one the create path
/// consults: engines whose override surfaces Tidebreak does not read stay on
/// `LocalSignIn` rather than claim a credential the reader cannot see.
fn resolve_auth_mode(
    hosted: bool,
    kind: HarnessKind,
    probe: &tidebreak_harness::HarnessProbe,
) -> HarnessAuthMode {
    if hosted {
        return if relay_covered(kind) {
            HarnessAuthMode::GatewayRelay
        } else {
            HarnessAuthMode::HostedUnavailable
        };
    }
    if probe.authenticated != Some(true)
        && tidebreak_harness::observe_auth_mode(kind, &probe.env).is_override()
    {
        return HarnessAuthMode::GatewayManaged;
    }
    HarnessAuthMode::LocalSignIn
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use tidebreak_harness::HarnessProbe;

    fn probe(authenticated: Option<bool>, env: Vec<(OsString, OsString)>) -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: None,
            version: None,
            authenticated,
            stderr: String::new(),
            env,
            commands: Vec::new(),
        }
    }

    fn gateway_env() -> Vec<(OsString, OsString)> {
        vec![(
            OsString::from("ANTHROPIC_BASE_URL"),
            OsString::from("https://gateway.example"),
        )]
    }

    #[test]
    fn a_credential_override_reads_as_gateway_managed() {
        assert_eq!(
            resolve_auth_mode(
                false,
                HarnessKind::ClaudeCode,
                &probe(Some(false), gateway_env())
            ),
            HarnessAuthMode::GatewayManaged
        );
        // The unverified case is the one the doctor used to call "Unverified"
        // and disable.
        assert_eq!(
            resolve_auth_mode(false, HarnessKind::ClaudeCode, &probe(None, gateway_env())),
            HarnessAuthMode::GatewayManaged
        );
    }

    #[test]
    fn a_confirmed_vendor_login_stays_local() {
        assert_eq!(
            resolve_auth_mode(
                false,
                HarnessKind::ClaudeCode,
                &probe(Some(true), gateway_env())
            ),
            HarnessAuthMode::LocalSignIn
        );
        assert_eq!(
            resolve_auth_mode(false, HarnessKind::ClaudeCode, &probe(Some(false), vec![])),
            HarnessAuthMode::LocalSignIn
        );
    }

    #[test]
    fn engines_with_no_override_surface_stay_local() {
        // `auth_override_present` answers `true` for these to avoid refusing a
        // session; the doctor must not turn that into a credential claim.
        for kind in [HarnessKind::Opencode, HarnessKind::Grok] {
            assert_eq!(
                resolve_auth_mode(false, kind, &probe(Some(false), gateway_env())),
                HarnessAuthMode::LocalSignIn
            );
        }
    }

    #[test]
    fn a_hosted_machine_still_reports_the_relay() {
        assert_eq!(
            resolve_auth_mode(
                true,
                HarnessKind::ClaudeCode,
                &probe(Some(false), gateway_env())
            ),
            HarnessAuthMode::GatewayRelay
        );
    }
}
