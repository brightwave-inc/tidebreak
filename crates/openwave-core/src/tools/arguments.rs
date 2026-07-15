use serde::Deserialize;
use serde_json::Value;

use crate::tool::ToolOutput;

/// Parse tool arguments into their typed contract, preserving malformed input
/// as a model-facing result rather than an infrastructure failure.
pub(super) fn parse<T: for<'de> Deserialize<'de>>(
    arguments: Value,
) -> std::result::Result<T, ToolOutput> {
    serde_json::from_value(arguments)
        .map_err(|error| ToolOutput::error(format!("invalid arguments: {error}")))
}
