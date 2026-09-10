//! Validate the installed engine before registering its runtime identity.

use tidebreak_core::HarnessKind;
use tidebreak_harness::HarnessProbe;

use crate::wire::{EmbeddedEngine, EmbeddedEngineRegistration};

/// Bind the installed binary and its version to Gateway's expected session.
pub fn from_probe(
    expected: &EmbeddedEngine,
    actual: HarnessKind,
    probe: &HarnessProbe,
) -> Result<EmbeddedEngineRegistration, String> {
    if expected.engine != actual || expected.engine_session_id.as_uuid().is_nil() {
        return Err("the installed engine does not match the managed session".into());
    }
    if !probe.found || probe.binary_path.is_none() {
        return Err("the managed engine binary was not found on this image".into());
    }
    let reported = probe
        .version
        .as_deref()
        .unwrap_or("")
        .trim_matches(|c: char| c.is_ascii_whitespace());
    let version = match actual {
        HarnessKind::ClaudeCode => reported.strip_suffix(" (Claude Code)").unwrap_or(reported),
        HarnessKind::Codex => reported.strip_prefix("codex-cli ").unwrap_or(reported),
        _ => return Err("managed engine registration supports claude_code and codex".into()),
    };
    let version = version.strip_prefix('v').unwrap_or(version);
    let core = version.split(['-', '+', '_']).next().unwrap_or("");
    let parts: Vec<_> = core.split('.').collect();
    if version.is_empty()
        || version.len() > 128
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-+_".contains(&b))
        || parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("the managed engine probe did not report a supported installed version".into());
    }
    Ok(EmbeddedEngineRegistration {
        engine_session_id: expected.engine_session_id,
        engine: actual,
        engine_version: version.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::SessionId;

    fn probe(version: &str) -> HarnessProbe {
        HarnessProbe {
            found: true,
            binary_path: Some("/installed/engine".into()),
            version: Some(version.into()),
            authenticated: None,
            stderr: String::new(),
            env: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn registration_keeps_the_persisted_session_and_normalizes_only_known_versions() {
        for (engine, reported, version) in [
            (HarnessKind::ClaudeCode, "2.1.234 (Claude Code)", "2.1.234"),
            (HarnessKind::Codex, "codex-cli 0.147.0", "0.147.0"),
            (HarnessKind::Codex, "v0.147.0-alpha.1", "0.147.0-alpha.1"),
        ] {
            let expected = EmbeddedEngine {
                engine_session_id: SessionId::new(),
                engine,
            };
            let registered = from_probe(&expected, engine, &probe(reported)).unwrap();
            assert_eq!(registered.engine_session_id, expected.engine_session_id);
            assert_eq!(registered.engine_version, version);
        }
    }

    #[test]
    fn registration_refuses_a_different_engine_and_unknown_or_missing_probe() {
        let expected = EmbeddedEngine {
            engine_session_id: SessionId::new(),
            engine: HarnessKind::ClaudeCode,
        };
        assert!(from_probe(&expected, HarnessKind::Codex, &probe("0.147.0")).is_err());
        for version in [
            "",
            "unknown",
            "codex-cli 0.147.0",
            "1.2",
            "1.2.3/other",
            "1.2.3\n4.5.6",
        ] {
            assert!(from_probe(&expected, expected.engine, &probe(version)).is_err());
        }
        let mut missing = probe("1.2.3");
        missing.found = false;
        assert!(from_probe(&expected, expected.engine, &missing).is_err());
        missing.found = true;
        missing.binary_path = None;
        assert!(from_probe(&expected, expected.engine, &missing).is_err());
    }
}
