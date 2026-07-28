use futures::StreamExt as _;
use reqwest::Response;
use serde::de::DeserializeOwned;

use crate::CodeExecutionError;

/// Bound JSON returned by managed-provider control and command APIs.
pub(crate) async fn decode_bounded_json<T: DeserializeOwned>(
    response: Response,
    provider: &str,
    max_bytes: usize,
) -> Result<T, CodeExecutionError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            CodeExecutionError::Unavailable(format!("{provider} returned an incomplete response"))
        })?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(CodeExecutionError::Unavailable(format!(
                "{provider} returned an oversized response"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        CodeExecutionError::Unavailable(format!("{provider} returned an invalid response"))
    })
}
