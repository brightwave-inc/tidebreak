use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::tool::{input_schema_for, ToolOutput};

/// Parse tool arguments into their typed contract, preserving malformed input
/// as a model-facing result rather than an infrastructure failure.
pub(super) fn parse<T: DeserializeOwned + JsonSchema>(
    arguments: Value,
) -> std::result::Result<T, ToolOutput> {
    serde_json::from_value(arguments).map_err(|error| {
        let schema = serde_json::to_string_pretty(&input_schema_for::<T>())
            .expect("a generated JSON Schema always serializes");
        ToolOutput::error(format!(
            "invalid arguments: {error}\n\nExpected schema:\n{schema}"
        ))
    })
}
