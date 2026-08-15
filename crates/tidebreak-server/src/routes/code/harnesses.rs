use axum::extract::State;

use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

use super::require_code;
use super::types::{HarnessDoctorEntry, HarnessDoctorReport};
use tidebreak_core::HarnessKind;
use tidebreak_harness::HostEnv;

pub async fn list_harnesses(
    State(state): State<AppState>,
) -> Result<Json<HarnessDoctorReport>, ServerError> {
    Ok(Json(doctor(&state).await?))
}

pub async fn refresh_harnesses(
    State(state): State<AppState>,
) -> Result<Json<HarnessDoctorReport>, ServerError> {
    Ok(Json(doctor(&state).await?))
}

async fn doctor(state: &AppState) -> Result<HarnessDoctorReport, ServerError> {
    let runtime = require_code(state)?;
    let sessions = runtime.list_sessions().await?;
    let host = HostEnv::from_process();
    let mut harnesses = Vec::new();
    for kind in HarnessKind::ALL {
        let Some(adapter) = runtime.adapters.get(*kind) else {
            continue;
        };
        let probe = adapter.probe(&host).await;
        let caps = adapter.capabilities(&probe);
        let unrecognized_event_count = sessions
            .iter()
            .filter(|session| session.harness_kind == *kind)
            .map(|session| session.unrecognized_event_count)
            .sum();
        let remediation = if !probe.found {
            format!("install {kind} so it is on your login-shell PATH, then refresh")
        } else if probe.authenticated == Some(false) {
            format!("sign in to {kind} in your own terminal, then refresh")
        } else {
            String::new()
        };
        harnesses.push(HarnessDoctorEntry {
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
        });
    }
    Ok(HarnessDoctorReport { harnesses })
}
