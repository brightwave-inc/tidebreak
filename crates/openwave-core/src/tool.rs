//! The tool contract: how the agent invokes a capability.
//!
//! Every tool is a typed args/result pair with a JSON Schema — no
//! stringly-typed tools. Tools come from three sources (built-in, skill-backed,
//! MCP-mounted) but all implement this one trait so the registry treats them
//! uniformly.

use std::path::PathBuf;
#[cfg(feature = "tools")]
use std::sync::Arc;

#[cfg(feature = "tools")]
use cap_std::ambient_authority;
#[cfg(feature = "tools")]
use cap_std::fs::Dir;

use async_trait::async_trait;
use schemars::{generate::SchemaSettings, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::error::Result;
use crate::id::{CallId, ChatId, ProjectId};

/// A pinned runtime-only directory capability for legacy private-scratch tools.
///
/// It carries no host path and grants access only to the already-open directory
/// handle supplied by the embedding runtime.
#[derive(Clone)]
pub struct ToolScratch {
    #[cfg(feature = "tools")]
    workspace: Arc<Dir>,
}

impl std::fmt::Debug for ToolScratch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolScratch")
            .field("available", &cfg!(feature = "tools"))
            .finish_non_exhaustive()
    }
}

impl ToolScratch {
    /// Wrap an already-open exact directory capability.
    #[cfg(feature = "tools")]
    #[must_use]
    pub fn from_dir(workspace: Dir) -> Self {
        Self {
            workspace: Arc::new(workspace),
        }
    }
}

/// The approval policy class a tool declares for itself.
///
/// Policy maps class → auto-approve / ask / deny. In v1: `ReadOnly` and
/// `Workspace` auto-approve; `Sensitive` parks on the approval gate unless a
/// matching standing grant covers the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalClass {
    /// Never mutates anything (e.g. `read_file`, `list_dir`, `search`).
    ReadOnly,
    /// Mutates the chat workspace (e.g. `write_file`).
    Workspace,
    /// Escapes the workspace or reaches the network / external services
    /// (connector writes, networked `exec`, writes outside the workspace).
    Sensitive,
}

impl ApprovalClass {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Workspace => "workspace",
            Self::Sensitive => "sensitive",
        }
    }
}

/// A tool's public contract: name, description, and the JSON Schema its
/// arguments must satisfy. This is what gets advertised to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique tool name (MCP-mounted tools are namespaced `mcp__{server}__{tool}`).
    pub name: String,
    /// Human- and model-readable description of what the tool does.
    pub description: String,
    /// JSON Schema (draft 2020-12) describing the argument object.
    pub input_schema: Value,
}

impl ToolSpec {
    /// Build a tool contract whose input schema comes from its deserialized
    /// argument type.
    ///
    /// The provider-facing schema intentionally omits document metadata and
    /// inlines nested types. OpenWave advertised that compact shape before
    /// typed derivation, and provider adapters forward this value unchanged.
    #[must_use]
    pub fn for_args<Args: JsonSchema>(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema: input_schema_for::<Args>(),
        }
    }
}

/// Generate the compact provider-facing JSON Schema for one argument type.
///
/// A meaningful `default` is kept. Most optional tool arguments are
/// `#[serde(default)]` fields whose absent value is a real decision — `exec`
/// runs in `"."` — and the model cannot
/// infer any of that from the type alone. A null default is dropped: it is what
/// an `Option` field emits, and "omitting this omits it" is not information
/// worth spending tokens on. The strict conversion drops the keyword entirely,
/// since there every property is required and a default can never apply.
#[must_use]
pub fn input_schema_for<Args: JsonSchema>() -> Value {
    let schema = SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.meta_schema = None;
            settings.inline_subschemas = true;
        })
        .into_generator()
        .into_root_schema_for::<Args>();
    let mut schema =
        serde_json::to_value(schema).expect("a generated JSON Schema always serializes");
    let root = schema
        .as_object_mut()
        .expect("a typed argument schema is an object");
    root.remove("title");
    root.remove("description");
    if root.get("type").and_then(Value::as_str) == Some("object") {
        root.entry("properties")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    remove_null_schema_defaults(&mut schema);
    schema
}

fn remove_null_schema_defaults(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            if object.get("default") == Some(&Value::Null) {
                object.remove("default");
            }
            for value in object.values_mut() {
                remove_null_schema_defaults(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                remove_null_schema_defaults(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// What to do with a property the schema does not require, when converting to
/// the strict schema subset providers enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionalProperties {
    /// Widen the property to also accept `null`, then require it.
    ///
    /// Correct when the reader is a serde `Option`, which reads an explicit
    /// `null` back as absent.
    AcceptNull,
    /// Refuse the conversion.
    ///
    /// Correct when the reader's handling of an absent value is unknown. A
    /// hand-written tool schema whose optional property deserializes into a
    /// `#[serde(default)]` field that is *not* an `Option` rejects the `null`
    /// that widening invites, and would start failing every call.
    Reject,
}

/// Keywords carried through a strict conversion unchanged.
///
/// Anything not listed here and not handled structurally below is refused
/// rather than dropped. Silently discarding a constraint changes what the model
/// was asked for, and strict mode is precisely the promise that it did not.
const STRICT_PASSTHROUGH_KEYWORDS: &[&str] = &[
    "description",
    "enum",
    "const",
    "pattern",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
];

/// `format` values providers recognize in a strict schema.
///
/// `format` is an annotation rather than a constraint, and generators emit
/// Rust-shaped values (`uint16`, `float`) that a strict validator rejects
/// outright. An unrecognized value is therefore dropped instead of refusing the
/// whole schema: the `type` and the numeric bounds beside it still carry the
/// constraint.
const STRICT_FORMATS: &[&str] = &[
    "date-time",
    "time",
    "date",
    "duration",
    "email",
    "hostname",
    "ipv4",
    "ipv6",
    "uuid",
];

/// Rewrite a draft 2020-12 schema into the strict subset providers enforce, or
/// return `None` when it cannot be expressed there.
///
/// Strict mode narrows JSON Schema in two ways. Every object must close
/// `additionalProperties` and must enumerate its properties — so a schema that
/// declares no `properties` at all ("any object") has no strict form. And every
/// declared property must appear in `required`, which is not safe in general;
/// `optional` decides what happens when one does not.
///
/// Refusing rather than guessing is the point. A schema the provider rejects
/// fails the whole turn, so a caller needs a definite answer: either a schema
/// the provider will accept, or a signal to send the request unconstrained.
#[must_use]
pub fn strict_json_schema(schema: &Value, optional: OptionalProperties) -> Option<Value> {
    let object = schema.as_object()?;
    let mut strict = serde_json::Map::new();
    for (keyword, value) in object {
        match keyword.as_str() {
            // Handled structurally below.
            "type" | "properties" | "required" | "items" | "additionalProperties" | "anyOf" => {}
            // Not a constraint on the value, so it neither survives nor blocks.
            // Strict mode requires every property, so an absent-value default
            // can never apply and providers reject the keyword outright.
            "title" | "default" => {}
            "format" => {
                if value.as_str().is_some_and(|f| STRICT_FORMATS.contains(&f)) {
                    strict.insert(keyword.clone(), value.clone());
                }
            }
            keyword if STRICT_PASSTHROUGH_KEYWORDS.contains(&keyword) => {
                strict.insert(keyword.to_owned(), value.clone());
            }
            // `$ref`/`$defs`, `oneOf`, `allOf`, `not`, `if`/`then`,
            // `patternProperties`, and whatever a later draft adds.
            _ => return None,
        }
    }

    if let Some(branches) = object.get("anyOf") {
        let branches: Vec<Value> = branches
            .as_array()
            .filter(|branches| !branches.is_empty())?
            .iter()
            .map(|branch| strict_json_schema(branch, optional))
            .collect::<Option<_>>()?;
        strict.insert("anyOf".to_owned(), Value::Array(branches));
    }

    let types: Vec<&str> = match object.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
            .filter(|names| !names.is_empty())?,
        Some(_) => return None,
        // Strict mode has no way to say "any value"; a branch schema carries
        // its types inside the branches instead.
        None if strict.contains_key("anyOf") => Vec::new(),
        None => return None,
    };

    if types.contains(&"object") {
        let properties = object.get("properties").and_then(Value::as_object)?;
        let required: Vec<&str> = match object.get("required") {
            Some(Value::Array(names)) => names.iter().map(Value::as_str).collect::<Option<_>>()?,
            Some(_) => return None,
            None => Vec::new(),
        };
        let mut strict_properties = serde_json::Map::new();
        for (name, property) in properties {
            let mut property = strict_json_schema(property, optional)?;
            if !required.contains(&name.as_str()) {
                if optional == OptionalProperties::Reject {
                    return None;
                }
                property = accepting_null(property)?;
            }
            strict_properties.insert(name.clone(), property);
        }
        strict.insert(
            "required".to_owned(),
            Value::Array(properties.keys().cloned().map(Value::String).collect()),
        );
        strict.insert("properties".to_owned(), Value::Object(strict_properties));
        strict.insert("additionalProperties".to_owned(), Value::Bool(false));
    }
    if types.contains(&"array") {
        let items = strict_json_schema(object.get("items")?, optional)?;
        strict.insert("items".to_owned(), items);
    }
    if let Some(declared) = object.get("type") {
        strict.insert("type".to_owned(), declared.clone());
    }
    Some(Value::Object(strict))
}

/// Widen an already-strict property schema to also accept `null`.
fn accepting_null(mut property: Value) -> Option<Value> {
    let object = property.as_object_mut()?;
    match object.get_mut("type") {
        Some(Value::String(name)) if name != "null" => {
            let widened = vec![
                Value::String(name.clone()),
                Value::String("null".to_owned()),
            ];
            object.insert("type".to_owned(), Value::Array(widened));
        }
        Some(Value::String(_)) => {}
        Some(Value::Array(names)) => {
            if !names.iter().any(|name| name.as_str() == Some("null")) {
                names.push(Value::String("null".to_owned()));
            }
        }
        // A branch schema has no single type to widen, and adding a `"null"`
        // branch would need every sibling keyword re-checked against it.
        _ => return None,
    }
    Some(property)
}

/// The result of executing a tool.
///
/// `content` is the model-readable result folded back into the conversation;
/// `data` is an optional structured payload for clients that can render it
/// (e.g. a tool-call card). A failing tool returns `is_error = true` rather than
/// Why a tool call failed.
///
/// A boolean says something went wrong; a category says whether anything *is*
/// wrong. A call the reader cancelled and a call the reader declined are not
/// failures of the tool, of the model, or of the product, and recording them
/// the same way as a crash makes every later question about reliability
/// unanswerable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolErrorCategory {
    /// The reader stopped the turn before or during the call.
    UserCancelled,
    /// The reader declined the call at the approval gate.
    UserDeclined,
    /// The model named a tool this turn does not advertise.
    NotFound,
    /// The call's arguments did not parse as JSON, so nothing ran.
    InvalidArguments,
    /// The capability is available after the reader configures it.
    ConfigurationRequired,
    /// The channel to an external tool failed before the tool could report an
    /// outcome of its own.
    TransportFailed,
    /// The tool ran and reported a failure of its own.
    ToolFailed,
}

impl ToolErrorCategory {
    /// Stable durable and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserCancelled => "user_cancelled",
            Self::UserDeclined => "user_declined",
            Self::NotFound => "not_found",
            Self::InvalidArguments => "invalid_arguments",
            Self::ConfigurationRequired => "configuration_required",
            Self::TransportFailed => "transport_failed",
            Self::ToolFailed => "tool_failed",
        }
    }

    /// Whether this counts against the product rather than describing a choice
    /// the reader made or a request the model got wrong.
    #[must_use]
    pub const fn is_product_failure(self) -> bool {
        match self {
            Self::UserCancelled
            | Self::UserDeclined
            | Self::NotFound
            | Self::ConfigurationRequired
            | Self::InvalidArguments => false,
            Self::TransportFailed | Self::ToolFailed => true,
        }
    }
}

/// an `Err` so the model sees the failure and can adapt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Result text fed back to the model.
    pub content: String,
    /// Optional structured payload for richer client rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Whether the tool reported a failure.
    #[serde(default)]
    pub is_error: bool,
    /// Why it failed, when it did. `None` on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_category: Option<ToolErrorCategory>,
    /// The validated MCP Apps view declared for the tool that produced this
    /// output, when there is one. Journal-durable so a replayed completion can
    /// still surface its view; never part of the model-facing content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_view: Option<Box<ToolUiView>>,
    /// Durable references to images produced by this tool.
    ///
    /// Pixel bytes ride beside these references only until the agent publishes
    /// them to blob storage. Journals, tool-call rows, and renderer events see
    /// identity and geometry, never media bytes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::ImageRef>,
    /// Ephemeral pixels backing [`Self::images`] before publication.
    #[serde(skip, default)]
    pub image_data: crate::ImageAttachments,
}

/// A tool's declared MCP Apps view: which configured server can serve it and
/// the validated `ui://` document it declared at discovery.
///
/// `server` is the locally configured namespace (user-authored, already shown
/// in Settings), and `resource_uri` passed the discovery-time validation in
/// `openwave-mcp` (bounded, `ui://`-schemed, no control characters). Remote
/// tool names and descriptions still never cross the renderer boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUiView {
    /// The configured MCP server namespace that can serve the document.
    pub server: String,
    /// The validated `ui://` document URI.
    pub resource_uri: String,
}

impl ToolOutput {
    /// A successful text result.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            data: None,
            is_error: false,
            error_category: None,
            ui_view: None,
            images: Vec::new(),
            image_data: crate::ImageAttachments::new(),
        }
    }

    /// A failure the model should see and react to.
    ///
    /// Categorized as the tool's own failure. Callers that know better — the
    /// loop, which is the only thing that can tell a cancellation from a
    /// crash — use [`Self::failed`].
    pub fn error(content: impl Into<String>) -> Self {
        Self::failed(ToolErrorCategory::ToolFailed, content)
    }

    /// A failure whose cause the caller can name.
    pub fn failed(category: ToolErrorCategory, content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            data: None,
            is_error: true,
            error_category: Some(category),
            ui_view: None,
            images: Vec::new(),
            image_data: crate::ImageAttachments::new(),
        }
    }

    /// Attach a declared MCP Apps view to this output.
    #[must_use]
    pub fn with_ui_view(mut self, view: ToolUiView) -> Self {
        // Boxed so the rare view does not widen every ToolOutput ever moved.
        self.ui_view = Some(Box::new(view));
        self
    }

    /// Attach a structured payload to this output.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Attach image references and their short-lived bytes.
    #[must_use]
    pub fn with_images(
        mut self,
        images: impl IntoIterator<Item = (crate::ImageRef, crate::ImageData)>,
    ) -> Self {
        for (image, data) in images {
            self.image_data.insert(image.blob_id, data);
            self.images.push(image);
        }
        self
    }

    /// Offer the renderer a list of what this call surfaced.
    ///
    /// The tool's own output text is what the model reads and is written for
    /// the model; these rows are what a person reads. Attached here rather than
    /// hand-built into `data` so a tool cannot get the key wrong and silently
    /// project nothing — see [`crate::ToolResultPreview::Entries`], which
    /// clamps every row before it crosses.
    #[must_use]
    pub fn with_entries(self, entries: Vec<crate::ResultEntry>) -> Self {
        self.with_projected("entries", serde_json::json!(entries))
    }

    /// Report what this call could not do, beside what it did.
    ///
    /// A batch tool that lists only its successes is not reporting. Carried
    /// alongside [`Self::with_entries`] rather than instead of it — a call that
    /// imported three files and failed two has both to say.
    #[must_use]
    pub fn with_failures(self, failures: Vec<crate::ResultFailure>) -> Self {
        self.with_projected("failures", serde_json::json!(failures))
    }

    /// Merge one renderer-projected key into this output's structured data.
    fn with_projected(mut self, key: &str, value: Value) -> Self {
        match self.data.as_mut().and_then(Value::as_object_mut) {
            Some(data) => {
                data.insert(key.into(), value);
            }
            None => self.data = Some(serde_json::json!({ key: value })),
        }
        self
    }
}

/// Execution context handed to a tool for one invocation.
///
/// Deliberately minimal in this slice — it grows (cancellation, store handles)
/// as the agent loop lands.
#[derive(Clone)]
pub struct ToolCtx {
    /// The chat this call belongs to.
    pub chat_id: ChatId,
    /// Project corpus inherited from the chat, or `None` for a loose chat.
    pub project_id: Option<ProjectId>,
    /// Stable identity of the canonical tool call, when execution came from an
    /// agent turn. Legacy direct/MCP contexts leave this absent.
    pub call_id: Option<CallId>,
    #[cfg(feature = "tools")]
    workspace: WorkspaceAccess,
}

#[cfg(feature = "tools")]
#[derive(Clone)]
enum WorkspaceAccess {
    Open(Arc<Dir>),
    Unavailable(Arc<str>),
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolCtx")
            .field("chat_id", &self.chat_id)
            .field("project_id", &self.project_id)
            .field("call_id", &self.call_id)
            .field("private_scratch_available", &self.scratch_available())
            .finish_non_exhaustive()
    }
}

impl ToolCtx {
    /// Build a legacy CLI/MCP context by opening an explicit workspace path.
    ///
    /// Product turns must use a pinned [`ToolScratch`] supplied by their runtime.
    pub fn new_legacy_workspace(
        chat_id: ChatId,
        project_id: Option<ProjectId>,
        workspace_dir: PathBuf,
    ) -> Self {
        match Self::try_new_legacy_workspace(chat_id, project_id, workspace_dir) {
            Ok(ctx) => ctx,
            Err(_error) => Self {
                chat_id,
                project_id,
                call_id: None,
                #[cfg(feature = "tools")]
                workspace: WorkspaceAccess::Unavailable(_error.to_string().into()),
            },
        }
    }

    /// Build a legacy CLI/MCP context, failing if its path cannot be pinned.
    pub fn try_new_legacy_workspace(
        chat_id: ChatId,
        project_id: Option<ProjectId>,
        workspace_dir: PathBuf,
    ) -> std::io::Result<Self> {
        #[cfg(feature = "tools")]
        let workspace = Dir::open_ambient_dir(&workspace_dir, ambient_authority())?;
        Ok(Self {
            chat_id,
            project_id,
            call_id: None,
            #[cfg(feature = "tools")]
            workspace: WorkspaceAccess::Open(Arc::new(workspace)),
        })
    }

    /// Build a product execution context from an exact pinned scratch handle.
    #[must_use]
    pub fn with_private_scratch(
        chat_id: ChatId,
        project_id: Option<ProjectId>,
        scratch: ToolScratch,
    ) -> Self {
        Self {
            chat_id,
            project_id,
            call_id: None,
            #[cfg(feature = "tools")]
            workspace: WorkspaceAccess::Open(scratch.workspace),
        }
    }

    /// Build a context with no direct filesystem scratch available.
    ///
    /// Non-filesystem tools remain usable; a legacy filesystem tool fails
    /// closed instead of resolving an absent path against the process CWD.
    #[must_use]
    pub fn without_private_scratch(chat_id: ChatId, project_id: Option<ProjectId>) -> Self {
        Self {
            chat_id,
            project_id,
            call_id: None,
            #[cfg(feature = "tools")]
            workspace: WorkspaceAccess::Unavailable("private scratch is unavailable".into()),
        }
    }

    /// Attach the canonical tool-call identity to this invocation context.
    #[must_use]
    pub fn with_call_id(mut self, call_id: CallId) -> Self {
        self.call_id = Some(call_id);
        self
    }

    fn scratch_available(&self) -> bool {
        #[cfg(feature = "tools")]
        {
            matches!(&self.workspace, WorkspaceAccess::Open(_))
        }
        #[cfg(not(feature = "tools"))]
        {
            false
        }
    }

    #[cfg(feature = "tools")]
    pub(crate) fn workspace(&self) -> std::result::Result<Arc<Dir>, String> {
        match &self.workspace {
            WorkspaceAccess::Open(workspace) => Ok(Arc::clone(workspace)),
            WorkspaceAccess::Unavailable(error) => {
                Err(format!("private scratch unavailable: {error}"))
            }
        }
    }
}

/// A capability the agent can invoke. Implementors are held as trait objects in
/// the registry, so this trait must stay object-safe (hence `#[async_trait]`).
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool's advertised contract.
    fn spec(&self) -> ToolSpec;

    /// The approval class governing this tool's calls.
    fn approval_class(&self) -> ApprovalClass;

    /// Execute the tool with JSON `args` matching [`ToolSpec::input_schema`].
    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput>;
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::Deserialize;

    #[derive(Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    /// Root documentation belongs in Rust docs, not the provider schema.
    #[allow(dead_code)]
    struct DerivedArguments {
        #[serde(default)]
        #[schemars(with = "String", description = "Optional text.")]
        optional: Option<String>,
        #[serde(default = "defaulted")]
        #[schemars(description = "Text with a default.")]
        defaulted: String,
    }

    fn defaulted() -> String {
        ".".to_owned()
    }

    #[test]
    fn typed_argument_schema_keeps_the_compact_provider_shape() {
        assert_eq!(
            input_schema_for::<DerivedArguments>(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "optional": {
                        "type": "string",
                        "description": "Optional text."
                    },
                    "defaulted": {
                        "type": "string",
                        "description": "Text with a default.",
                        "default": "."
                    }
                },
                "additionalProperties": false
            })
        );
    }

    #[test]
    fn strict_mode_requires_every_property_or_refuses_to_convert() {
        // The case the post-processor exists for: schemars leaves a defaulted
        // field out of `required`, and strict mode has no optional properties.
        // The advertised `default` goes with it — every property is required
        // here, so no default could ever apply.
        let schema = input_schema_for::<DerivedArguments>();
        assert_eq!(
            strict_json_schema(&schema, OptionalProperties::AcceptNull).unwrap(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "optional": {
                        "type": ["string", "null"],
                        "description": "Optional text."
                    },
                    "defaulted": {
                        "type": ["string", "null"],
                        "description": "Text with a default."
                    }
                },
                "required": ["defaulted", "optional"],
                "additionalProperties": false
            })
        );
        // Widening invites a `null` the reader has to absorb, which only an
        // `Option` reliably does. A hand-written tool schema cannot promise
        // that, so the conversion is refused rather than quietly performed.
        assert!(strict_json_schema(&schema, OptionalProperties::Reject).is_none());
    }

    #[test]
    fn strict_mode_closes_objects_and_refuses_what_it_cannot_express() {
        // Closing the object is the safe half of strict mode: readers ignore
        // unknown keys, so forbidding them takes nothing away.
        let open = serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
        });
        let strict = strict_json_schema(&open, OptionalProperties::Reject).unwrap();
        assert_eq!(strict["additionalProperties"], serde_json::json!(false));

        // A generator-shaped `format` is an annotation a strict validator
        // rejects outright, so it goes while the real bound beside it stays.
        let counted = serde_json::json!({
            "type": "object",
            "properties": { "n": { "type": "integer", "format": "uint16", "minimum": 0 } },
            "required": ["n"],
        });
        let strict = strict_json_schema(&counted, OptionalProperties::Reject).unwrap();
        assert_eq!(
            strict["properties"]["n"],
            serde_json::json!({ "type": "integer", "minimum": 0 })
        );

        // An object declaring no properties means "any object"; closing it would
        // narrow it to `{}`. A `$ref` is a constraint we never translated.
        for inexpressible in [
            serde_json::json!({ "type": "object" }),
            serde_json::json!({ "$ref": "#/$defs/Node" }),
            serde_json::json!({ "description": "anything at all" }),
        ] {
            assert!(
                strict_json_schema(&inexpressible, OptionalProperties::AcceptNull).is_none(),
                "{inexpressible}"
            );
        }
    }

    #[test]
    fn a_reader_choice_is_not_recorded_as_a_product_failure() {
        // The distinction this exists for: a cancelled or declined call is a
        // choice, not a defect, and counting it as one makes any later question
        // about reliability unanswerable.
        for category in [
            ToolErrorCategory::UserCancelled,
            ToolErrorCategory::UserDeclined,
            ToolErrorCategory::NotFound,
            ToolErrorCategory::ConfigurationRequired,
        ] {
            assert!(!category.is_product_failure(), "{}", category.as_str());
        }
        assert!(ToolErrorCategory::TransportFailed.is_product_failure());
        assert!(ToolErrorCategory::ToolFailed.is_product_failure());

        // Every category has a distinct durable spelling.
        let spellings = [
            ToolErrorCategory::UserCancelled,
            ToolErrorCategory::UserDeclined,
            ToolErrorCategory::NotFound,
            ToolErrorCategory::ConfigurationRequired,
            ToolErrorCategory::TransportFailed,
            ToolErrorCategory::ToolFailed,
        ]
        .map(ToolErrorCategory::as_str);
        assert_eq!(
            spellings.len(),
            spellings
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }

    #[test]
    fn an_uncategorized_failure_reads_as_the_tool_failing() {
        // `error` is what an ordinary tool calls, and a tool reporting a failure
        // is exactly the tool failing. Only the loop can distinguish more.
        let plain = ToolOutput::error("boom");
        assert!(plain.is_error);
        assert_eq!(plain.error_category, Some(ToolErrorCategory::ToolFailed));

        let cancelled = ToolOutput::failed(ToolErrorCategory::UserCancelled, "stopped");
        assert_eq!(
            cancelled.error_category,
            Some(ToolErrorCategory::UserCancelled)
        );

        // Success carries no category at all rather than a benign-looking one.
        assert_eq!(ToolOutput::text("fine").error_category, None);
        assert!(!ToolOutput::text("fine").is_error);
    }

    use super::*;

    #[test]
    fn tool_output_omits_absent_data_when_serialized() {
        let json = serde_json::to_string(&ToolOutput::text("ok")).unwrap();
        assert!(
            !json.contains("data"),
            "absent data should be skipped: {json}"
        );

        let with = ToolOutput::text("ok").with_data(serde_json::json!({"k": 1}));
        assert_eq!(with.data, Some(serde_json::json!({"k": 1})));
    }

    #[test]
    fn tool_output_serializes_preview_identity_but_never_pixels() {
        let bytes = vec![9, 8, 7];
        let image = crate::ImageRef {
            blob_id: uuid::Uuid::from_u128(9),
            media_type: crate::ImageMediaType::Png,
            width: 1,
            height: 1,
            byte_len: 3,
        };
        let output = ToolOutput::text("ok")
            .with_images([(image, crate::ImageData::new(image.media_type, bytes))]);
        let json = serde_json::to_value(output).unwrap();
        assert_eq!(json["images"][0]["blob_id"], image.blob_id.to_string());
        assert!(json.get("image_data").is_none());
        assert!(json.get("bytes").is_none());
    }

    #[test]
    fn approval_class_serializes_snake_case() {
        let json = serde_json::to_string(&ApprovalClass::ReadOnly).unwrap();
        assert_eq!(json, "\"read_only\"");
    }
}
