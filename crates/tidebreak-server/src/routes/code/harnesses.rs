use axum::extract::{Path, State};

use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

use super::require_code;
use super::types::{HarnessDoctorEntry, HarnessDoctorReport, HarnessModel, HarnessModelList};
use tidebreak_core::HarnessKind;

/// The doctor surface, served from the memoized probes (decision 0034).
pub async fn list_harnesses(
    State(state): State<AppState>,
) -> Result<Json<HarnessDoctorReport>, ServerError> {
    Ok(Json(doctor(&state).await?))
}

/// The explicit re-probe decision 0034 puts behind the doctor's refresh: drop
/// the memoized probes, then report what a cold read finds. This is how a
/// harness installed or signed into while the app runs becomes visible.
pub async fn refresh_harnesses(
    State(state): State<AppState>,
) -> Result<Json<HarnessDoctorReport>, ServerError> {
    let runtime = require_code(&state)?;
    #[cfg(not(test))]
    runtime.refresh_pinned_harnesses().await;
    runtime.invalidate_probes();
    Ok(Json(doctor(&state).await?))
}

/// Models this harness currently lists. Not on the doctor path.
pub async fn list_harness_models(
    State(state): State<AppState>,
    Path(kind): Path<HarnessKind>,
) -> Result<Json<HarnessModelList>, ServerError> {
    let runtime = require_code(&state)?;
    let adapter = runtime.adapter(kind)?;
    let probe = runtime.probe(adapter.as_ref()).await;
    if !probe.found {
        return Err(ServerError::unprocessable_kind(
            "harness_not_found",
            format!("{kind} is not installed"),
        ));
    }
    let listed = adapter.list_models(&probe).await;
    let models = listed
        .into_iter()
        .map(|model| HarnessModel {
            id: model.id,
            label: model.label,
            default: model.default,
        })
        .collect();
    Ok(Json(HarnessModelList { kind, models }))
}

async fn doctor(state: &AppState) -> Result<HarnessDoctorReport, ServerError> {
    let runtime = require_code(state)?;
    let sessions = runtime.list_sessions().await?;
    let sessions = &sessions;
    // Probe the kinds concurrently. A cold probe is a login shell plus a
    // version and an authentication subprocess, so a serial walk over the
    // harnesses is most of what this route costs on a cache miss.
    let harnesses = futures::future::join_all(HarnessKind::ALL.iter().filter_map(|kind| {
        let adapter = runtime.adapters.get(*kind)?;
        Some(async move {
            let probe = runtime.probe(adapter.as_ref()).await;
            let caps = adapter.capabilities(&probe);
            let unrecognized_event_count = sessions
                .iter()
                .filter(|session| session.harness_kind == *kind)
                .map(|session| session.unrecognized_event_count)
                .sum();
            let install_error = runtime.pin_install_error(*kind);
            let remediation = if let Some(err) = install_error {
                format!("could not install the pinned {kind} binary: {err}")
            } else if !probe.found {
                format!("refresh to install the pinned {kind} binary")
            } else if probe.authenticated == Some(false) {
                format!("sign in to {kind} in your own terminal, then refresh")
            } else {
                String::new()
            };
            HarnessDoctorEntry {
                kind: *kind,
                found: probe.found,
                path: probe
                    .binary_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                version: probe.version,
                tier: kind.tier(),
                caps,
                authenticated: probe.authenticated,
                remediation,
                stderr: probe.stderr,
                unrecognized_event_count,
            }
        })
    }))
    .await;
    Ok(HarnessDoctorReport { harnesses })
}
