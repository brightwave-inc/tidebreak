//! Native Google Gemini Developer API provider.
//!
//! Gemini's GenerateContent protocol differs materially from OpenAI-compatible
//! chat completions: output limits live under `generationConfig`, streamed SSE
//! frames are complete (but partial) responses, and current tool calls carry
//! ids plus opaque thought signatures that must survive a same-route replay.
//! Keeping that conversion here makes the catalog's Gemini rows honest rather
//! than depending on a compatibility layer silently accepting or dropping
//! fields it does not understand.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use futures::stream::{BoxStream, StreamExt};
use serde_json::{json, Value};
use uuid::Uuid;

use tidebreak_core::error::{AgentError, ProviderErrorInfo, Result};
use tidebreak_core::provider::{
    provider_executed_tool_call_text, ChatRequest, ContentBlock, ModelProvider, ProviderEvent,
    ProviderId, RefusalDetails, ResponseFormat, StopReason, ToolChoice, Usage,
};
use tidebreak_core::tool::{strict_json_schema, OptionalProperties};
use tidebreak_core::{ImageAttachments, ReasoningEffort, Role};

use crate::sse::{
    classify_in_band_error, classify_provider_error, frame_data_raw, read_bounded_error_body,
    safe_http_error, SseFramer,
};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";
/// Client-visible identity of this adapter, used in error attribution.
const PROVIDER_NAME: &str = "gemini";
const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Gemini accepts this documented value when an application cannot replay an
/// opaque thought signature. Tidebreak stores provider-neutral tool calls, so
/// the bypass keeps their history portable across a per-chat model switch.
///
/// `thoughtSignature` is a proto `bytes` field, so the JSON mapping carries its
/// base64 encoding rather than these ASCII bytes; see
/// [`thought_signature_bypass`].
const THOUGHT_SIGNATURE_BYPASS: &str = "skip_thought_signature_validator";

/// The wire value for [`THOUGHT_SIGNATURE_BYPASS`]: base64 of its ASCII bytes.
fn thought_signature_bypass() -> &'static str {
    static ENCODED: LazyLock<String> = LazyLock::new(|| BASE64.encode(THOUGHT_SIGNATURE_BYPASS));
    &ENCODED
}

/// A [`ModelProvider`] for native Gemini GenerateContent over the Developer
/// API.
#[derive(Clone)]
pub struct GeminiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl GeminiProvider {
    /// Build a provider using a Gemini Developer API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: crate::http::streaming_client(),
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Override the base URL. This is primarily useful for controlled local
    /// test servers.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn endpoint(&self, model: &str) -> Result<String> {
        if model.is_empty()
            || model.len() > 128
            || !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(AgentError::config("gemini received an invalid model id"));
        }
        let base = self.base_url.trim_end_matches('/');
        Ok(format!(
            "{base}/v1beta/models/{model}:streamGenerateContent?alt=sse"
        ))
    }

    pub async fn transcribe_audio(
        &self,
        model: &str,
        content_type: &str,
        audio_base64: &str,
    ) -> Result<String> {
        let provider = PROVIDER_NAME;
        let endpoint = self
            .endpoint(model)?
            .replace(":streamGenerateContent?alt=sse", ":generateContent");
        let body = json!({
            "contents": [{
                "role": "user",
                "parts": [
                    {"text": "Transcribe this audio verbatim. Return only the transcript text."},
                    {"inlineData": {"mimeType": content_type, "data": audio_base64}}
                ]
            }],
            "generationConfig": {"temperature": 0}
        });
        let request = self
            .client
            .post(endpoint)
            .header("content-type", "application/json")
            .json(&body)
            .header("x-goog-api-key", &self.api_key);
        let response = request.send().await.map_err(|_| {
            AgentError::Provider(format!("{provider} audio transcription request failed"))
        })?;
        if !response.status().is_success() {
            return Err(AgentError::Provider(format!(
                "{provider} audio transcription returned {}",
                response.status().as_u16()
            )));
        }
        let body: Value = response.json().await.map_err(|_| {
            AgentError::Provider(format!(
                "{provider} returned an invalid audio transcription response"
            ))
        })?;
        body.pointer("/candidates/0/content/parts/0/text")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| AgentError::Provider(format!("{provider} returned no audio transcript")))
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("gemini")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let provider = PROVIDER_NAME;
        let body = build_request_json(&req)?;
        let request = self
            .client
            .post(self.endpoint(&req.model)?)
            .header("content-type", "application/json")
            .header("x-goog-api-key", &self.api_key)
            .json(&body);
        let response = request
            .send()
            .await
            // reqwest's display includes the URL, which is not worth echoing
            // into a client-visible error.
            .map_err(|_| AgentError::Provider(format!("{provider} request failed")))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = crate::sse::retry_after_hint(response.headers());
            let body = read_bounded_error_body(response.bytes_stream()).await;
            return Err(classify_gemini_error(
                provider,
                status.as_u16(),
                &body,
                retry_after,
            ));
        }

        let ceiling = crate::http::timeouts().total_stream;
        let stream = async_stream::stream! {
            let bytes = crate::http::with_stream_deadline(response.bytes_stream(), ceiling);
            futures::pin_mut!(bytes);
            let mut framer = SseFramer::default();
            let mut state = StreamState::default();
            while let Some(chunk) = bytes.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    // The transport error's own text stays suppressed —
                    // reqwest's display includes the request URL. Our own
                    // deadline text carries no such thing and is worth
                    // surfacing.
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(error.client_message(provider)),
                        };
                        return;
                    }
                };
                let frames = match framer.push(&chunk) {
                    Ok(frames) => frames,
                    Err(error) => {
                        yield ProviderEvent::Failed {
                            error: ProviderErrorInfo::provider(format!("{provider} {error}")),
                        };
                        return;
                    }
                };
                for frame in frames {
                    let Some(data) = frame_data_raw(&frame) else {
                        continue;
                    };
                    let data = match serde_json::from_str::<Value>(&data) {
                        Ok(data) => data,
                        Err(_) => {
                            yield ProviderEvent::Failed {
                                error: ProviderErrorInfo::provider(
                                    format!("{provider} returned an invalid stream frame"),
                                ),
                            };
                            return;
                        }
                    };
                    for event in normalize(&data, &mut state) {
                        yield event;
                    }
                    if state.terminal {
                        return;
                    }
                }
            }
            let final_frame = match framer.finish() {
                Ok(frame) => frame,
                Err(error) => {
                    yield ProviderEvent::Failed {
                        error: ProviderErrorInfo::provider(format!("{provider} {error}")),
                    };
                    return;
                }
            };
            if let Some(frame) = final_frame {
                if let Some(data) = frame_data_raw(&frame) {
                    let data = match serde_json::from_str::<Value>(&data) {
                        Ok(data) => data,
                        Err(_) => {
                            yield ProviderEvent::Failed {
                                error: ProviderErrorInfo::provider(
                                    format!("{provider} returned an invalid stream frame"),
                                ),
                            };
                            return;
                        }
                    };
                    for event in normalize(&data, &mut state) {
                        yield event;
                    }
                }
            }
            if !state.terminal {
                for event in finish_stream(&mut state) {
                    yield event;
                }
            }
        };
        Ok(Box::pin(stream))
    }
}

/// Build a native GenerateContent request.
///
/// Gemini 3 rejects the deprecated sampling controls, so `temperature` is
/// intentionally not forwarded. The registry only marks Gemini rows as
/// reasoning-capable when this adapter can turn the selected effort into the
/// native `thinkingLevel` shape.
fn build_request_json(req: &ChatRequest) -> Result<Value> {
    let mut generation_config = json!({
        "maxOutputTokens": req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
    });
    if req.reasoning_model {
        if let Some(effort) = req.reasoning_effort {
            generation_config["thinkingConfig"] = json!({
                "thinkingLevel": gemini_thinking_level(effort),
            });
        }
    }

    match &req.response_format {
        Some(ResponseFormat::JsonSchema { name, schema }) => {
            // Gemini's native JSON mode streams the constrained value as
            // ordinary text parts, so there is nothing to renormalize on the way
            // back out.
            let schema =
                strict_json_schema(schema, OptionalProperties::AcceptNull).ok_or_else(|| {
                    AgentError::Provider(format!(
                        "response format {name} has no strict JSON Schema form"
                    ))
                })?;
            generation_config["responseMimeType"] = json!("application/json");
            generation_config["responseSchema"] = gemini_response_schema(&schema)?;
        }
        None => {}
        // `ResponseFormat` is open. A format this adapter has not learned must
        // fail the request rather than stream an unconstrained answer that only
        // looks like a success.
        Some(other) => {
            return Err(AgentError::Provider(format!(
                "gemini cannot enforce response format {other:?}"
            )))
        }
    }

    let grounding = declares_search_grounding(req);
    let mut body = json!({
        "contents": gemini_contents(
            &req.messages,
            &req.images,
            grounding,
            req.provider.as_ref(),
            &req.model,
        )?,
        "generationConfig": generation_config,
    });
    if let Some(system) = &req.system {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    if grounding {
        // Search grounding is a tool entry of its own. Gemini has no cap on how
        // many searches one turn may run, so `VendorWebSearch::max_uses` has
        // nothing to map to here and is deliberately ignored.
        body["tools"] = json!([{ "googleSearch": {} }]);
    }
    if !req.tools.is_empty() {
        let declarations = req
            .tools
            .iter()
            .map(|tool| {
                Ok(json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": gemini_tool_schema(&tool.input_schema)?,
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        body["tools"] = json!([{ "functionDeclarations": declarations }]);
        body["toolConfig"] = json!({
            "functionCallingConfig": gemini_function_calling_config(req.tool_choice.as_ref())?,
        });
    }
    Ok(body)
}

/// Whether this request declares Google Search grounding.
///
/// Grounding and host function tools do not currently coexist on a shape this
/// adapter can send. Google's generateContent documentation puts built-in tools
/// alongside `functionDeclarations` in Preview on Gemini 3 only, and only with
/// `toolConfig.includeServerSideToolInvocations` set: that flag turns on tool
/// context circulation, which drops `AUTO` function-calling mode and requires
/// every later turn to replay the model's own parts verbatim, including part
/// ids and opaque `thoughtSignature` values. Tidebreak captures those parts as
/// route-originated replay state for ordinary host tool calls, but it still
/// stores provider-neutral history so a chat can switch models. On a foreign
/// route the native state flattens away and the documented signature bypass
/// keeps that older history portable.
///
/// Dropping the host's tools to make room for grounding would be worse than
/// ignoring grounding: the turn would lose the agent's whole tool loop. So the
/// search is honored only when the request carries no tools of its own, and the
/// registry keeps `supports_vendor_web_search` false for Gemini rows until the
/// combined shape is actually reachable.
fn declares_search_grounding(req: &ChatRequest) -> bool {
    req.vendor_web_search.is_some() && req.tools.is_empty()
}

fn gemini_function_calling_config(choice: Option<&ToolChoice>) -> Result<Value> {
    Ok(match choice {
        None | Some(ToolChoice::Auto) => json!({ "mode": "AUTO" }),
        Some(ToolChoice::Required) => json!({ "mode": "ANY" }),
        Some(ToolChoice::None) => json!({ "mode": "NONE" }),
        Some(ToolChoice::Tool { name }) => {
            json!({ "mode": "ANY", "allowedFunctionNames": [name] })
        }
        // `ToolChoice` is open so a provider-neutral mode can be added without
        // a breaking change. Silently substituting the model's own judgement
        // for a mode this adapter has not learned would turn "must not call a
        // tool" into "may".
        Some(other) => {
            return Err(AgentError::Provider(format!(
                "gemini cannot express tool choice {other:?}"
            )))
        }
    })
}

/// Translate JSON Schema into the subset Gemini accepts for function parameters.
///
/// Tool schemas are intentionally lossy at this provider boundary: unsupported
/// validation keywords are omitted, while the structural shape and constraints
/// Gemini can express are retained. Building from an allowlist prevents a new
/// JSON Schema keyword from becoming an unknown protobuf field in every turn.
///
/// Every node this emits carries an explicit `type` (optionally alongside
/// `nullable`) or is an `anyOf` whose branches each satisfy the same rule.
/// Gemini rejects the whole request when any node omits its type, so a schema
/// that carries its shape some other way — a `$ref` into `$defs`, a bare
/// `const`, an enum with the type left implicit — has to be resolved here
/// rather than forwarded.
fn gemini_tool_schema(schema: &Value) -> Result<Value> {
    gemini_tool_schema_node(schema, schema, &mut Vec::new())
}

fn gemini_tool_schema_node(
    schema: &Value,
    root: &Value,
    resolving: &mut Vec<String>,
) -> Result<Value> {
    let unsupported = |detail: &str| {
        AgentError::Provider(format!(
            "gemini function parameters cannot express {detail}"
        ))
    };
    let object = schema
        .as_object()
        .ok_or_else(|| unsupported("a non-object schema"))?;

    // Gemini has no `$defs`, so a reference is inlined before translation.
    // Sibling keywords on the referring node (a `description`, a `default`)
    // stay authoritative over the target's own.
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        if resolving.iter().any(|seen| seen == reference) {
            return Err(unsupported(&format!(
                "the recursive schema reference `{reference}`"
            )));
        }
        let target = resolve_schema_ref(root, reference)
            .and_then(Value::as_object)
            .ok_or_else(|| unsupported(&format!("the unresolvable reference `{reference}`")))?;
        let mut inlined = target.clone();
        for (keyword, value) in object {
            if keyword != "$ref" {
                inlined.insert(keyword.clone(), value.clone());
            }
        }
        resolving.push(reference.to_owned());
        let translated = gemini_tool_schema_node(&Value::Object(inlined), root, resolving);
        resolving.pop();
        return translated;
    }

    let mut out = serde_json::Map::new();

    for keyword in GEMINI_SCHEMA_KEYWORDS {
        if let Some(value) = object.get(*keyword) {
            out.insert((*keyword).to_owned(), value.clone());
        }
    }
    // Gemini has no `const`; a single-member enum says the same thing.
    if let Some(value) = object.get("const") {
        out.insert("enum".to_owned(), json!([value]));
    }
    if let Some(format) = object.get("format").and_then(Value::as_str) {
        if GEMINI_FORMATS.contains(&format) {
            out.insert("format".to_owned(), json!(format));
        }
    }
    if let Some(default) = object.get("default") {
        out.insert("default".to_owned(), default.clone());
    }

    let mut nullable = object
        .get("nullable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // A `null` member of an enum is Gemini's `nullable`, not an enum value.
    let denulled = out
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| {
            values
                .iter()
                .any(Value::is_null)
                .then(|| values.iter().filter(|value| !value.is_null()).cloned())
        });
    if let Some(values) = denulled {
        nullable = true;
        out.insert("enum".to_owned(), Value::Array(values.collect()));
    }
    let mut declared: Option<&str> = None;
    let mut any_of = None;
    if let Some(branches) = object.get("anyOf") {
        let branches = branches
            .as_array()
            .ok_or_else(|| unsupported("a non-array `anyOf`"))?;
        let mut concrete = Vec::new();
        for branch in branches {
            if branch.get("type").is_some_and(|value| value == "null") {
                nullable = true;
            } else {
                concrete.push(gemini_tool_schema_node(branch, root, resolving)?);
            }
        }
        if concrete.len() == 1 && object.get("type").is_none() {
            out.extend(
                concrete
                    .pop()
                    .and_then(|branch| branch.as_object().cloned())
                    .ok_or_else(|| unsupported("a non-object `anyOf` branch"))?,
            );
        } else if !concrete.is_empty() {
            any_of = Some(concrete);
        }
    }
    if let Some(value) = object.get("type") {
        let names: Vec<&str> = match value {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names
                .iter()
                .map(Value::as_str)
                .collect::<Option<_>>()
                .ok_or_else(|| unsupported("a non-string type name"))?,
            _ => return Err(unsupported("a non-string type")),
        };
        for name in names {
            if name == "null" {
                nullable = true;
            } else if declared.replace(name).is_some() {
                return Err(unsupported("a union of two concrete types"));
            }
        }
    }

    if let Some(name) = declared {
        let gemini_type =
            gemini_schema_type(name).ok_or_else(|| unsupported(&format!("the type `{name}`")))?;
        out.insert("type".to_owned(), json!(gemini_type));
        if name == "object" {
            let properties = object
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| {
                    properties
                        .iter()
                        .map(|(name, property)| {
                            Ok((
                                name.clone(),
                                gemini_tool_schema_node(property, root, resolving)?,
                            ))
                        })
                        .collect::<Result<serde_json::Map<_, _>>>()
                })
                .transpose()?
                .unwrap_or_default();
            out.insert("properties".to_owned(), Value::Object(properties));
            if let Some(required) = object.get("required") {
                out.insert("required".to_owned(), required.clone());
            }
        }
        if name == "array" {
            let items = object
                .get("items")
                .ok_or_else(|| unsupported("an array with no item schema"))?;
            out.insert(
                "items".to_owned(),
                gemini_tool_schema_node(items, root, resolving)?,
            );
        }
    } else if let Some(branches) = any_of {
        out.insert("anyOf".to_owned(), Value::Array(branches));
    } else if !out.contains_key("type") && !out.contains_key("anyOf") {
        // A schema that only lists its permitted values still owes Gemini a
        // type; JSON Schema leaves it implicit, Gemini rejects the request.
        let inferred = out
            .get("enum")
            .and_then(Value::as_array)
            .ok_or_else(|| unsupported("a schema with no type, enum, or anyOf"))
            .and_then(|values| {
                gemini_enum_type(values)
                    .ok_or_else(|| unsupported("an enum with no single value type"))
            })?;
        out.insert("type".to_owned(), json!(inferred));
    }
    if nullable {
        out.insert("nullable".to_owned(), json!(true));
    }
    Ok(Value::Object(out))
}

/// Resolve a local `#/...` JSON pointer reference against the root schema.
///
/// Only same-document references are supported; this boundary never fetches a
/// remote schema, and every generator this repository uses emits `$defs`
/// pointers.
fn resolve_schema_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    root.pointer(pointer)
}

/// Infer the Gemini type an enum's members share, or `None` when they disagree.
fn gemini_enum_type(values: &[Value]) -> Option<&'static str> {
    let mut inferred: Option<&'static str> = None;
    for value in values {
        let name = match value {
            Value::String(_) => "STRING",
            Value::Bool(_) => "BOOLEAN",
            Value::Number(number) if number.is_i64() || number.is_u64() => "INTEGER",
            Value::Number(_) => "NUMBER",
            _ => return None,
        };
        inferred = Some(match inferred {
            None => name,
            // Integer literals mixed with fractional ones are still numbers.
            Some(seen) if seen == name => seen,
            Some("INTEGER") if name == "NUMBER" => "NUMBER",
            Some("NUMBER") if name == "INTEGER" => "NUMBER",
            Some(_) => return None,
        });
    }
    inferred
}

/// Keywords `responseSchema` understands and that carry over unchanged.
const GEMINI_SCHEMA_KEYWORDS: &[&str] = &[
    "description",
    "enum",
    "pattern",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "minimum",
    "maximum",
];

/// `format` values `responseSchema` recognizes.
///
/// Gemini's list is shorter than JSON Schema's, and it rejects a value it does
/// not know. `format` is an annotation rather than a constraint, so an
/// unrecognized one is dropped — the same trade [`strict_json_schema`] makes.
const GEMINI_FORMATS: &[&str] = &[
    "date-time",
    "date",
    "time",
    "duration",
    "enum",
    "float",
    "double",
    "int32",
    "int64",
];

/// Translate a strict draft 2020-12 schema into Gemini's `responseSchema`.
///
/// `responseSchema` is a subset of the OpenAPI 3.0 Schema object, not JSON
/// Schema: type names are upper-case, nullability is a `nullable` flag rather
/// than a `"null"` member of a type union, and `additionalProperties` does not
/// exist — a Gemini object is closed already.
///
/// Anything outside the subset is an error rather than a dropped keyword, for
/// the same reason as in [`strict_json_schema`]: a constraint we neither sent
/// nor reported is a promise we quietly broke.
fn gemini_response_schema(schema: &Value) -> Result<Value> {
    let unsupported = |detail: &str| {
        AgentError::Provider(format!("gemini responseSchema cannot express {detail}"))
    };
    let object = schema
        .as_object()
        .ok_or_else(|| unsupported("a non-object schema"))?;
    let mut out = serde_json::Map::new();
    for (keyword, value) in object {
        match keyword.as_str() {
            "type" | "properties" | "required" | "items" | "anyOf" => {}
            // Gemini objects reject unknown properties on their own.
            "additionalProperties" => {}
            "format" => {
                if value.as_str().is_some_and(|f| GEMINI_FORMATS.contains(&f)) {
                    out.insert(keyword.clone(), value.clone());
                }
            }
            keyword if GEMINI_SCHEMA_KEYWORDS.contains(&keyword) => {
                out.insert(keyword.to_owned(), value.clone());
            }
            keyword => return Err(unsupported(&format!("the keyword `{keyword}`"))),
        }
    }

    if let Some(branches) = object.get("anyOf").and_then(Value::as_array) {
        let branches = branches
            .iter()
            .map(gemini_response_schema)
            .collect::<Result<Vec<_>>>()?;
        out.insert("anyOf".to_owned(), Value::Array(branches));
    }

    let mut nullable = false;
    let mut declared: Option<&str> = None;
    if let Some(value) = object.get("type") {
        let names: Vec<&str> = match value {
            Value::String(name) => vec![name.as_str()],
            Value::Array(names) => names
                .iter()
                .map(Value::as_str)
                .collect::<Option<_>>()
                .ok_or_else(|| unsupported("a non-string type name"))?,
            _ => return Err(unsupported("a non-string type")),
        };
        for name in names {
            if name == "null" {
                nullable = true;
            } else if declared.replace(name).is_some() {
                // OpenAPI 3.0 has one type per schema, so a genuine union has
                // no representation here.
                return Err(unsupported("a union of two concrete types"));
            }
        }
    }

    if let Some(name) = declared {
        let gemini_type =
            gemini_schema_type(name).ok_or_else(|| unsupported(&format!("the type `{name}`")))?;
        out.insert("type".to_owned(), json!(gemini_type));
        if name == "object" {
            let properties = object
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| unsupported("an object with no declared properties"))?;
            let translated = properties
                .iter()
                .map(|(name, property)| Ok((name.clone(), gemini_response_schema(property)?)))
                .collect::<Result<serde_json::Map<_, _>>>()?;
            out.insert("properties".to_owned(), Value::Object(translated));
            if let Some(required) = object.get("required") {
                out.insert("required".to_owned(), required.clone());
            }
        }
        if name == "array" {
            let items = object
                .get("items")
                .ok_or_else(|| unsupported("an array with no item schema"))?;
            out.insert("items".to_owned(), gemini_response_schema(items)?);
        }
    } else if !out.contains_key("anyOf") {
        return Err(unsupported("a schema with no type"));
    }
    if nullable {
        out.insert("nullable".to_owned(), json!(true));
    }
    Ok(Value::Object(out))
}

fn gemini_schema_type(name: &str) -> Option<&'static str> {
    match name {
        "object" => Some("OBJECT"),
        "array" => Some("ARRAY"),
        "string" => Some("STRING"),
        "integer" => Some("INTEGER"),
        "number" => Some("NUMBER"),
        "boolean" => Some("BOOLEAN"),
        _ => None,
    }
}

fn gemini_thinking_level(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::None => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High | ReasoningEffort::XHigh | ReasoningEffort::Max => "high",
    }
}

/// Translate durable, provider-neutral history to Gemini content messages.
///
/// Gemini requires all responses to the parallel calls in one model turn to
/// form one following user message. The stored transcript keeps tool results as
/// independent messages, so consecutive pure result messages are coalesced.
/// Current Gemini calls and responses both carry the durable call id. The
/// coalescing pass remains load-bearing because Gemini requires every response
/// to parallel calls in one model turn to share the following user message.
/// `rename_client_web_search` rewrites the name of historical client-style
/// `web_search` calls — see [`PRIOR_WEB_SEARCH_TOOL`].
fn gemini_contents(
    messages: &[tidebreak_core::provider::ChatMessage],
    images: &ImageAttachments,
    rename_client_web_search: bool,
    provider: Option<&ProviderId>,
    model: &str,
) -> Result<Vec<Value>> {
    let rename = rename_client_web_search;
    let tool_names: HashMap<&str, &str> = messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolUse { id, name, .. } => {
                Some((id.as_str(), replayed_tool_name(name, rename)))
            }
            _ => None,
        })
        .collect();

    let mut contents = Vec::with_capacity(messages.len());
    for message in messages {
        let role = match message.role {
            Role::Assistant => "model",
            Role::System | Role::Tool | Role::User => "user",
        };
        let mut parts = Vec::with_capacity(message.content.len());
        let mut attached_signature = false;
        for block in &message.content {
            match block {
                ContentBlock::Text { text } => parts.push(json!({ "text": text })),
                ContentBlock::ToolUse { id, name, input } => {
                    let mut part = json!({
                        "functionCall": {
                            "id": id,
                            "name": replayed_tool_name(name, rename),
                            "args": input,
                        },
                    });
                    // Same-route history restores the provider's opaque bytes
                    // exactly. Foreign or legacy history has no valid native
                    // signature, so only that fallback takes the documented
                    // validator bypass.
                    if let Some(signature) = gemini_thought_signature(message, provider, model, id)
                    {
                        part["thoughtSignature"] = signature;
                        attached_signature = true;
                    } else if !attached_signature {
                        part["thoughtSignature"] = json!(thought_signature_bypass());
                        attached_signature = true;
                    }
                    parts.push(part);
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let name = tool_names.get(tool_use_id.as_str()).ok_or_else(|| {
                        AgentError::Provider(
                            "gemini tool result has no matching function call".to_string(),
                        )
                    })?;
                    // Its `response` struct expresses failure with an `error`
                    // key; the id and name together match the original call.
                    let response = if *is_error {
                        json!({ "error": content })
                    } else {
                        json!({ "output": content })
                    };
                    parts.push(json!({
                        "functionResponse": {
                            "id": tool_use_id,
                            "name": name,
                            "response": response,
                        },
                    }));
                }
                ContentBlock::Image { image } => {
                    let data = images.get(image.blob_id).ok_or_else(|| {
                        AgentError::Provider(format!(
                            "image attachment {} has no hydrated bytes",
                            image.blob_id
                        ))
                    })?;
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": data.media_type().as_str(),
                            "data": BASE64.encode(data.bytes()),
                        },
                    }));
                }
                // Gemini has no part for a call another provider ran
                // server-side, and its own grounding is a different mechanism
                // entirely. One line of text keeps the fact of the search.
                ContentBlock::ProviderExecutedToolCall {
                    name,
                    input,
                    output,
                    is_error,
                    replay: _,
                } => parts.push(json!({
                    "text": provider_executed_tool_call_text(name, input, output, *is_error),
                })),
                // `ContentBlock` is deliberately open for new provider-neutral
                // variants. Dropping one here would silently change the model
                // prompt, so make the adapter fail until that new variant gains
                // an explicit Gemini representation.
                _ => {
                    return Err(AgentError::Provider(
                        "gemini cannot encode an unsupported content block".to_string(),
                    ));
                }
            }
        }
        contents.push(json!({ "role": role, "parts": parts }));
    }

    let function_responses_only = |content: &Value| {
        content.get("role").and_then(Value::as_str) == Some("user")
            && content
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    !parts.is_empty()
                        && parts
                            .iter()
                            .all(|part| part.get("functionResponse").is_some())
                })
    };
    let mut merged = Vec::with_capacity(contents.len());
    for content in contents {
        if function_responses_only(&content) && merged.last().is_some_and(function_responses_only) {
            let previous = merged.last_mut().expect("last was checked");
            let parts = content
                .get("parts")
                .and_then(Value::as_array)
                .expect("function responses have parts")
                .clone();
            previous
                .get_mut("parts")
                .and_then(Value::as_array_mut)
                .expect("function responses have mutable parts")
                .extend(parts);
        } else {
            merged.push(content);
        }
    }
    Ok(merged)
}

/// The exact thought signature captured for `tool_use_id`, but only when the
/// message state came from this provider+model route.
fn gemini_thought_signature(
    message: &tidebreak_core::provider::ChatMessage,
    provider: Option<&ProviderId>,
    model: &str,
    tool_use_id: &str,
) -> Option<Value> {
    message
        .reasoning
        .replayable_for(provider, model)
        .iter()
        .find(|part| part.pointer("/functionCall/id").and_then(Value::as_str) == Some(tool_use_id))
        .and_then(|part| part.get("thoughtSignature"))
        .filter(|signature| signature.is_string())
        .cloned()
}

#[derive(Default)]
struct StreamState {
    next_tool_index: u32,
    seen_tool_ids: HashSet<String>,
    usage: Option<Usage>,
    saw_tool_call: bool,
    terminal: bool,
    reported_grounding: bool,
}

/// Convert one complete Gemini response frame into provider-neutral events.
fn normalize(data: &Value, state: &mut StreamState) -> Vec<ProviderEvent> {
    if state.terminal {
        return Vec::new();
    }
    if let Some(error) = data.get("error") {
        state.terminal = true;
        let error = classify_in_band_error(PROVIDER_NAME, error);
        return vec![ProviderEvent::Failed {
            error: ProviderErrorInfo::from_error(&error),
        }];
    }
    if let Some(block_reason) = data
        .get("promptFeedback")
        .and_then(|feedback| feedback.get("blockReason"))
        .and_then(Value::as_str)
    {
        state.terminal = true;
        return vec![ProviderEvent::Refusal {
            details: refusal_details(block_reason),
        }];
    }

    if let Some(metadata) = data.get("usageMetadata") {
        state.usage = Some(gemini_usage(metadata));
    }

    let Some(candidate) = data
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
    else {
        return Vec::new();
    };

    let mut events = Vec::new();
    if let Some(parts) = candidate
        .get("content")
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
    {
        for part in parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    events.push(ProviderEvent::ReasoningDelta {
                        text: text.to_string(),
                    });
                }
                continue;
            }
            if let Some(text) = part
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                events.push(ProviderEvent::TextDelta {
                    text: text.to_string(),
                });
            }
            if let Some(call) = part.get("functionCall") {
                let Some(name) = call
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                else {
                    state.terminal = true;
                    events.push(ProviderEvent::Failed {
                        error: ProviderErrorInfo::provider(format!(
                            "{} returned a malformed function call",
                            PROVIDER_NAME
                        )),
                    });
                    return events;
                };
                let id = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    // Current Gemini 3 endpoints supply ids. Synthesizing for
                    // older or proxy responses still keeps the agent's own
                    // call/result pairing well-formed.
                    .unwrap_or_else(|| format!("gemini-{}", Uuid::new_v4()));
                if !state.seen_tool_ids.insert(id.clone()) {
                    continue;
                }
                let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                if !args.is_object() {
                    state.terminal = true;
                    events.push(ProviderEvent::Failed {
                        error: ProviderErrorInfo::provider(format!(
                            "{} returned non-object function arguments",
                            PROVIDER_NAME
                        )),
                    });
                    return events;
                }
                let index = state.next_tool_index;
                state.next_tool_index = state.next_tool_index.saturating_add(1);
                state.saw_tool_call = true;
                if part.get("thoughtSignature").is_some_and(Value::is_string) {
                    events.push(ProviderEvent::ReasoningBlock {
                        data: json!({
                            "functionCall": {"id": id.clone()},
                            "thoughtSignature": part["thoughtSignature"].clone(),
                        }),
                    });
                }
                events.push(ProviderEvent::ToolCallStarted {
                    index,
                    id,
                    name: name.to_string(),
                });
                events.push(ProviderEvent::ToolCallArgsDelta {
                    index,
                    fragment: serde_json::to_string(&args).expect("JSON values serialize"),
                });
            }
        }
    }

    // Grounding metadata rides on the candidate rather than on a part, and a
    // stream repeats it once the searches are done, so the first usable one
    // wins and later chunks add nothing.
    if !state.reported_grounding {
        if let Some(event) = candidate
            .get("groundingMetadata")
            .and_then(grounding_search_event)
        {
            state.reported_grounding = true;
            events.push(event);
        }
    }

    if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
        finish_candidate(reason, state, &mut events);
    }
    events
}

/// The name a search Gemini ran server-side surfaces under to the host.
///
/// Deliberately the host's own tool name, so one renderer draws a grounded
/// search and a client-executed one alike.
const VENDOR_WEB_SEARCH_TOOL: &str = "web_search";

/// The name a historical client-side `web_search` call is replayed under once
/// the request declares Google Search grounding.
///
/// Gemini correlates a `functionCall` to its `functionResponse` by name, with
/// no ids on either side, so a replayed client call named `web_search` sitting
/// beside a declared `googleSearch` tool is ambiguous exactly where the model
/// has the least to disambiguate with. Renaming keeps the history well-formed
/// and readable; the matching response is renamed with it.
const PRIOR_WEB_SEARCH_TOOL: &str = "web_search_prior";

fn replayed_tool_name(name: &str, rename_client_web_search: bool) -> &str {
    if rename_client_web_search && name == VENDOR_WEB_SEARCH_TOOL {
        PRIOR_WEB_SEARCH_TOOL
    } else {
        name
    }
}

/// Turn a candidate's `groundingMetadata` into one provider-executed search
/// call, or nothing when it carries neither queries nor sources.
///
/// Grounding is not a tool-call part: the searches happen inside the turn and
/// the whole record arrives as candidate metadata, in practice on the last
/// chunk of a stream. So this reads whichever chunk carries it and reports the
/// searches as a single completed call — there is no per-search error surface
/// to report, which is why an unusable metadata object yields nothing rather
/// than a failed call.
fn grounding_search_event(metadata: &Value) -> Option<ProviderEvent> {
    /// Same cap the host tool applies, so a grounded result set cannot put
    /// more into context than the host's own search would.
    const MAX_RESULTS: usize = tidebreak_core::MAX_WEB_SEARCH_RESULTS;
    /// Ceiling on the Search Suggestions markup carried back to the host. It
    /// is a small widget in practice; anything past this is dropped whole
    /// rather than truncated, since half an HTML document renders as garbage.
    const MAX_ATTRIBUTION_HTML_BYTES: usize = 64 * 1024;

    let queries: Vec<&str> = metadata
        .get("webSearchQueries")
        .and_then(Value::as_array)
        .map(|queries| {
            queries
                .iter()
                .filter_map(Value::as_str)
                .filter(|query| !query.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let mut seen = HashSet::new();
    let results: Vec<Value> = metadata
        .get("groundingChunks")
        .and_then(Value::as_array)
        .map(|chunks| {
            chunks
                .iter()
                .filter_map(|chunk| chunk.get("web"))
                .filter_map(|web| {
                    let url = str_at(web, "uri");
                    (!url.is_empty() && seen.insert(url.clone())).then(|| {
                        json!({
                            "url": url,
                            "title": str_at(web, "title"),
                            // Grounding returns no excerpt; the field stays
                            // present so the shape does not vary by provider.
                            "snippet": "",
                        })
                    })
                })
                .take(MAX_RESULTS)
                .collect()
        })
        .unwrap_or_default();

    if queries.is_empty() && results.is_empty() {
        return None;
    }

    // One record covers every search the turn ran, so the queries join into
    // the single `query` field the host tool and the prose replay both read.
    let mut output = json!({ "provider": "gemini", "results": results });
    let attribution = metadata
        .get("searchEntryPoint")
        .map(|entry| str_at(entry, "renderedContent"))
        .filter(|html| !html.is_empty() && html.len() <= MAX_ATTRIBUTION_HTML_BYTES);
    if let Some(html) = attribution {
        // Carried through unsanitized and unrendered: Google requires the
        // Search Suggestions widget to be displayed, and the surface that
        // displays it decides how to make that markup safe.
        output["attribution_html"] = json!(html);
    }
    Some(ProviderEvent::ProviderExecutedToolCall {
        name: VENDOR_WEB_SEARCH_TOOL.to_string(),
        input: json!({ "query": queries.join("; ") }),
        output,
        is_error: false,
        replay: None,
    })
}

fn str_at(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn finish_stream(state: &mut StreamState) -> Vec<ProviderEvent> {
    let mut events = Vec::new();
    emit_usage(state, &mut events);
    events.push(ProviderEvent::Stop {
        reason: if state.saw_tool_call {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        },
    });
    state.terminal = true;
    events
}

fn finish_candidate(reason: &str, state: &mut StreamState, events: &mut Vec<ProviderEvent>) {
    emit_usage(state, events);
    state.terminal = true;
    match reason {
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" => {
            events.push(ProviderEvent::Refusal {
                details: refusal_details(reason),
            });
        }
        "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" => {
            events.push(ProviderEvent::Failed {
                error: ProviderErrorInfo::provider(format!(
                    "{} rejected a function call",
                    PROVIDER_NAME
                )),
            });
        }
        "MAX_TOKENS" => events.push(ProviderEvent::Stop {
            reason: StopReason::MaxTokens,
        }),
        // A Gemini tool call can report `STOP`, so tools must win over the
        // provider's finish code when the normalized stream contains one.
        _ => events.push(ProviderEvent::Stop {
            reason: if state.saw_tool_call {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            },
        }),
    }
}

fn emit_usage(state: &mut StreamState, events: &mut Vec<ProviderEvent>) {
    if let Some(usage) = state.usage.take() {
        events.push(ProviderEvent::Usage(usage));
    }
}

fn gemini_usage(metadata: &Value) -> Usage {
    let prompt = u64_at(metadata, "promptTokenCount");
    let cached = u64_at(metadata, "cachedContentTokenCount");
    Usage {
        // Gemini's prompt count includes the cached portion.
        input_tokens: saturating_u32(prompt.saturating_sub(cached)),
        // Thoughts are billed output in addition to candidate tokens.
        output_tokens: saturating_u32(
            u64_at(metadata, "candidatesTokenCount")
                .saturating_add(u64_at(metadata, "thoughtsTokenCount")),
        ),
        cache_read_input_tokens: saturating_u32(cached),
        cache_creation_input_tokens: 0,
    }
}

fn u64_at(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn refusal_details(reason: &str) -> RefusalDetails {
    let category = reason
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' => char::from(byte.to_ascii_lowercase()),
            b'a'..=b'z' | b'0'..=b'9' => char::from(byte),
            _ => '_',
        })
        .collect::<String>();
    RefusalDetails::from_category(Some(&category))
}

fn classify_gemini_error(
    provider: &str,
    status: u16,
    body: &str,
    retry_after: Option<std::time::Duration>,
) -> AgentError {
    let prompt_too_long = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| value.get("error").cloned().or(Some(value)))
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|message| {
            let message = message.to_ascii_lowercase();
            message.contains("too many tokens")
                || message.contains("input token count")
                || message.contains("maximum number of tokens")
        });
    if prompt_too_long {
        return AgentError::PromptTooLong(safe_http_error(provider, status, body));
    }
    classify_provider_error(provider, status, body, retry_after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::provider::{ChatMessage, MessageReasoning, ReasoningOrigin};
    use tidebreak_core::tool::{Tool, ToolSpec};
    use tidebreak_core::{
        ask_user_questions_tool_spec, create_app_tool_spec, exit_plan_mode_tool_spec,
        import_connected_file_tool_spec, list_connected_folders_tool_spec, list_folder_tool_spec,
        read_connected_file_tool_spec, request_folder_access_tool_spec,
        sandbox_folder_access_proposal_tool_spec, sandbox_read_delegated_file_tool_spec,
        sandbox_web_search_tool_spec, spawn_sandbox_agent_tool_spec, wait_for_agents_tool_spec,
        web_extract_tool_spec, web_search_tool_spec, write_output_to_connected_folder_tool_spec,
        ListDir, ReadFile, WriteFile,
    };

    fn request(messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            provider: Some(ProviderId::new("gemini")),
            model: "gemini-3.6-flash".into(),
            reasoning_model: true,
            system: Some("be brief".into()),
            messages,
            tools: vec![ToolSpec {
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema: json!({"type": "object"}),
            }],
            max_tokens: Some(65_536),
            temperature: Some(0.2),
            reasoning_effort: Some(ReasoningEffort::High),
            images: ImageAttachments::new(),
            ..Default::default()
        }
    }

    #[test]
    fn request_uses_native_output_cap_and_current_tool_shape() {
        let body = build_request_json(&request(vec![ChatMessage::text(Role::User, "hi")])).unwrap();
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 65_536);
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert!(body.get("temperature").is_none());
        for unsupported in ["temperature", "topP", "topK", "candidateCount"] {
            assert!(body["generationConfig"].get(unsupported).is_none());
        }
        assert!(body["generationConfig"]["thinkingConfig"]
            .get("thinkingBudget")
            .is_none());
        assert!(body["generationConfig"].get("max_tokens").is_none());
    }

    #[test]
    fn low_reasoning_request_uses_low_not_minimal() {
        // The host policy raises a stored `none` to `low` for Gemini 3.1 Pro
        // Preview;
        // keep that reconciled level intact on the native wire.
        let mut req = request(vec![ChatMessage::text(Role::User, "hi")]);
        req.model = "gemini-3.1-pro-preview".into();
        req.reasoning_effort = Some(ReasoningEffort::Low);

        let body = build_request_json(&req).unwrap();
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "low"
        );
        assert!(!body.to_string().contains("minimal"));
    }

    #[test]
    fn all_tool_schemas_translate_to_geminis_supported_subset() {
        let mut req = request(vec![ChatMessage::text(Role::User, "hi")]);
        req.tools = vec![
            spawn_sandbox_agent_tool_spec(),
            wait_for_agents_tool_spec(),
            web_search_tool_spec(),
            web_extract_tool_spec(),
            sandbox_web_search_tool_spec(),
            sandbox_read_delegated_file_tool_spec(),
            request_folder_access_tool_spec(),
            sandbox_folder_access_proposal_tool_spec(),
            list_connected_folders_tool_spec(),
            list_folder_tool_spec(),
            read_connected_file_tool_spec(),
            import_connected_file_tool_spec(),
            write_output_to_connected_folder_tool_spec(),
            exit_plan_mode_tool_spec(),
            ask_user_questions_tool_spec(),
            create_app_tool_spec(),
            ReadFile.spec(),
            ListDir.spec(),
            WriteFile.spec(),
        ];

        let body = build_request_json(&req).unwrap();
        let declarations = body["tools"][0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(declarations.len(), req.tools.len());

        fn assert_supported(schema: &Value) {
            const SUPPORTED: &[&str] = &[
                "type",
                "format",
                "description",
                "nullable",
                "enum",
                "items",
                "properties",
                "required",
                "minimum",
                "maximum",
                "minItems",
                "maxItems",
                "minLength",
                "maxLength",
                "pattern",
                "anyOf",
                "default",
            ];
            let object = schema.as_object().expect("Gemini schema must be an object");
            // Gemini rejects the whole request when any node omits its type,
            // so the translator must resolve a shape JSON Schema left implicit
            // rather than forward it.
            assert!(
                object.contains_key("type") || object.contains_key("anyOf"),
                "Gemini schema node must declare a type or anyOf: {schema}"
            );
            for (keyword, value) in object {
                assert!(
                    SUPPORTED.contains(&keyword.as_str()),
                    "unsupported Gemini schema keyword `{keyword}` in {schema}"
                );
                if keyword == "type" {
                    assert!(value.is_string(), "Gemini type must be scalar in {schema}");
                }
            }
            if let Some(properties) = object.get("properties").and_then(Value::as_object) {
                for property in properties.values() {
                    assert_supported(property);
                }
            }
            if let Some(items) = object.get("items") {
                assert_supported(items);
            }
            if let Some(branches) = object.get("anyOf").and_then(Value::as_array) {
                for branch in branches {
                    assert_supported(branch);
                }
            }
        }

        for declaration in declarations {
            assert_supported(&declaration["parameters"]);
        }
        let wait = declarations
            .iter()
            .find(|declaration| declaration["name"] == "wait_for_agents")
            .unwrap();
        assert!(wait["parameters"]["properties"]["agent_ids"]
            .get("uniqueItems")
            .is_none());
        let nullable = gemini_tool_schema(&json!({ "type": ["string", "null"] })).unwrap();
        assert_eq!(nullable, json!({ "type": "STRING", "nullable": true }));
    }

    /// Reproduces the shapes a derived tool schema reaches Gemini with that
    /// carry no type of their own: a `$defs` reference, a bare `const`, and an
    /// enum whose type is left implicit. Gemini rejects the entire request for
    /// any one of them.
    #[test]
    fn derived_schema_shapes_without_a_type_are_resolved() {
        let translated = gemini_tool_schema(&json!({
            "type": "object",
            "$defs": {
                "Capability": { "enum": ["read_files", "write_files"] },
                "Version": { "const": 2 },
            },
            "properties": {
                "capabilities": {
                    "type": "array",
                    "items": { "$ref": "#/$defs/Capability" },
                    "uniqueItems": true,
                },
                "version": { "$ref": "#/$defs/Version", "description": "Wire version." },
            },
            "required": ["capabilities"],
        }))
        .unwrap();
        assert_eq!(
            translated["properties"]["capabilities"]["items"],
            json!({ "type": "STRING", "enum": ["read_files", "write_files"] })
        );
        assert_eq!(
            translated["properties"]["version"],
            json!({ "type": "INTEGER", "enum": [2], "description": "Wire version." })
        );

        let cyclic = gemini_tool_schema(&json!({
            "$defs": { "Node": { "type": "object", "properties": { "next": { "$ref": "#/$defs/Node" } } } },
            "$ref": "#/$defs/Node",
        }))
        .unwrap_err();
        assert!(
            cyclic.to_string().contains("recursive schema reference"),
            "expected a descriptive cycle error, got {cyclic}"
        );
    }

    #[test]
    fn a_constrained_request_translates_the_schema_to_the_openapi_subset() {
        let mut req = request(vec![ChatMessage::text(Role::User, "hi")]);
        req.tools.clear();
        req.response_format = Some(ResponseFormat::JsonSchema {
            name: "note".into(),
            schema: json!({
                "type": "object",
                "properties": {
                    "items": { "type": "array", "items": { "type": "string" } },
                    "note": { "type": "string", "description": "Optional note." },
                    "count": { "type": "integer", "format": "uint16", "minimum": 0 },
                },
                "required": ["items", "count"],
            }),
        });
        let body = build_request_json(&req).unwrap();
        assert_eq!(
            body["generationConfig"]["responseMimeType"],
            "application/json"
        );
        // Upper-case type names, `nullable` in place of a `"null"` type member,
        // no `additionalProperties`, and no generator-shaped `format`.
        assert_eq!(
            body["generationConfig"]["responseSchema"],
            json!({
                "type": "OBJECT",
                "properties": {
                    "items": { "type": "ARRAY", "items": { "type": "STRING" } },
                    "note": {
                        "type": "STRING",
                        "nullable": true,
                        "description": "Optional note.",
                    },
                    "count": { "type": "INTEGER", "minimum": 0 },
                },
                "required": ["count", "items", "note"],
            })
        );

        // OpenAPI 3.0 schemas carry one concrete type, so a genuine union has no
        // translation and must not go out as an unconstrained request.
        assert!(gemini_response_schema(&json!({ "type": ["string", "integer"] })).is_err());
    }

    #[test]
    fn tool_calls_and_results_preserve_ids_and_same_route_signatures() {
        let messages = vec![
            ChatMessage {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "call_one".into(),
                        name: "read_file".into(),
                        input: json!({"path": "one"}),
                    },
                    ContentBlock::ToolUse {
                        id: "call_two".into(),
                        name: "read_file".into(),
                        input: json!({"path": "two"}),
                    },
                ],
                reasoning: MessageReasoning::captured(
                    ReasoningOrigin {
                        provider: Some(ProviderId::new("gemini")),
                        model: "gemini-3.6-flash".into(),
                    },
                    vec![
                        json!({
                            "functionCall": {"id": "call_one"},
                            "thoughtSignature": "b3BhcXVlLXNpZ25hdHVyZS0x",
                        }),
                        json!({
                            "functionCall": {"id": "call_two"},
                            "thoughtSignature": "b3BhcXVlLXNpZ25hdHVyZS0y",
                        }),
                    ],
                ),
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_one".into(),
                    content: "one".into(),
                    is_error: false,
                }],
                reasoning: MessageReasoning::default(),
            },
            ChatMessage {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_two".into(),
                    content: "boom".into(),
                    is_error: true,
                }],
                reasoning: MessageReasoning::default(),
            },
        ];
        let body = build_request_json(&request(messages)).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "model");

        let calls = contents[0]["parts"].as_array().unwrap();
        assert_eq!(calls[0]["functionCall"]["id"], "call_one");
        assert_eq!(calls[1]["functionCall"]["id"], "call_two");
        assert_eq!(calls[0]["functionCall"]["args"]["path"], "one");
        assert_eq!(calls[1]["functionCall"]["args"]["path"], "two");
        assert_eq!(calls[0]["thoughtSignature"], "b3BhcXVlLXNpZ25hdHVyZS0x");
        assert_eq!(calls[1]["thoughtSignature"], "b3BhcXVlLXNpZ25hdHVyZS0y");

        let responses = contents[1]["parts"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["functionResponse"]["id"], "call_one");
        assert_eq!(responses[1]["functionResponse"]["id"], "call_two");
        assert!(responses
            .iter()
            .all(|response| response["functionResponse"]["name"] == "read_file"));
        assert_eq!(
            responses[0]["functionResponse"]["response"]["output"],
            "one"
        );
        assert_eq!(
            responses[1]["functionResponse"]["response"]["error"],
            "boom"
        );
        assert!(responses[1]["functionResponse"]["response"]
            .get("is_error")
            .is_none());
    }

    #[test]
    fn gemini_replays_its_signature_and_route_switches_use_the_legacy_bypass() {
        let tool_message = |origin_provider: &str| ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_one".into(),
                name: "read_file".into(),
                input: json!({"path": "one"}),
            }],
            reasoning: MessageReasoning::captured(
                ReasoningOrigin {
                    provider: Some(ProviderId::new(origin_provider)),
                    model: "gemini-3.6-flash".into(),
                },
                vec![json!({
                    "functionCall": {"id": "call_one"},
                    "thoughtSignature": "Z2VtaW5pLW9wYXF1ZS1zaWduYXR1cmU=",
                })],
            ),
        };

        let mut same_route = request(vec![tool_message("gemini")]);
        same_route.provider = Some(ProviderId::new("gemini"));
        let body = build_request_json(&same_route).unwrap();
        assert_eq!(
            body["contents"][0]["parts"][0]["thoughtSignature"],
            "Z2VtaW5pLW9wYXF1ZS1zaWduYXR1cmU="
        );

        let mut switched = request(vec![tool_message("model_gateway")]);
        switched.provider = Some(ProviderId::new("gemini"));
        let body = build_request_json(&switched).unwrap();
        let signature = body["contents"][0]["parts"][0]["thoughtSignature"]
            .as_str()
            .unwrap();
        assert_eq!(
            String::from_utf8(BASE64.decode(signature).unwrap()).unwrap(),
            THOUGHT_SIGNATURE_BYPASS
        );

        let legacy = request(vec![ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_one".into(),
                name: "read_file".into(),
                input: json!({"path": "one"}),
            }],
            reasoning: MessageReasoning::default(),
        }]);
        let body = build_request_json(&legacy).unwrap();
        assert_eq!(
            String::from_utf8(
                BASE64
                    .decode(
                        body["contents"][0]["parts"][0]["thoughtSignature"]
                            .as_str()
                            .unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap(),
            THOUGHT_SIGNATURE_BYPASS
        );
    }

    // ── Image blocks ───────────────────────────────────────────────

    fn png_ref(blob: u128) -> tidebreak_core::ImageRef {
        tidebreak_core::ImageRef {
            blob_id: Uuid::from_u128(blob),
            media_type: tidebreak_core::ImageMediaType::Png,
            width: 800,
            height: 600,
            byte_len: 3,
        }
    }

    #[test]
    fn image_blocks_become_hydrated_inline_data() {
        let image = png_ref(1);
        let mut req = request(vec![ChatMessage {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "what is this?".into(),
                },
                ContentBlock::Image { image },
            ],
            reasoning: MessageReasoning::default(),
        }]);
        req.images.insert(
            image.blob_id,
            tidebreak_core::ImageData::new(tidebreak_core::ImageMediaType::Png, vec![1, 2, 3]),
        );

        let body = build_request_json(&req).unwrap();
        let parts = body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], BASE64.encode([1, 2, 3]));
    }

    #[test]
    fn unhydrated_images_fail_instead_of_being_dropped() {
        let req = request(vec![ChatMessage {
            role: Role::User,
            content: vec![ContentBlock::Image { image: png_ref(9) }],
            reasoning: MessageReasoning::default(),
        }]);
        let err = build_request_json(&req).unwrap_err();
        assert!(err.to_string().contains("no hydrated bytes"), "{err}");
    }

    fn run(chunks: &[Value]) -> Vec<ProviderEvent> {
        let mut state = StreamState::default();
        let mut out = chunks
            .iter()
            .flat_map(|chunk| normalize(chunk, &mut state))
            .collect::<Vec<_>>();
        if !state.terminal {
            out.extend(finish_stream(&mut state));
        }
        out
    }

    #[test]
    fn normalizes_partial_responses_usage_and_parallel_tool_calls() {
        let out = run(&[
            json!({"candidates":[{"content":{"parts":[
                {"thought": true, "text":"considering"},
                {"text":"I'll inspect it."},
                {"functionCall":{"id":"call_1", "name":"read_file", "args":{"path":"a"}}, "thoughtSignature":"c2lnbmF0dXJlLW9uZQ=="},
                {"functionCall":{"id":"call_2", "name":"read_file", "args":{"path":"b"}}, "thoughtSignature":"c2lnbmF0dXJlLXR3bw=="}
            ]}}]}),
            json!({"candidates":[{"finishReason":"STOP"}], "usageMetadata": {
                "promptTokenCount": 10,
                "cachedContentTokenCount": 4,
                "candidatesTokenCount": 2,
                "thoughtsTokenCount": 3
            }}),
        ]);
        assert_eq!(
            out,
            vec![
                ProviderEvent::ReasoningDelta {
                    text: "considering".into()
                },
                ProviderEvent::TextDelta {
                    text: "I'll inspect it.".into()
                },
                ProviderEvent::ReasoningBlock {
                    data: json!({
                        "functionCall": {"id": "call_1"},
                        "thoughtSignature": "c2lnbmF0dXJlLW9uZQ==",
                    })
                },
                ProviderEvent::ToolCallStarted {
                    index: 0,
                    id: "call_1".into(),
                    name: "read_file".into()
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 0,
                    fragment: r#"{"path":"a"}"#.into()
                },
                ProviderEvent::ReasoningBlock {
                    data: json!({
                        "functionCall": {"id": "call_2"},
                        "thoughtSignature": "c2lnbmF0dXJlLXR3bw==",
                    })
                },
                ProviderEvent::ToolCallStarted {
                    index: 1,
                    id: "call_2".into(),
                    name: "read_file".into()
                },
                ProviderEvent::ToolCallArgsDelta {
                    index: 1,
                    fragment: r#"{"path":"b"}"#.into()
                },
                ProviderEvent::Usage(Usage {
                    input_tokens: 6,
                    output_tokens: 5,
                    cache_read_input_tokens: 4,
                    cache_creation_input_tokens: 0
                }),
                ProviderEvent::Stop {
                    reason: StopReason::ToolUse
                },
            ]
        );
    }

    #[test]
    fn usage_counts_subtract_before_saturating() {
        assert_eq!(
            gemini_usage(&json!({
                "promptTokenCount": u64::from(u32::MAX) + 1,
                "cachedContentTokenCount": 1,
                "candidatesTokenCount": u64::MAX,
                "thoughtsTokenCount": 1,
            })),
            Usage {
                input_tokens: u32::MAX,
                output_tokens: u32::MAX,
                cache_read_input_tokens: 1,
                cache_creation_input_tokens: 0,
            }
        );
    }

    #[test]
    fn prompt_blocks_and_stream_errors_are_terminal_and_safe() {
        let blocked = run(&[json!({"promptFeedback":{"blockReason":"SAFETY"}})]);
        assert_eq!(
            blocked,
            vec![ProviderEvent::Refusal {
                details: RefusalDetails::from_category(Some("safety")),
            }]
        );
        let failed = run(&[json!({"error":{"code":401,"message":"AIza-secret"}})]);
        assert_eq!(
            failed,
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo {
                    kind: "authentication".into(),
                    message: "gemini returned 401".into(),
                },
            }]
        );
    }

    #[test]
    fn max_tokens_is_not_reclassified_as_a_retryable_end_turn() {
        let out = run(&[json!({"candidates":[{"finishReason":"MAX_TOKENS"}]})]);
        assert_eq!(
            out,
            vec![ProviderEvent::Stop {
                reason: StopReason::MaxTokens
            }]
        );
    }

    // ── Search grounding ───────────────────────────────────────────

    fn search_request() -> ChatRequest {
        ChatRequest {
            vendor_web_search: Some(tidebreak_core::provider::VendorWebSearch { max_uses: 3 }),
            ..request(vec![ChatMessage::text(Role::User, "what happened today?")])
        }
    }

    #[test]
    fn grounding_is_declared_only_when_the_turn_carries_no_host_tools() {
        // With host tools present, grounding is dropped rather than the tool
        // loop: the combined shape needs tool context circulation, which this
        // adapter's portable history cannot satisfy.
        let body = build_request_json(&search_request()).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["functionDeclarations"][0]["name"], "read_file");

        let mut tool_free = search_request();
        tool_free.tools.clear();
        let body = build_request_json(&tool_free).unwrap();
        assert_eq!(body["tools"], json!([{ "googleSearch": {} }]));
        // No function tools, so no function-calling config to send with them.
        assert!(body.get("toolConfig").is_none());

        // Absent the control, nothing about the request changes.
        let mut plain = tool_free.clone();
        plain.vendor_web_search = None;
        let body = build_request_json(&plain).unwrap();
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn a_prior_client_side_search_is_replayed_under_another_name() {
        // Gemini pairs a call to its response by name alone, so a replayed
        // client `web_search` beside the declared `googleSearch` tool is
        // renamed on both halves of the pair.
        let mut req = search_request();
        req.tools.clear();
        req.messages.push(ChatMessage {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".into(),
                name: "web_search".into(),
                input: json!({"query": "yesterday"}),
            }],
            reasoning: MessageReasoning::default(),
        });
        req.messages.push(ChatMessage {
            role: Role::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".into(),
                content: "one hit".into(),
                is_error: false,
            }],
            reasoning: MessageReasoning::default(),
        });

        let body = build_request_json(&req).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(
            contents[1]["parts"][0]["functionCall"]["name"],
            "web_search_prior"
        );
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["name"],
            "web_search_prior"
        );

        // Without the vendor tool the history keeps its own name.
        req.vendor_web_search = None;
        let body = build_request_json(&req).unwrap();
        assert_eq!(
            body["contents"][1]["parts"][0]["functionCall"]["name"],
            "web_search"
        );
    }

    #[test]
    fn grounding_metadata_on_the_final_chunk_becomes_one_executed_search() {
        let out = run(&[
            json!({"candidates":[{"content":{"parts":[{"text":"Two teams advanced."}]}}]}),
            json!({"candidates":[{
                "content": {"parts": [{"text": ""}]},
                "groundingMetadata": {
                    "webSearchQueries": ["who advanced today", "semi-final results"],
                    "groundingChunks": [
                        {"web": {"uri": "https://a.example/1", "title": "a.example"}},
                        // Deduped by url, and a chunk with no web entry or no
                        // uri contributes nothing.
                        {"web": {"uri": "https://a.example/1", "title": "a.example"}},
                        {"web": {"title": "no uri"}},
                        {"retrievedContext": {"title": "not a web chunk"}},
                        {"web": {"uri": "https://b.example/2", "title": "b.example"}}
                    ],
                    "groundingSupports": [{"segment": {"startIndex": 0, "endIndex": 3}}],
                    "searchEntryPoint": {"renderedContent": "<div>suggestions</div>"}
                },
                "finishReason": "STOP"
            }]}),
        ]);

        assert_eq!(
            out,
            vec![
                ProviderEvent::TextDelta {
                    text: "Two teams advanced.".into()
                },
                ProviderEvent::ProviderExecutedToolCall {
                    name: "web_search".into(),
                    input: json!({"query": "who advanced today; semi-final results"}),
                    output: json!({
                        "provider": "gemini",
                        "results": [
                            {"url": "https://a.example/1", "title": "a.example", "snippet": ""},
                            {"url": "https://b.example/2", "title": "b.example", "snippet": ""},
                        ],
                        "attribution_html": "<div>suggestions</div>",
                    }),
                    is_error: false,
                    replay: None,
                },
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn
                },
            ]
        );
    }

    #[test]
    fn grounding_results_are_capped_and_empty_metadata_reports_nothing() {
        let chunks: Vec<Value> = (0..tidebreak_core::MAX_WEB_SEARCH_RESULTS + 5)
            .map(|n| json!({"web": {"uri": format!("https://e.example/{n}"), "title": "e"}}))
            .collect();
        let event = grounding_search_event(&json!({
            "webSearchQueries": ["one"],
            "groundingChunks": chunks,
        }))
        .unwrap();
        let ProviderEvent::ProviderExecutedToolCall { output, .. } = event else {
            panic!("expected an executed search");
        };
        assert_eq!(
            output["results"].as_array().unwrap().len(),
            tidebreak_core::MAX_WEB_SEARCH_RESULTS
        );
        // Nothing to attribute when the entry point is absent.
        assert!(output.get("attribution_html").is_none());

        // Oversized markup is dropped whole; the search is still reported.
        let event = grounding_search_event(&json!({
            "webSearchQueries": ["one"],
            "searchEntryPoint": {"renderedContent": "x".repeat(64 * 1024 + 1)},
        }))
        .unwrap();
        let ProviderEvent::ProviderExecutedToolCall { output, .. } = event else {
            panic!("expected an executed search");
        };
        assert!(output.get("attribution_html").is_none());
        assert_eq!(output["results"], json!([]));

        // Metadata with neither queries nor sources is not a failed search.
        assert!(grounding_search_event(&json!({})).is_none());
        assert!(grounding_search_event(&json!({
            "webSearchQueries": [],
            "groundingChunks": [{"web": {"title": "no uri"}}],
        }))
        .is_none());
    }

    #[test]
    fn endpoint_uses_developer_api_streaming_route() {
        let provider = GeminiProvider::new("key").with_base_url("http://127.0.0.1:8080/");
        assert_eq!(
            provider.endpoint("gemini-3.6-flash").unwrap(),
            "http://127.0.0.1:8080/v1beta/models/gemini-3.6-flash:streamGenerateContent?alt=sse"
        );
    }

    #[tokio::test]
    async fn setup_and_connection_failures_carry_provider_attribution() {
        let provider = GeminiProvider::new("developer-key").with_base_url("http://127.0.0.1:1");
        let mut invalid = request(vec![ChatMessage::text(Role::User, "hi")]);
        invalid.model = "../not-a-model".into();
        let error = match provider.stream(invalid).await {
            Err(error) => error,
            Ok(_) => panic!("gemini unexpectedly accepted an invalid model id"),
        };
        assert!(
            error
                .to_string()
                .contains("gemini received an invalid model id"),
            "{error}"
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let provider =
            GeminiProvider::new("developer-key").with_base_url(format!("http://{address}"));
        let error = match provider
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("gemini unexpectedly connected to a closed fixture port"),
        };
        assert_eq!(error.to_string(), "provider error: gemini request failed");
    }

    #[tokio::test]
    async fn requests_carry_the_developer_api_key_header_and_the_shared_body() {
        use axum::extract::State;
        use axum::http::{header, HeaderMap};
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::{Json, Router};
        use std::sync::{Arc, Mutex};

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<(HeaderMap, Value)>>>);

        async fn capture(
            State(capture): State<Capture>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            capture.0.lock().unwrap().push((headers, body));
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n",
            )
        }

        let capture_state = Capture::default();
        let app = Router::new()
            .fallback(post(capture))
            .with_state(capture_state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base_url = format!("http://{address}");

        let developer = GeminiProvider::new("developer-key").with_base_url(&base_url);
        let mut stream = developer
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let requests = capture_state.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].0.get("x-goog-api-key").unwrap(),
            "developer-key"
        );
        assert!(requests[0].0.get(header::AUTHORIZATION).is_none());
        assert_eq!(requests[0].1["generationConfig"]["maxOutputTokens"], 65_536);
        assert_eq!(
            requests[0].1["tools"][0]["functionDeclarations"][0]["name"],
            "read_file"
        );
        server.abort();
    }

    #[tokio::test]
    async fn http_errors_carry_provider_attribution() {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::{Json, Router};

        async fn deny() -> impl IntoResponse {
            (
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error": {
                        "code": "permission_denied",
                        "message": "The caller does not have permission",
                    }
                })),
            )
        }

        let app = Router::new().fallback(post(deny));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let base_url = format!("http://{address}");
        let developer = GeminiProvider::new("developer-key").with_base_url(&base_url);
        let error = match developer
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("Gemini unexpectedly accepted the denied request"),
        };
        server.abort();

        assert!(matches!(error, AgentError::AccessDenied(_)));
        let visible = error.to_string();
        assert!(visible.contains("gemini returned 403"), "{visible}");
        assert!(visible.contains("permission_denied"), "{visible}");
    }

    #[test]
    fn oversized_prompts_classify_as_prompt_too_long() {
        let body = json!({
            "error": {
                "code": "invalid_argument",
                "message": "Input token count exceeds the maximum number of tokens",
            }
        })
        .to_string();

        let error = classify_gemini_error("gemini", 400, &body, None);
        assert!(matches!(error, AgentError::PromptTooLong(_)));
        assert!(error.to_string().contains("gemini returned 400"));
    }

    #[tokio::test]
    async fn malformed_frames_carry_provider_attribution() {
        use axum::http::header;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;

        async fn malformed() -> impl IntoResponse {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: not-json\n\n",
            )
        }

        let app = Router::new().fallback(post(malformed));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let provider =
            GeminiProvider::new("developer-key").with_base_url(format!("http://{address}"));
        let events: Vec<_> = provider
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(
            events,
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::provider(
                    "gemini returned an invalid stream frame".to_string()
                ),
            }]
        );
        server.abort();
    }

    #[tokio::test]
    async fn transport_failures_carry_provider_attribution() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let _ = socket.read(&mut request).await.unwrap();
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 128\r\nconnection: close\r\n\r\n: accepted\n\n",
                    )
                    .await
                    .unwrap();
            }
        });
        let provider =
            GeminiProvider::new("developer-key").with_base_url(format!("http://{address}"));
        let events: Vec<_> = provider
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(
            events,
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo::provider("gemini stream ended early".to_string()),
            }]
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn in_band_error_after_accept_fails_the_stream() {
        use axum::http::header;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;

        async fn deny_after_accepting() -> impl IntoResponse {
            let frame = json!({
                "error": {
                    "code": 403,
                    "type": "permission_denied",
                    "message": "The caller does not have permission",
                }
            });
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {frame}\n\n"),
            )
        }

        let app = Router::new().fallback(post(deny_after_accepting));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let provider =
            GeminiProvider::new("developer-key").with_base_url(format!("http://{address}"));
        let events: Vec<_> = provider
            .stream(request(vec![ChatMessage::text(Role::User, "hi")]))
            .await
            .unwrap()
            .collect()
            .await;
        server.abort();

        assert_eq!(
            events,
            vec![ProviderEvent::Failed {
                error: ProviderErrorInfo {
                    kind: "access_denied".into(),
                    message: "gemini returned 403 (permission_denied): The caller does not have permission"
                        .into(),
                },
            }]
        );
    }
}
