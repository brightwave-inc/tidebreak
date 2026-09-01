//! Engine-side wiring for a session-scoped inference relay (decision 71).
//!
//! A host that holds the session's provider credentials — Tidebreak's own
//! server, or a sandbox supervisor — serves inference on endpoints it
//! controls and hands each engine child an opaque per-session key instead
//! of a real credential. What is engine knowledge lives here: which
//! variables, flags, and config documents point each engine's own HTTP
//! client at those endpoints. What is host knowledge stays with the
//! caller: where the endpoints are and what the key variable is called.

use tidebreak_core::HarnessKind;

/// Where an engine child's inference goes and how it authenticates.
///
/// The bases are protocol roots *without* the `/v1` version segment; which
/// segment each engine's client appends is engine knowledge and composed
/// by [`spawn_wiring`]. A trailing `/` on a base is tolerated.
pub struct InferenceWiring<'a> {
    /// Root of an endpoint speaking the Anthropic Messages protocol.
    pub anthropic_base: &'a str,
    /// Root of an endpoint speaking the OpenAI Responses protocol (and its
    /// model listing).
    pub openai_base: &'a str,
    /// Environment variable name that carries `key` to engines whose
    /// clients read their credential from the environment (Codex reads it
    /// through its provider config; the Grok adapter consumes it into an
    /// auth file). Callers pass the same name as
    /// [`crate::SessionSpec::relay_key_env`] so the adapters' reserved-key
    /// handling matches the wiring.
    pub key_env: &'a str,
    /// The per-session relay key.
    pub key: &'a str,
}

/// The argv and environment that point one engine child at the relay.
///
/// Claude Code takes a base URL and bearer through its standard variables.
/// Codex takes a custom model provider on the command line; the custom
/// provider also keeps it off its websocket transport, whose vendor-only
/// endpoint produced the reconnect noise hosted sessions logged. Opencode
/// takes a provider override through its JSON config environment variable —
/// one entry per protocol the relay serves, so any session model under
/// those providers rides the caller's grant. Grok takes a custom models
/// endpoint plus the relay key itself; its CLI reads credentials only from
/// an auth file, so the grok adapter materializes the key as a
/// session-scoped `GROK_AUTH_PATH` file and strips the variable from the
/// child's environment.
#[must_use]
pub fn spawn_wiring(
    kind: HarnessKind,
    wiring: &InferenceWiring<'_>,
) -> (Vec<String>, Vec<(String, String)>) {
    let anthropic = wiring.anthropic_base.trim_end_matches('/');
    let openai = wiring.openai_base.trim_end_matches('/');
    let key_env = wiring.key_env;
    let key = wiring.key;
    match kind {
        HarnessKind::ClaudeCode => (
            Vec::new(),
            vec![
                ("ANTHROPIC_BASE_URL".into(), anthropic.to_owned()),
                ("ANTHROPIC_AUTH_TOKEN".into(), key.to_owned()),
            ],
        ),
        HarnessKind::Codex => (
            vec![
                "-c".into(),
                "model_provider=tidebreak".into(),
                "-c".into(),
                "model_providers.tidebreak.name=Tidebreak".into(),
                "-c".into(),
                format!("model_providers.tidebreak.base_url={openai}/v1"),
                "-c".into(),
                format!("model_providers.tidebreak.env_key={key_env}"),
                "-c".into(),
                "model_providers.tidebreak.wire_api=responses".into(),
            ],
            vec![(key_env.to_owned(), key.to_owned())],
        ),
        HarnessKind::Opencode => (
            Vec::new(),
            vec![(
                "OPENCODE_CONFIG_CONTENT".into(),
                opencode_relay_config(anthropic, openai, key),
            )],
        ),
        HarnessKind::Grok => (
            Vec::new(),
            vec![
                (key_env.to_owned(), key.to_owned()),
                ("GROK_MODELS_BASE_URL".into(), format!("{openai}/v1")),
            ],
        ),
        // The in-process engine spawns no child and resolves its inference
        // through the server itself; there is nothing to wire.
        HarnessKind::Internal => (Vec::new(), Vec::new()),
    }
}

/// Opencode's provider override: one entry per protocol the relay serves.
///
/// `OPENCODE_CONFIG_CONTENT` is the JSON config object the CLI merges over
/// its file config, and `options` is the base-URL and key surface every
/// catalog provider accepts. The Anthropic loader posts `{baseURL}/messages`
/// with the key as `x-api-key`; the OpenAI loader posts `{baseURL}/responses`
/// with the key as bearer. `model-gateway` is not a catalog provider — the
/// adapter maps vendor-neutral gateway model ids (deepseek, glm, kimi, ...)
/// to it — so the entry names the OpenAI loader itself: without `npm` the
/// CLI falls back to its OpenAI-compatible loader and posts
/// `{baseURL}/chat/completions`, which the relay does not serve.
/// Non-reasoning OpenAI models would also go to chat completions — pinned
/// against opencode 1.18.x.
fn opencode_relay_config(anthropic: &str, openai: &str, key: &str) -> String {
    serde_json::json!({
        "provider": {
            "anthropic": {
                "options": {
                    "baseURL": format!("{anthropic}/v1"),
                    "apiKey": key,
                },
            },
            "openai": {
                "options": {
                    "baseURL": format!("{openai}/v1"),
                    "apiKey": key,
                },
            },
            "model-gateway": {
                "name": "Model Gateway",
                "npm": "@ai-sdk/openai",
                "options": {
                    "baseURL": format!("{openai}/v1"),
                    "apiKey": key,
                },
            },
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiring<'a>(key: &'a str) -> InferenceWiring<'a> {
        InferenceWiring {
            anthropic_base: "http://127.0.0.1:9101/relay/anthropic/",
            openai_base: "http://127.0.0.1:9101/relay/openai",
            key_env: "TB_RELAY_KEY",
            key,
        }
    }

    #[test]
    fn claude_takes_the_anthropic_root_and_the_key_as_bearer() {
        let (argv, env) = spawn_wiring(HarnessKind::ClaudeCode, &wiring("k1"));
        assert!(argv.is_empty());
        assert_eq!(
            env,
            vec![
                (
                    "ANTHROPIC_BASE_URL".to_owned(),
                    "http://127.0.0.1:9101/relay/anthropic".to_owned()
                ),
                ("ANTHROPIC_AUTH_TOKEN".to_owned(), "k1".to_owned()),
            ]
        );
    }

    #[test]
    fn codex_takes_a_custom_provider_reading_the_named_key_variable() {
        let (argv, env) = spawn_wiring(HarnessKind::Codex, &wiring("k2"));
        assert_eq!(env, vec![("TB_RELAY_KEY".to_owned(), "k2".to_owned())]);
        assert_eq!(argv.len(), 10, "five -c pairs: {argv:?}");
        assert!(argv.contains(&"model_provider=tidebreak".to_owned()));
        assert!(argv.contains(
            &"model_providers.tidebreak.base_url=http://127.0.0.1:9101/relay/openai/v1".to_owned()
        ));
        assert!(argv.contains(&"model_providers.tidebreak.env_key=TB_RELAY_KEY".to_owned()));
        assert!(argv.contains(&"model_providers.tidebreak.wire_api=responses".to_owned()));
    }

    #[test]
    fn opencode_takes_a_config_override_with_one_entry_per_protocol() {
        let (argv, env) = spawn_wiring(HarnessKind::Opencode, &wiring("k3"));
        assert!(argv.is_empty());
        assert_eq!(env.len(), 1, "one config override: {env:?}");
        let (name, config) = &env[0];
        assert_eq!(name, "OPENCODE_CONFIG_CONTENT");
        let config: serde_json::Value = serde_json::from_str(config).unwrap();
        assert_eq!(
            config["provider"]["anthropic"]["options"]["baseURL"],
            "http://127.0.0.1:9101/relay/anthropic/v1",
            "the Anthropic loader posts {{baseURL}}/messages: {config}"
        );
        assert_eq!(config["provider"]["anthropic"]["options"]["apiKey"], "k3");
        assert_eq!(
            config["provider"]["openai"]["options"]["baseURL"],
            "http://127.0.0.1:9101/relay/openai/v1",
            "the OpenAI loader posts {{baseURL}}/responses: {config}"
        );
        assert_eq!(config["provider"]["openai"]["options"]["apiKey"], "k3");
        assert_eq!(
            config["provider"]["model-gateway"]["options"]["baseURL"],
            "http://127.0.0.1:9101/relay/openai/v1",
            "provider-neutral gateway models use the model-gateway provider: {config}"
        );
        assert_eq!(
            config["provider"]["model-gateway"]["options"]["apiKey"],
            "k3"
        );
        assert_eq!(
            config["provider"]["model-gateway"]["npm"], "@ai-sdk/openai",
            "without a loader the CLI defaults to its OpenAI-compatible one and posts \
             chat completions, which the relay does not serve: {config}"
        );
        assert!(
            !config.to_string().contains("TB_RELAY_KEY"),
            "opencode carries the key inside the config, not an env variable: {config}"
        );
    }

    #[test]
    fn grok_takes_the_models_endpoint_and_the_key_under_the_named_variable() {
        let (argv, env) = spawn_wiring(HarnessKind::Grok, &wiring("k4"));
        assert!(argv.is_empty());
        assert_eq!(
            env,
            vec![
                ("TB_RELAY_KEY".to_owned(), "k4".to_owned()),
                (
                    "GROK_MODELS_BASE_URL".to_owned(),
                    "http://127.0.0.1:9101/relay/openai/v1".to_owned()
                ),
            ]
        );
    }
}
