use futures::StreamExt as _;
use reqwest::Response;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

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

/// Download one workspace file, refusing anything beyond the transfer bound.
pub(crate) async fn download_bounded_file(
    response: Response,
    provider: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, CodeExecutionError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| {
            CodeExecutionError::Unavailable(format!("{provider} returned an incomplete file"))
        })?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(CodeExecutionError::WorkspaceFileTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// One encoded `multipart/form-data` upload body.
pub(crate) struct MultipartFile {
    pub(crate) content_type: String,
    pub(crate) body: Vec<u8>,
}

/// Encode a single `file` form part without pulling a multipart dependency
/// into the crate. The boundary is derived from the content and re-derived
/// until the content cannot forge it.
pub(crate) fn multipart_file(filename: &str, content: &[u8]) -> MultipartFile {
    let boundary = multipart_boundary(content);
    let escaped = filename.replace('\\', "\\\\").replace('"', "\\\"");
    let mut body = Vec::with_capacity(content.len() + 256);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{escaped}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    MultipartFile {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        body,
    }
}

fn multipart_boundary(content: &[u8]) -> String {
    let mut counter: u64 = 0;
    loop {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hasher.update(counter.to_le_bytes());
        let digest = hasher.finalize();
        let boundary: String = digest
            .iter()
            .take(16)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let boundary = format!("openwave-{boundary}");
        let marker = format!("--{boundary}");
        if !content
            .windows(marker.len())
            .any(|window| window == marker.as_bytes())
        {
            return boundary;
        }
        counter += 1;
    }
}
