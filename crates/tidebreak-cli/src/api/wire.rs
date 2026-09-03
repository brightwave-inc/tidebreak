//! Client-side decode of the server's JSON.
//!
//! Every shape this crate reads is the server's own type, imported from
//! `tidebreak_server::wire`: one Rust definition serializes on the server and
//! deserializes here, so a renamed field is a compile error in this crate the
//! way it is a type error in the desktop renderer's generated `wire.ts`. The
//! contract those types carry — closed vocabularies, unknown keys rejected, an
//! unknown event type failing its frame — is documented on that module, along
//! with the one record (`McpServerInfo`) that tolerates unknown keys because
//! it flattens its definition.
//!
//! The chat event socket's frames and the REST records (the model catalog,
//! providers, MCP servers, agent runs, and conversation outputs) both come
//! through here; the tests below decode the server's fixtures for each.

pub use tidebreak_core::{
    Chat, PendingPlanApproval, PendingUserQuestions, RendererToolName, ToolActionPreview,
    ToolApprovalKind,
};
pub use tidebreak_server::wire::{
    AgentActivityHistoryItem, ApprovalGrantRung, RendererAgentEvent, RendererChatFrame,
    RendererToolFailure, RendererToolFailureReason, RendererToolStatus,
};
// REST records.
pub use tidebreak_server::wire::{
    AgentRunSnapshot, DeliverablePreview, DeliverableSummary, DeliverablesCatalog, McpServerInfo,
    McpServersInfo, ModelCatalog, OutputRevisionInfo, OutputRevisionsCatalog, ProviderInfo,
    ProvidersList,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Path of the server's chat-frame fixtures, relative to this crate.
    const CHAT_FRAMES: &str = "../tidebreak-server/fixtures/chat-frames.json";

    fn chat_frame_fixtures() -> Vec<(String, serde_json::Value)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CHAT_FRAMES);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&text).expect("the fixture file is a JSON array");
        entries
            .into_iter()
            .map(|entry| {
                let name = entry["name"]
                    .as_str()
                    .expect("every fixture is named")
                    .to_owned();
                (name, entry["frame"].clone())
            })
            .collect()
    }

    /// Every frame the server can send decodes here, byte for byte. The
    /// fixtures are serialized from the server's own types by a test in
    /// `tidebreak-server`, and the renderer's tests read the same file, so
    /// the three decoders cannot drift apart without one of them failing.
    #[test]
    fn every_server_chat_frame_decodes() {
        let fixtures = chat_frame_fixtures();
        assert!(fixtures.len() > 20, "the fixture list looks truncated");
        for (name, frame) in fixtures {
            let decoded: RendererChatFrame = serde_json::from_value(frame.clone())
                .unwrap_or_else(|error| panic!("fixture {name} does not decode: {error}"));
            let again = serde_json::to_value(&decoded).expect("a decoded frame serializes");
            assert_eq!(again, frame, "fixture {name} changed across the round trip");
        }
    }

    /// The CLI reads the cursor from event frames and ignores metadata frames;
    /// both kinds are in the fixtures, so pin which is which.
    #[test]
    fn fixtures_carry_both_frame_kinds() {
        let mut events = 0;
        let mut metadata = 0;
        for (_, frame) in chat_frame_fixtures() {
            match serde_json::from_value::<RendererChatFrame>(frame).expect("decodes") {
                RendererChatFrame::Event(frame) => {
                    assert!(frame.seq > 0);
                    events += 1;
                }
                RendererChatFrame::Metadata(_) => metadata += 1,
            }
        }
        assert!(events > 0 && metadata > 0);
    }

    /// Path of the server's REST record fixtures, relative to this crate.
    const REST_RECORDS: &str = "../tidebreak-server/fixtures/rest-records.json";

    fn rest_record_fixtures() -> Vec<(String, String, serde_json::Value)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(REST_RECORDS);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&text).expect("the fixture file is a JSON array");
        entries
            .into_iter()
            .map(|entry| {
                let field = |key: &str| {
                    entry[key]
                        .as_str()
                        .unwrap_or_else(|| panic!("every fixture has a {key}"))
                        .to_owned()
                };
                (field("name"), field("type"), entry["value"].clone())
            })
            .collect()
    }

    /// Every REST record the server answers with decodes here through the
    /// type the fixture names, and serializes back to the same bytes. The
    /// server's own test renders the file from real values, so a response
    /// field this crate cannot read fails here rather than at a prompt.
    #[test]
    fn every_server_rest_record_decodes() {
        fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(
            name: &str,
            value: &serde_json::Value,
        ) {
            let decoded: T = serde_json::from_value(value.clone())
                .unwrap_or_else(|error| panic!("fixture {name} does not decode: {error}"));
            let again = serde_json::to_value(&decoded).expect("a decoded record serializes");
            assert_eq!(
                &again, value,
                "fixture {name} changed across the round trip"
            );
        }
        let fixtures = rest_record_fixtures();
        assert!(fixtures.len() >= 7, "the fixture list looks truncated");
        for (name, kind, value) in &fixtures {
            match kind.as_str() {
                "ModelCatalog" => round_trip::<ModelCatalog>(name, value),
                "ProvidersList" => round_trip::<ProvidersList>(name, value),
                "McpServersInfo" => round_trip::<McpServersInfo>(name, value),
                "AgentRunSnapshot" => round_trip::<AgentRunSnapshot>(name, value),
                "DeliverablesCatalog" => round_trip::<DeliverablesCatalog>(name, value),
                "DeliverablePreview" => round_trip::<DeliverablePreview>(name, value),
                "OutputRevisionsCatalog" => round_trip::<OutputRevisionsCatalog>(name, value),
                other => panic!("fixture {name} names a type this crate does not read: {other}"),
            }
        }
    }

    /// The fields the CLI prints are reachable on the server's types: the
    /// flattened MCP definition, the typed output timestamp, and the producer
    /// enum a hand-written mirror used to carry as a string.
    #[test]
    fn printed_fields_come_from_the_server_types() {
        let fixtures = rest_record_fixtures();
        let value = |wanted: &str| {
            fixtures
                .iter()
                .find(|(name, _, _)| name == wanted)
                .unwrap_or_else(|| panic!("the {wanted} fixture exists"))
                .2
                .clone()
        };
        let servers: McpServersInfo =
            serde_json::from_value(value("mcp_servers")).expect("decodes");
        let plugin = servers
            .servers
            .iter()
            .find(|server| server.definition.plugin.is_some())
            .expect("one server is plugin-sourced");
        assert_eq!(
            plugin.definition.gateway_endpoint.as_deref(),
            Some("linear")
        );
        assert_eq!(plugin.health.as_str(), "disabled");

        let revisions: OutputRevisionsCatalog =
            serde_json::from_value(value("output_revisions")).expect("decodes");
        let producers: Vec<&str> = revisions
            .revisions
            .iter()
            .map(|revision| revision.produced_by.as_str())
            .collect();
        assert_eq!(producers, ["agent", "backgroundAgent", "user"]);
        assert!(revisions.revisions[0].created_at < revisions.revisions[2].created_at);
    }
}
