//! Bounded newline-delimited JSON adapter for the broker sidecar process.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::{
    Broker, ControlEnvelope, ControlResponseEnvelope, OperationEnvelope, OperationResponseEnvelope,
    RequestId,
};

/// Enough for one base64-encoded maximum output write plus strict envelope overhead.
pub const MAX_REQUEST_BYTES: usize = 4 * crate::broker::MAX_WRITE_FILE_BYTES / 3 + 128 * 1024;

/// Enough for a base64 [`crate::protocol::MAX_READ_FILE_BINARY_BYTES`] payload
/// plus envelope overhead. Binary reads are the only response that approaches
/// this bound; every other one is orders of magnitude smaller.
pub const MAX_RESPONSE_BYTES: usize =
    4 * crate::protocol::MAX_READ_FILE_BINARY_BYTES / 3 + 512 * 1024;

/// One strictly typed request on the desktop-owned sidecar pipe.
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "channel",
    content = "envelope",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SidecarRequest {
    Control(ControlEnvelope),
    Operation(OperationEnvelope),
}

/// One response for one non-empty input line.
#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "channel",
    content = "envelope",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SidecarResponse {
    Control(ControlResponseEnvelope),
    Operation(OperationResponseEnvelope),
    TransportError(TransportError),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportError {
    pub request_id: Option<RequestId>,
    pub code: TransportErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportErrorCode {
    MalformedRequest,
    RequestTooLarge,
    ResponseTooLarge,
}

/// Serve requests synchronously until EOF. The desktop owns this pipe and
/// awaits exactly one output line for every non-empty input line.
pub fn serve(broker: &Broker, mut input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    while let Some(line) = read_bounded_line(&mut input)? {
        let response = match line {
            BoundedLine::Data(line) if line.is_empty() => continue,
            BoundedLine::Data(line) => dispatch(broker, &line),
            BoundedLine::TooLarge => SidecarResponse::TransportError(TransportError {
                request_id: None,
                code: TransportErrorCode::RequestTooLarge,
                message: "sidecar request exceeded its size limit".to_owned(),
            }),
        };
        let mut encoded = serde_json::to_vec(&response).map_err(io::Error::other)?;
        if encoded.len() > MAX_RESPONSE_BYTES {
            encoded = serde_json::to_vec(&SidecarResponse::TransportError(TransportError {
                request_id: response_request_id(&response),
                code: TransportErrorCode::ResponseTooLarge,
                message: "sidecar response exceeded its size limit".to_owned(),
            }))
            .map_err(io::Error::other)?;
        }
        encoded.push(b'\n');
        output.write_all(&encoded)?;
        output.flush()?;
    }
    Ok(())
}

fn dispatch(broker: &Broker, line: &[u8]) -> SidecarResponse {
    match serde_json::from_slice::<SidecarRequest>(line) {
        Ok(SidecarRequest::Control(envelope)) => {
            SidecarResponse::Control(broker.controller().handle(envelope))
        }
        Ok(SidecarRequest::Operation(envelope)) => {
            SidecarResponse::Operation(broker.operator().handle(envelope))
        }
        Err(_) => SidecarResponse::TransportError(TransportError {
            request_id: None,
            code: TransportErrorCode::MalformedRequest,
            message: "sidecar request was malformed".to_owned(),
        }),
    }
}

fn response_request_id(response: &SidecarResponse) -> Option<RequestId> {
    match response {
        SidecarResponse::Control(envelope) => Some(envelope.request_id),
        SidecarResponse::Operation(envelope) => Some(envelope.request_id),
        SidecarResponse::TransportError(error) => error.request_id,
    }
}

enum BoundedLine {
    Data(Vec<u8>),
    TooLarge,
}

fn read_bounded_line(input: &mut impl BufRead) -> io::Result<Option<BoundedLine>> {
    let mut line = Vec::new();
    let mut too_large = false;
    let mut observed_any = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return if observed_any {
                Ok(Some(if too_large {
                    BoundedLine::TooLarge
                } else {
                    BoundedLine::Data(line)
                }))
            } else {
                Ok(None)
            };
        }
        observed_any = true;
        let through_newline = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        let chunk = &available[..through_newline];
        if !too_large {
            if line.len().saturating_add(chunk.len()) > MAX_REQUEST_BYTES {
                too_large = true;
                line.clear();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let ended = chunk.last() == Some(&b'\n');
        input.consume(through_newline);
        if ended {
            if !too_large {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
            }
            return Ok(Some(if too_large {
                BoundedLine::TooLarge
            } else {
                BoundedLine::Data(line)
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_broker() -> (tempfile::TempDir, Broker) {
        let temp = tempfile::tempdir().unwrap();
        let policy = crate::RootPolicy::for_test(
            temp.path().join("home"),
            Vec::new(),
            vec![temp.path().to_path_buf()],
            Vec::new(),
        );
        (temp, Broker::new(policy))
    }

    #[test]
    fn response_bound_admits_a_maximum_binary_read() {
        let response = SidecarResponse::Operation(crate::OperationResponseEnvelope {
            protocol_version: crate::PROTOCOL_VERSION,
            request_id: RequestId::new(),
            response: crate::Response::Ok(crate::OperationResult::ReadFileBinary(
                crate::ReadFileBinaryResult {
                    content_base64: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        vec![0xffu8; crate::MAX_READ_FILE_BINARY_BYTES],
                    ),
                    bytes: crate::MAX_READ_FILE_BINARY_BYTES,
                },
            )),
        });
        // MAX_RESPONSE_BYTES is derived from the binary bound rather than chosen
        // independently, so a full-size read must never trip ResponseTooLarge.
        let encoded = serde_json::to_vec(&response).unwrap();
        assert!(
            encoded.len() <= MAX_RESPONSE_BYTES,
            "{} exceeds {MAX_RESPONSE_BYTES}",
            encoded.len()
        );
    }

    #[test]
    fn bounded_reader_drains_an_oversized_line_and_resynchronizes() {
        let bytes = [vec![b'x'; MAX_REQUEST_BYTES + 1], b"\nnext\n".to_vec()].concat();
        let mut input = io::BufReader::new(bytes.as_slice());
        assert!(matches!(
            read_bounded_line(&mut input).unwrap(),
            Some(BoundedLine::TooLarge)
        ));
        let Some(BoundedLine::Data(next)) = read_bounded_line(&mut input).unwrap() else {
            panic!("expected next bounded line")
        };
        assert_eq!(next, b"next");
    }

    #[test]
    fn request_union_rejects_unknown_top_level_fields() {
        let request = format!(
            r#"{{"channel":"control","envelope":{{"protocol_version":2,"request_id":"{}","request":{{"control":"hello"}}}},"extra":true}}"#,
            RequestId::new()
        );
        assert!(serde_json::from_str::<SidecarRequest>(&request).is_err());
        let nested = format!(
            r#"{{"channel":"control","envelope":{{"protocol_version":2,"request_id":"{}","request":{{"control":"hello","extra":true}}}}}}"#,
            RequestId::new()
        );
        assert!(serde_json::from_str::<SidecarRequest>(&nested).is_err());
    }

    #[test]
    fn serve_returns_one_safe_error_then_resynchronizes() {
        let (_temp, broker) = test_broker();
        let hello = serde_json::to_vec(&serde_json::json!({
            "channel": "control",
            "envelope": {
                "protocol_version": crate::PROTOCOL_VERSION,
                "request_id": RequestId::new(),
                "request": { "control": "hello" }
            }
        }))
        .unwrap();
        let input = [
            vec![b'x'; MAX_REQUEST_BYTES + 1],
            b"\n   \n".to_vec(),
            hello,
            b"\n".to_vec(),
        ]
        .concat();
        let mut output = Vec::new();
        serve(&broker, io::Cursor::new(input), &mut output).unwrap();
        let responses = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0]["channel"], "transport_error");
        assert_eq!(responses[0]["envelope"]["code"], "request_too_large");
        assert_eq!(responses[1]["channel"], "transport_error");
        assert_eq!(responses[1]["envelope"]["code"], "malformed_request");
        assert_eq!(responses[2]["channel"], "control");
        assert_eq!(responses[2]["envelope"]["response"]["status"], "ok");
    }
}
