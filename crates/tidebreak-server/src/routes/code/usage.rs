//! Subscription quota usage for code mode.
//!
//! Model Gateway is the richest source and already exposes a stable JSON CLI
//! contract. When it is absent or signed out, Codex's app-server protocol is a
//! useful direct-provider fallback. Other harnesses remain visible in the UI
//! through the doctor, but are not guessed here when their CLIs expose no
//! machine-readable quota command.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tidebreak_harness::{filter_child_env, probe_shell, HostEnv, ProbeCapture};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::Json;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct CodeSubscriptionUsage {
    source: UsageSource,
    providers: Vec<UsageProvider>,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum UsageSource {
    ModelGateway,
    Direct,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct UsageProvider {
    id: String,
    label: String,
    accounts: Vec<UsageAccount>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct UsageAccount {
    id: String,
    label: String,
    is_own: bool,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at_unix_seconds: Option<i64>,
    windows: Vec<UsageWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct UsageWindow {
    key: String,
    label: String,
    used_percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_at_unix_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_scope: Option<String>,
}

/// Read every subscription quota source the local code installation can
/// answer. Source failures are diagnostics, not route failures: a missing
/// optional CLI must render as an honest empty state rather than a broken rail.
pub(crate) async fn subscription_usage(
    _code: ScopedCode,
) -> Result<Json<CodeSubscriptionUsage>, ServerError> {
    Ok(Json(collect_usage().await))
}

async fn collect_usage() -> CodeSubscriptionUsage {
    let mut diagnostics = Vec::new();
    match collect_modelctl().await {
        Ok(Some(report)) if !report.providers.is_empty() => return report,
        Ok(_) => diagnostics.push("Model Gateway returned no subscription usage.".into()),
        Err(error) => {
            tracing::debug!("model-gateway usage unavailable: {error}");
            diagnostics.push("Model Gateway usage is unavailable.".into());
        }
    }

    match collect_codex().await {
        Ok(Some(provider)) => CodeSubscriptionUsage {
            source: UsageSource::Direct,
            providers: vec![provider],
            diagnostics: Vec::new(),
        },
        Ok(None) => {
            diagnostics.push("Codex returned no subscription usage.".into());
            CodeSubscriptionUsage {
                source: UsageSource::Unavailable,
                providers: Vec::new(),
                diagnostics,
            }
        }
        Err(error) => {
            tracing::debug!("direct Codex usage unavailable: {error}");
            diagnostics.push("Direct Codex usage is unavailable.".into());
            CodeSubscriptionUsage {
                source: UsageSource::Unavailable,
                providers: Vec::new(),
                diagnostics,
            }
        }
    }
}

async fn collect_modelctl() -> Result<Option<CodeSubscriptionUsage>, String> {
    let probe = probe_shell(&HostEnv::from_process(), "modelctl")
        .await
        .map_err(|error| error.to_string())?;
    let mut command = command_from_probe(&probe);
    command.args(["--json", "usage"]);
    let output = timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| "usage command timed out".to_owned())?
        .map_err(|error| format!("could not run usage command: {error}"))?;
    if !output.status.success() {
        return Err(bounded_text(&output.stderr, 320));
    }
    if output.stdout.len() > MAX_JSON_BYTES {
        return Err("usage response was too large".into());
    }
    let raw: ModelctlUsage = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("could not decode usage response: {error}"))?;
    Ok(Some(normalize_modelctl(raw)))
}

fn command_from_probe(probe: &ProbeCapture) -> Command {
    let mut command = Command::new(&probe.binary);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in filter_child_env(probe.env.iter().cloned()) {
        command.env(key, value);
    }
    command
}

#[derive(Debug, Deserialize)]
struct ModelctlUsage {
    #[serde(default)]
    providers: Vec<ModelctlProvider>,
}

#[derive(Debug, Deserialize)]
struct ModelctlProvider {
    provider_kind: String,
    name: String,
    #[serde(default)]
    bindings: Vec<ModelctlBinding>,
}

#[derive(Debug, Deserialize)]
struct ModelctlBinding {
    #[serde(default)]
    is_own: bool,
    limit_state: String,
    usage_updated_at_unix_seconds: Option<i64>,
    #[serde(default)]
    usage_windows: Vec<ModelctlWindow>,
}

#[derive(Debug, Deserialize)]
struct ModelctlWindow {
    key: String,
    label: String,
    used_percent: f64,
    resets_at_unix_seconds: Option<i64>,
    status: Option<String>,
    model_scope: Option<String>,
}

fn normalize_modelctl(raw: ModelctlUsage) -> CodeSubscriptionUsage {
    let providers = raw
        .providers
        .into_iter()
        .filter_map(|provider| {
            let mut own_index = 0usize;
            let mut shared_index = 0usize;
            let accounts = provider
                .bindings
                .into_iter()
                .filter(|binding| !binding.usage_windows.is_empty())
                .map(|binding| {
                    let (id, label) = if binding.is_own {
                        own_index += 1;
                        (
                            format!("personal-{own_index}"),
                            if own_index == 1 {
                                "Personal".to_owned()
                            } else {
                                format!("Personal {own_index}")
                            },
                        )
                    } else {
                        shared_index += 1;
                        (
                            format!("shared-{shared_index}"),
                            if shared_index == 1 {
                                "Shared".to_owned()
                            } else {
                                format!("Shared {shared_index}")
                            },
                        )
                    };
                    UsageAccount {
                        id,
                        label,
                        is_own: binding.is_own,
                        state: binding.limit_state,
                        updated_at_unix_seconds: binding.usage_updated_at_unix_seconds,
                        windows: binding
                            .usage_windows
                            .into_iter()
                            .map(|window| UsageWindow {
                                key: window.key,
                                label: window.label,
                                used_percent: window.used_percent,
                                resets_at_unix_seconds: window.resets_at_unix_seconds,
                                status: window.status,
                                model_scope: window.model_scope,
                            })
                            .collect(),
                    }
                })
                .collect::<Vec<_>>();
            (!accounts.is_empty()).then_some(UsageProvider {
                id: provider.provider_kind,
                label: provider.name,
                accounts,
            })
        })
        .collect();
    CodeSubscriptionUsage {
        source: UsageSource::ModelGateway,
        providers,
        diagnostics: Vec::new(),
    }
}

async fn collect_codex() -> Result<Option<UsageProvider>, String> {
    let probe = probe_shell(&HostEnv::from_process(), "codex")
        .await
        .map_err(|error| error.to_string())?;
    let mut command = command_from_probe(&probe);
    command
        .args(["app-server", "--stdio"])
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start app server: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("app server has no stdin")?;
    let stdout = child.stdout.take().ok_or("app server has no stdout")?;
    let request = format!(
        "{}\n{}\n{}\n",
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "tidebreak-usage", "version": env!("CARGO_PKG_VERSION")}, "capabilities": {"experimentalApi": false}}}),
        json!({"method": "initialized"}),
        json!({"id": 2, "method": "account/rateLimits/read", "params": null})
    );
    stdin
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("could not query app server: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("could not query app server: {error}"))?;
    drop(stdin);

    let read = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|error| format!("could not read app server: {error}"))?
        {
            if line.len() > MAX_JSON_BYTES {
                return Err("app-server response was too large".into());
            }
            let value: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Value::as_i64) == Some(2) {
                if let Some(error) = value.get("error") {
                    return Err(format!("rate-limit request failed: {error}"));
                }
                return normalize_codex(value.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        Err("app server closed before answering".into())
    };
    let result = timeout(COMMAND_TIMEOUT, read)
        .await
        .map_err(|_| "rate-limit request timed out".to_owned())?;
    let _ = child.kill().await;
    result
}

fn normalize_codex(result: Value) -> Result<Option<UsageProvider>, String> {
    let raw: CodexRateLimits = serde_json::from_value(result)
        .map_err(|error| format!("could not decode rate limits: {error}"))?;
    let snapshots = raw.rate_limits_by_limit_id.unwrap_or_else(|| {
        let mut snapshots = BTreeMap::new();
        snapshots.insert("codex".into(), raw.rate_limits);
        snapshots
    });
    let mut windows = Vec::new();
    let mut state = "available".to_owned();
    let mut plan = None;
    for (limit_id, snapshot) in snapshots {
        let limit_label = snapshot
            .limit_name
            .clone()
            .unwrap_or_else(|| humanize_limit_id(&limit_id));
        if snapshot.rate_limit_reached_type.is_some()
            || snapshot.spend_control_reached == Some(true)
        {
            state = "limited".into();
        }
        plan = plan.or(snapshot.plan_type.clone());
        if let Some(window) = snapshot.primary {
            windows.push(codex_window(&limit_id, &limit_label, "primary", window));
        }
        if let Some(window) = snapshot.secondary {
            windows.push(codex_window(&limit_id, &limit_label, "secondary", window));
        }
    }
    if windows.is_empty() {
        return Ok(None);
    }
    windows.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(Some(UsageProvider {
        id: "openai".into(),
        label: "Codex".into(),
        accounts: vec![UsageAccount {
            id: "codex-direct".into(),
            label: plan
                .map(|plan| format!("Codex {}", title_case(&plan)))
                .unwrap_or_else(|| "Codex".into()),
            is_own: true,
            state,
            updated_at_unix_seconds: Some(chrono::Utc::now().timestamp()),
            windows,
        }],
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexRateLimits {
    rate_limits: CodexSnapshot,
    rate_limits_by_limit_id: Option<BTreeMap<String, CodexSnapshot>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexSnapshot {
    limit_name: Option<String>,
    primary: Option<CodexWindow>,
    secondary: Option<CodexWindow>,
    plan_type: Option<String>,
    rate_limit_reached_type: Option<String>,
    spend_control_reached: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexWindow {
    used_percent: f64,
    window_duration_mins: Option<i64>,
    resets_at: Option<i64>,
}

fn codex_window(limit_id: &str, limit_label: &str, slot: &str, window: CodexWindow) -> UsageWindow {
    let duration = window
        .window_duration_mins
        .map(format_window_duration)
        .unwrap_or_else(|| title_case(slot));
    let label = if limit_label == "Codex" {
        duration
    } else {
        format!("{duration} · {limit_label}")
    };
    UsageWindow {
        key: format!("{limit_id}-{slot}"),
        label,
        used_percent: window.used_percent,
        resets_at_unix_seconds: window.resets_at,
        status: None,
        model_scope: None,
    }
}

fn format_window_duration(minutes: i64) -> String {
    if minutes % 10_080 == 0 {
        format!("Weekly ({}d)", minutes / 1_440)
    } else if minutes % 1_440 == 0 {
        format!("{}d", minutes / 1_440)
    } else if minutes % 60 == 0 {
        format!("Session ({}h)", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn humanize_limit_id(id: &str) -> String {
    if id == "codex" {
        return "Codex".into();
    }
    title_case(&id.replace(['_', '-'], " "))
}

fn title_case(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn bounded_text(bytes: &[u8], max_chars: usize) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modelctl_usage_keeps_own_and_shared_accounts() {
        let raw: ModelctlUsage = serde_json::from_value(json!({
            "providers": [{
                "provider_kind": "anthropic",
                "name": "Anthropic Direct",
                "bindings": [{
                    "binding_id": "mine",
                    "label": "Personal",
                    "is_own": true,
                    "owner_email": "reader@example.com",
                    "limit_state": "available",
                    "usage_updated_at_unix_seconds": 123,
                    "usage_windows": [{
                        "key": "5h",
                        "label": "Session (5h)",
                        "used_percent": 14.0,
                        "resets_at_unix_seconds": 456,
                        "status": "allowed",
                        "model_scope": null
                    }]
                }, {
                    "binding_id": "shared",
                    "label": "Team",
                    "is_own": false,
                    "owner_email": "owner@example.com",
                    "limit_state": "available",
                    "usage_windows": [{
                        "key": "7d",
                        "label": "Weekly (7d)",
                        "used_percent": 52.0
                    }]
                }]
            }]
        }))
        .expect("fixture");
        let report = normalize_modelctl(raw);
        assert_eq!(report.source, UsageSource::ModelGateway);
        assert_eq!(report.providers[0].accounts.len(), 2);
        assert!(report.providers[0].accounts[0].is_own);
        assert_eq!(
            report.providers[0].accounts[1].windows[0].used_percent,
            52.0
        );
    }

    #[test]
    fn codex_multi_bucket_usage_becomes_readable_windows() {
        let provider = normalize_codex(json!({
            "rateLimits": {"primary": null, "secondary": null},
            "rateLimitsByLimitId": {
                "codex": {
                    "limitName": null,
                    "primary": {"usedPercent": 88, "windowDurationMins": 300, "resetsAt": 456},
                    "secondary": null,
                    "planType": "pro",
                    "rateLimitReachedType": null,
                    "spendControlReached": false
                },
                "codex_spark": {
                    "limitName": "GPT-5 Spark",
                    "primary": {"usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 789},
                    "secondary": null,
                    "planType": "pro",
                    "rateLimitReachedType": null,
                    "spendControlReached": false
                }
            }
        }))
        .expect("decode")
        .expect("provider");
        assert_eq!(provider.accounts[0].label, "Codex Pro");
        assert!(provider.accounts[0]
            .windows
            .iter()
            .any(|window| window.label == "Session (5h)"));
        assert!(provider.accounts[0]
            .windows
            .iter()
            .any(|window| window.label == "Weekly (7d) · GPT-5 Spark"));
    }
}
