//! OpenAPI ingest into a bounded operation catalog.
//!
//! A `rest_api` connected app carries an operator-supplied OpenAPI document.
//! That document is foreign input: it is read exactly once, at configuration
//! time, and either yields a complete, self-contained [`OperationCatalog`] or
//! is refused outright — the fail-closed posture the model gateway applies to
//! operator-supplied specs. There is no partial acceptance: an operation the
//! catalog cannot name or bound would make the catalog lie about the document,
//! so any such operation refuses the whole ingest.
//!
//! The catalog is what the governed REST executor validates requests against
//! (path template, method, parameter shape) and what `{app, operation_ids[]}`
//! manifest bindings name. Internal `$ref`s are resolved here so the stored
//! catalog never depends on the document again; the document's `servers` are
//! deliberately ignored because the base URL comes from the connected-app
//! record, never from the document.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Largest OpenAPI document accepted, in bytes. Prevents a configuration
/// upload from ballooning the connected-app record and the ingest's memory.
pub const MAX_OPENAPI_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Most operations one catalog may hold. Prevents a spec from minting an
/// unbounded binding/consent surface.
pub const MAX_CATALOG_OPERATIONS: usize = 256;
/// Longest `operationId` accepted, in bytes. Ids are echoed in errors,
/// fingerprints, and manifests, so they must stay short and printable.
pub const MAX_OPERATION_ID_BYTES: usize = 128;
/// Longest path template accepted, in bytes. Bounds what the executor must
/// match and what refusals may echo.
pub const MAX_PATH_TEMPLATE_BYTES: usize = 512;
/// Most parameters one operation may declare after path-level merge. Bounds
/// per-request validation work in the executor.
pub const MAX_OPERATION_PARAMETERS: usize = 64;
/// Longest parameter name accepted, in bytes. Parameter names become request
/// keys the executor matches against.
pub const MAX_PARAMETER_NAME_BYTES: usize = 128;
/// Largest stored schema subtree, in serialized bytes. Keeps a single
/// parameter or body schema from smuggling the whole document into the record.
pub const MAX_SCHEMA_SUBTREE_BYTES: usize = 16 * 1024;
/// Deepest `$ref` chain followed during resolution. Caps work on cyclic or
/// adversarially nested reference graphs.
pub const MAX_REF_RESOLUTION_DEPTH: usize = 8;

/// HTTP methods an operation may declare — the standard OpenAPI path-item set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpMethod {
    Get,
    Put,
    Post,
    Delete,
    Patch,
    Head,
    Options,
}

impl HttpMethod {
    /// The method's lowercase spelling, as it appears as a path-item key.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "get",
            HttpMethod::Put => "put",
            HttpMethod::Post => "post",
            HttpMethod::Delete => "delete",
            HttpMethod::Patch => "patch",
            HttpMethod::Head => "head",
            HttpMethod::Options => "options",
        }
    }

    fn from_path_item_key(key: &str) -> Option<Self> {
        Some(match key {
            "get" => HttpMethod::Get,
            "put" => HttpMethod::Put,
            "post" => HttpMethod::Post,
            "delete" => HttpMethod::Delete,
            "patch" => HttpMethod::Patch,
            "head" => HttpMethod::Head,
            "options" => HttpMethod::Options,
            _ => return None,
        })
    }
}

impl fmt::Display for HttpMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where a request parameter is carried. `cookie` is deliberately absent: the
/// executor never sends cookies, so a spec that needs them is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParameterLocation {
    Path,
    Query,
    Header,
}

/// One declared request parameter, with its bounded raw schema subtree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogParameter {
    /// Parameter name as declared.
    pub name: String,
    /// Where the parameter is carried.
    pub location: ParameterLocation,
    /// Whether a request must supply the parameter.
    pub required: bool,
    /// The declared JSON schema subtree, `$ref`-free and size-bounded, if the
    /// document declared one.
    pub schema: Option<Value>,
}

/// The declared request body, if the operation takes one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogRequestBody {
    /// Whether a request must supply a body.
    pub required: bool,
    /// The declared `application/json` schema subtree, `$ref`-free and
    /// size-bounded, if the document declared one.
    pub schema: Option<Value>,
}

/// One executable operation the document declared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogOperation {
    /// The document's `operationId` — the name bindings and grants pin.
    pub operation_id: String,
    /// HTTP method the operation uses.
    pub method: HttpMethod,
    /// Path template as declared, e.g. `/repos/{owner}/{repo}/issues`.
    pub path_template: String,
    /// Declared parameters, path-level merged with operation-level
    /// (operation-level wins on the same name and location).
    pub parameters: Vec<CatalogParameter>,
    /// Declared request body, if any.
    pub request_body: Option<CatalogRequestBody>,
}

/// The bounded operation catalog one OpenAPI document ingests to.
///
/// Persisted as JSON inside the connected-app record's definition, so its
/// shape is a durable contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationCatalog {
    /// Lowercase-hex SHA-256 of the raw document bytes, exactly as ingested.
    /// The `rest_api` consent fingerprint hashes over this value.
    pub document_sha256: String,
    /// Every declared operation, keyed by `operationId`.
    pub operations: BTreeMap<String, CatalogOperation>,
}

/// Why an OpenAPI document was refused.
///
/// The enum is closed and never echoes unbounded document text: an
/// `operation_id` or `path` is only carried after it has passed its own byte
/// bound, and identifiers that failed those bounds are described, not quoted.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OpenApiIngestError {
    #[error("OpenAPI document exceeds {} bytes", MAX_OPENAPI_DOCUMENT_BYTES)]
    DocumentTooLarge,
    #[error("only JSON OpenAPI documents are supported; the document is not a JSON object")]
    NotJson,
    #[error("Swagger 2.0 documents are not supported; provide OpenAPI 3.x")]
    SwaggerNotSupported,
    #[error("document does not declare an OpenAPI 3.x version")]
    NotOpenApi3,
    #[error("document declares no operations")]
    NoOperations,
    #[error("document declares more than {} operations", MAX_CATALOG_OPERATIONS)]
    TooManyOperations,
    #[error("operation {method} {path} has no operationId")]
    MissingOperationId { method: HttpMethod, path: String },
    #[error(
        "operation {method} {path} has an operationId over {} bytes or outside [A-Za-z0-9_.-]",
        MAX_OPERATION_ID_BYTES
    )]
    InvalidOperationId { method: HttpMethod, path: String },
    #[error("duplicate operationId {operation_id}")]
    DuplicateOperationId { operation_id: String },
    #[error("a path template exceeds {} bytes", MAX_PATH_TEMPLATE_BYTES)]
    PathTemplateTooLong,
    #[error("malformed path template {path}")]
    MalformedPathTemplate { path: String },
    #[error("operation {operation_id}: template parameters and declared path parameters disagree")]
    PathParameterMismatch { operation_id: String },
    #[error("operation {operation_id}: only path, query, and header parameters are supported")]
    UnsupportedParameterLocation { operation_id: String },
    #[error("operation {operation_id}: malformed parameter object")]
    MalformedParameter { operation_id: String },
    #[error(
        "operation {operation_id}: more than {} parameters",
        MAX_OPERATION_PARAMETERS
    )]
    TooManyParameters { operation_id: String },
    #[error(
        "operation {operation_id}: a parameter name exceeds {} bytes",
        MAX_PARAMETER_NAME_BYTES
    )]
    ParameterNameTooLong { operation_id: String },
    #[error(
        "operation {operation_id}: a schema subtree exceeds {} serialized bytes",
        MAX_SCHEMA_SUBTREE_BYTES
    )]
    SchemaTooLarge { operation_id: String },
    #[error("operation {operation_id}: a $ref is not a resolvable internal components reference")]
    UnresolvableRef { operation_id: String },
    #[error(
        "operation {operation_id}: $ref resolution exceeded depth {}",
        MAX_REF_RESOLUTION_DEPTH
    )]
    RefDepthExceeded { operation_id: String },
    #[error("malformed OpenAPI structure: {context}")]
    MalformedStructure { context: &'static str },
}

/// Ingest an OpenAPI document into a bounded [`OperationCatalog`], or refuse.
///
/// JSON, OpenAPI 3.x documents only. Every declared operation must carry a
/// well-formed `operationId`; any operation the catalog cannot represent
/// within its bounds refuses the whole document rather than being skipped.
pub fn ingest_openapi_document(document: &[u8]) -> Result<OperationCatalog, OpenApiIngestError> {
    if document.len() > MAX_OPENAPI_DOCUMENT_BYTES {
        return Err(OpenApiIngestError::DocumentTooLarge);
    }
    // Leading-bytes heuristic: a JSON OpenAPI document is always a JSON
    // object, so anything not opening with `{` (a YAML spec, most obviously)
    // gets the JSON-only refusal without attempting a parse.
    let opens_as_object = document
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| *byte == b'{');
    if !opens_as_object {
        return Err(OpenApiIngestError::NotJson);
    }
    let root: Value = serde_json::from_slice(document).map_err(|_| OpenApiIngestError::NotJson)?;
    let Value::Object(ref root_object) = root else {
        return Err(OpenApiIngestError::NotJson);
    };

    if root_object.contains_key("swagger") {
        return Err(OpenApiIngestError::SwaggerNotSupported);
    }
    let declares_openapi_3 = root_object
        .get("openapi")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with("3."));
    if !declares_openapi_3 {
        return Err(OpenApiIngestError::NotOpenApi3);
    }

    let paths = match root_object.get("paths") {
        None | Some(Value::Object(_)) => root_object.get("paths").and_then(Value::as_object),
        Some(_) => {
            return Err(OpenApiIngestError::MalformedStructure {
                context: "paths is not an object",
            })
        }
    };

    let mut operations = BTreeMap::new();
    for (path, path_item) in paths.into_iter().flatten() {
        if path.len() > MAX_PATH_TEMPLATE_BYTES {
            return Err(OpenApiIngestError::PathTemplateTooLong);
        }
        let template_parameters = parse_path_template(path)?;
        let Value::Object(path_item) = path_item else {
            return Err(OpenApiIngestError::MalformedStructure {
                context: "a path item is not an object",
            });
        };
        let path_level_parameters = path_item.get("parameters");
        for (key, operation_value) in path_item {
            let Some(method) = HttpMethod::from_path_item_key(key) else {
                continue;
            };
            let operation = ingest_operation(
                &root,
                method,
                path,
                operation_value,
                path_level_parameters,
                &template_parameters,
            )?;
            if operations.len() == MAX_CATALOG_OPERATIONS {
                return Err(OpenApiIngestError::TooManyOperations);
            }
            let operation_id = operation.operation_id.clone();
            if operations.insert(operation_id.clone(), operation).is_some() {
                return Err(OpenApiIngestError::DuplicateOperationId { operation_id });
            }
        }
    }
    if operations.is_empty() {
        return Err(OpenApiIngestError::NoOperations);
    }

    let mut hasher = Sha256::new();
    hasher.update(document);
    let document_sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    Ok(OperationCatalog {
        document_sha256,
        operations,
    })
}

/// Validate one path template and return its `{param}` names.
///
/// The template must start with `/`; braces must pair without nesting; each
/// parameter name must be non-empty, free of `/`, and unique in the template.
fn parse_path_template(path: &str) -> Result<BTreeSet<String>, OpenApiIngestError> {
    let malformed = || OpenApiIngestError::MalformedPathTemplate {
        path: path.to_string(),
    };
    if !path.starts_with('/') {
        return Err(malformed());
    }
    let mut names = BTreeSet::new();
    let mut current: Option<String> = None;
    for character in path.chars() {
        match (character, &mut current) {
            ('{', None) => current = Some(String::new()),
            ('{', Some(_)) => return Err(malformed()),
            ('}', Some(name)) => {
                if name.is_empty() || !names.insert(std::mem::take(name)) {
                    return Err(malformed());
                }
                current = None;
            }
            ('}', None) => return Err(malformed()),
            ('/', Some(_)) => return Err(malformed()),
            (other, Some(name)) => name.push(other),
            (_, None) => {}
        }
    }
    if current.is_some() {
        return Err(malformed());
    }
    Ok(names)
}

/// Build one catalog operation, refusing anything the catalog cannot bound.
fn ingest_operation(
    root: &Value,
    method: HttpMethod,
    path: &str,
    operation_value: &Value,
    path_level_parameters: Option<&Value>,
    template_parameters: &BTreeSet<String>,
) -> Result<CatalogOperation, OpenApiIngestError> {
    let Value::Object(operation_object) = operation_value else {
        return Err(OpenApiIngestError::MalformedStructure {
            context: "an operation is not an object",
        });
    };
    let operation_id = match operation_object.get("operationId").and_then(Value::as_str) {
        Some(id) => id,
        None => {
            return Err(OpenApiIngestError::MissingOperationId {
                method,
                path: path.to_string(),
            })
        }
    };
    let well_formed_id = !operation_id.is_empty()
        && operation_id.len() <= MAX_OPERATION_ID_BYTES
        && operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if !well_formed_id {
        return Err(OpenApiIngestError::InvalidOperationId {
            method,
            path: path.to_string(),
        });
    }

    // Path-level parameters apply to every operation on the path; an
    // operation-level declaration with the same name and location replaces
    // the path-level one.
    let mut merged: BTreeMap<(String, ParameterLocation), CatalogParameter> = BTreeMap::new();
    for source in [path_level_parameters, operation_object.get("parameters")]
        .into_iter()
        .flatten()
    {
        let Value::Array(declared) = source else {
            return Err(OpenApiIngestError::MalformedParameter {
                operation_id: operation_id.to_string(),
            });
        };
        for parameter in declared {
            let parameter = ingest_parameter(root, operation_id, parameter)?;
            merged.insert((parameter.name.clone(), parameter.location), parameter);
        }
    }
    if merged.len() > MAX_OPERATION_PARAMETERS {
        return Err(OpenApiIngestError::TooManyParameters {
            operation_id: operation_id.to_string(),
        });
    }

    // Every template parameter must be declared as a required path parameter
    // and vice versa, so the executor can always bind a full concrete path.
    let declared_path_parameters: BTreeSet<String> = merged
        .values()
        .filter(|parameter| parameter.location == ParameterLocation::Path)
        .map(|parameter| parameter.name.clone())
        .collect();
    let all_path_parameters_required = merged
        .values()
        .filter(|parameter| parameter.location == ParameterLocation::Path)
        .all(|parameter| parameter.required);
    if declared_path_parameters != *template_parameters || !all_path_parameters_required {
        return Err(OpenApiIngestError::PathParameterMismatch {
            operation_id: operation_id.to_string(),
        });
    }

    let request_body = operation_object
        .get("requestBody")
        .map(|body| ingest_request_body(root, operation_id, body))
        .transpose()?;

    Ok(CatalogOperation {
        operation_id: operation_id.to_string(),
        method,
        path_template: path.to_string(),
        parameters: merged.into_values().collect(),
        request_body,
    })
}

/// Ingest one declared parameter, resolving a `#/components/parameters/` ref.
fn ingest_parameter(
    root: &Value,
    operation_id: &str,
    declared: &Value,
) -> Result<CatalogParameter, OpenApiIngestError> {
    let declared = resolve_refs(root, operation_id, declared, 0)?;
    let malformed = || OpenApiIngestError::MalformedParameter {
        operation_id: operation_id.to_string(),
    };
    let Value::Object(parameter) = &declared else {
        return Err(malformed());
    };
    let name = parameter
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(malformed)?;
    if name.is_empty() {
        return Err(malformed());
    }
    if name.len() > MAX_PARAMETER_NAME_BYTES {
        return Err(OpenApiIngestError::ParameterNameTooLong {
            operation_id: operation_id.to_string(),
        });
    }
    let location = match parameter.get("in").and_then(Value::as_str) {
        Some("path") => ParameterLocation::Path,
        Some("query") => ParameterLocation::Query,
        Some("header") => ParameterLocation::Header,
        Some(_) => {
            return Err(OpenApiIngestError::UnsupportedParameterLocation {
                operation_id: operation_id.to_string(),
            })
        }
        None => return Err(malformed()),
    };
    let required = parameter
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let schema = parameter
        .get("schema")
        .map(|schema| bounded_schema(operation_id, schema))
        .transpose()?;
    Ok(CatalogParameter {
        name: name.to_string(),
        location,
        required,
        schema,
    })
}

/// Ingest the declared request body: presence, `required`, and the
/// `application/json` schema subtree if one is declared.
fn ingest_request_body(
    root: &Value,
    operation_id: &str,
    declared: &Value,
) -> Result<CatalogRequestBody, OpenApiIngestError> {
    // A request-body `$ref` would point at `#/components/requestBodies/...`,
    // which is outside the two resolvable component sections, so the resolver
    // refuses it — exactly the fail-closed answer for v1.
    let declared = resolve_refs(root, operation_id, declared, 0)?;
    let Value::Object(body) = &declared else {
        return Err(OpenApiIngestError::MalformedStructure {
            context: "a requestBody is not an object",
        });
    };
    let required = body
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let schema = body
        .get("content")
        .and_then(Value::as_object)
        .and_then(|content| content.get("application/json"))
        .and_then(Value::as_object)
        .and_then(|media_type| media_type.get("schema"))
        .map(|schema| bounded_schema(operation_id, schema))
        .transpose()?;
    Ok(CatalogRequestBody { required, schema })
}

/// Admit one already-resolved schema subtree under the serialized-size bound.
fn bounded_schema(operation_id: &str, schema: &Value) -> Result<Value, OpenApiIngestError> {
    let serialized_length = serde_json::to_string(schema)
        .map(|serialized| serialized.len())
        .unwrap_or(usize::MAX);
    if serialized_length > MAX_SCHEMA_SUBTREE_BYTES {
        return Err(OpenApiIngestError::SchemaTooLarge {
            operation_id: operation_id.to_string(),
        });
    }
    Ok(schema.clone())
}

/// Recursively replace every internal `$ref` with its target so the stored
/// subtree is self-contained.
///
/// Only `#/components/parameters/<name>` and `#/components/schemas/<name>`
/// are resolvable; any other `$ref` — external URL, file path, other pointer
/// — refuses. `depth` counts followed refs, so a cycle exhausts
/// [`MAX_REF_RESOLUTION_DEPTH`] and refuses instead of looping.
fn resolve_refs(
    root: &Value,
    operation_id: &str,
    value: &Value,
    depth: usize,
) -> Result<Value, OpenApiIngestError> {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref") {
                if depth >= MAX_REF_RESOLUTION_DEPTH {
                    return Err(OpenApiIngestError::RefDepthExceeded {
                        operation_id: operation_id.to_string(),
                    });
                }
                let target = reference
                    .as_str()
                    .and_then(|pointer| lookup_components_ref(root, pointer))
                    .ok_or_else(|| OpenApiIngestError::UnresolvableRef {
                        operation_id: operation_id.to_string(),
                    })?;
                // Per the OpenAPI reference-object rules, siblings of `$ref`
                // are ignored: the target replaces the whole object.
                return resolve_refs(root, operation_id, target, depth + 1);
            }
            let mut resolved = Map::with_capacity(object.len());
            for (key, entry) in object {
                resolved.insert(key.clone(), resolve_refs(root, operation_id, entry, depth)?);
            }
            Ok(Value::Object(resolved))
        }
        Value::Array(entries) => entries
            .iter()
            .map(|entry| resolve_refs(root, operation_id, entry, depth))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

/// Look up an internal components reference, or `None` if the pointer is not
/// one of the two resolvable sections or has no target in the document.
fn lookup_components_ref<'document>(
    root: &'document Value,
    pointer: &str,
) -> Option<&'document Value> {
    let name = pointer
        .strip_prefix("#/components/parameters/")
        .or_else(|| pointer.strip_prefix("#/components/schemas/"))?;
    // A nested pointer (`Foo/properties/bar`) is not a plain component name;
    // refuse it rather than guessing at JSON-pointer semantics.
    if name.is_empty() || name.contains('/') || name.contains('~') {
        return None;
    }
    let section = if pointer.starts_with("#/components/parameters/") {
        "parameters"
    } else {
        "schemas"
    };
    root.get("components")?.get(section)?.get(name)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn document(paths: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "openapi": "3.1.0",
            "info": { "title": "t", "version": "1" },
            "servers": [{ "url": "https://ignored.example" }],
            "paths": paths,
        }))
        .unwrap()
    }

    #[test]
    fn realistic_spec_ingests_to_a_selfcontained_catalog_that_round_trips() {
        let bytes = serde_json::to_vec(&json!({
            "openapi": "3.0.3",
            "info": { "title": "issues", "version": "1" },
            "servers": [{ "url": "https://api.example.com" }],
            "components": {
                "parameters": {
                    "Owner": {
                        "name": "owner", "in": "path", "required": true,
                        "schema": { "type": "string" }
                    }
                },
                "schemas": {
                    "Issue": {
                        "type": "object",
                        "properties": { "title": { "$ref": "#/components/schemas/Title" } }
                    },
                    "Title": { "type": "string" }
                }
            },
            "paths": {
                "/repos/{owner}/{repo}/issues": {
                    "parameters": [
                        { "$ref": "#/components/parameters/Owner" },
                        { "name": "repo", "in": "path", "required": true },
                        { "name": "page", "in": "query", "schema": { "type": "integer" } }
                    ],
                    "get": {
                        "operationId": "issues.list",
                        "parameters": [
                            // Overrides the path-level `page` on same name+location.
                            { "name": "page", "in": "query", "required": true,
                              "schema": { "type": "integer", "minimum": 1 } },
                            { "name": "x-trace", "in": "header" }
                        ]
                    },
                    "post": {
                        "operationId": "issues.create",
                        "requestBody": {
                            "required": true,
                            "content": {
                                "application/json": {
                                    "schema": { "$ref": "#/components/schemas/Issue" }
                                }
                            }
                        }
                    }
                }
            }
        }))
        .unwrap();

        let catalog = ingest_openapi_document(&bytes).unwrap();

        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let expected_digest: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(catalog.document_sha256, expected_digest);

        assert_eq!(
            catalog.operations.keys().collect::<Vec<_>>(),
            ["issues.create", "issues.list"]
        );

        let list = &catalog.operations["issues.list"];
        assert_eq!(list.method, HttpMethod::Get);
        assert_eq!(list.path_template, "/repos/{owner}/{repo}/issues");
        assert_eq!(list.parameters.len(), 4);
        let page = list
            .parameters
            .iter()
            .find(|parameter| parameter.name == "page")
            .unwrap();
        // The operation-level declaration won the merge.
        assert!(page.required);
        assert_eq!(
            page.schema,
            Some(json!({ "type": "integer", "minimum": 1 }))
        );
        let owner = list
            .parameters
            .iter()
            .find(|parameter| parameter.name == "owner")
            .unwrap();
        assert_eq!(owner.location, ParameterLocation::Path);
        assert_eq!(owner.schema, Some(json!({ "type": "string" })));

        let create = &catalog.operations["issues.create"];
        assert_eq!(create.method, HttpMethod::Post);
        let body = create.request_body.as_ref().unwrap();
        assert!(body.required);
        // Nested refs resolved away: the stored subtree is self-contained.
        assert_eq!(
            body.schema,
            Some(json!({
                "type": "object",
                "properties": { "title": { "type": "string" } }
            }))
        );
        assert!(!serde_json::to_string(&catalog).unwrap().contains("$ref"));

        // The catalog is a persisted contract shape: it must survive a JSON
        // round trip unchanged.
        let serialized = serde_json::to_string(&catalog).unwrap();
        let restored: OperationCatalog = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, catalog);
    }

    #[test]
    fn every_refusal_class_refuses_the_whole_document() {
        let minimal_get = |operation_id: &str| json!({ "get": { "operationId": operation_id } });
        let cases: Vec<(&str, Vec<u8>, OpenApiIngestError)> = vec![
            (
                "over the byte bound",
                vec![b' '; MAX_OPENAPI_DOCUMENT_BYTES + 1],
                OpenApiIngestError::DocumentTooLarge,
            ),
            (
                "YAML gets the JSON-only refusal",
                b"openapi: \"3.1.0\"\npaths: {}\n".to_vec(),
                OpenApiIngestError::NotJson,
            ),
            (
                "JSON but not an object",
                b"[1, 2]".to_vec(),
                OpenApiIngestError::NotJson,
            ),
            (
                "Swagger 2.0 refused distinctly",
                serde_json::to_vec(&json!({ "swagger": "2.0", "paths": {} })).unwrap(),
                OpenApiIngestError::SwaggerNotSupported,
            ),
            (
                "no OpenAPI 3.x version",
                serde_json::to_vec(&json!({ "openapi": "4.0.0", "paths": {} })).unwrap(),
                OpenApiIngestError::NotOpenApi3,
            ),
            (
                "zero operations",
                document(json!({})),
                OpenApiIngestError::NoOperations,
            ),
            (
                "operation without operationId",
                document(json!({ "/a": { "get": {} } })),
                OpenApiIngestError::MissingOperationId {
                    method: HttpMethod::Get,
                    path: "/a".to_string(),
                },
            ),
            (
                "operationId outside the charset",
                document(json!({ "/a": minimal_get("no spaces") })),
                OpenApiIngestError::InvalidOperationId {
                    method: HttpMethod::Get,
                    path: "/a".to_string(),
                },
            ),
            (
                "operationId over the byte bound",
                document(json!({ "/a": minimal_get(&"x".repeat(MAX_OPERATION_ID_BYTES + 1)) })),
                OpenApiIngestError::InvalidOperationId {
                    method: HttpMethod::Get,
                    path: "/a".to_string(),
                },
            ),
            (
                "duplicate operationIds",
                document(json!({
                    "/a": minimal_get("op"),
                    "/b": minimal_get("op"),
                })),
                OpenApiIngestError::DuplicateOperationId {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "path template over the byte bound",
                document(json!({
                    (format!("/{}", "a".repeat(MAX_PATH_TEMPLATE_BYTES))): minimal_get("op")
                })),
                OpenApiIngestError::PathTemplateTooLong,
            ),
            (
                "path template must start with a slash",
                document(json!({ "a": minimal_get("op") })),
                OpenApiIngestError::MalformedPathTemplate {
                    path: "a".to_string(),
                },
            ),
            (
                "unclosed template brace",
                document(json!({ "/a/{id": minimal_get("op") })),
                OpenApiIngestError::MalformedPathTemplate {
                    path: "/a/{id".to_string(),
                },
            ),
            (
                "empty template parameter",
                document(json!({ "/a/{}": minimal_get("op") })),
                OpenApiIngestError::MalformedPathTemplate {
                    path: "/a/{}".to_string(),
                },
            ),
            (
                "template parameter never declared",
                document(json!({ "/a/{id}": minimal_get("op") })),
                OpenApiIngestError::PathParameterMismatch {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "declared path parameter not in the template",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "parameters": [{ "name": "id", "in": "path", "required": true }]
                } } })),
                OpenApiIngestError::PathParameterMismatch {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "path parameter declared optional",
                document(json!({ "/a/{id}": { "get": {
                    "operationId": "op",
                    "parameters": [{ "name": "id", "in": "path" }]
                } } })),
                OpenApiIngestError::PathParameterMismatch {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "cookie parameters unsupported",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "parameters": [{ "name": "session", "in": "cookie" }]
                } } })),
                OpenApiIngestError::UnsupportedParameterLocation {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "external $ref refused",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "parameters": [{ "$ref": "https://evil.example/spec.json#/p" }]
                } } })),
                OpenApiIngestError::UnresolvableRef {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "internal $ref outside the resolvable sections",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "requestBody": { "$ref": "#/components/requestBodies/Body" }
                } } })),
                OpenApiIngestError::UnresolvableRef {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "$ref with no target in the document",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "parameters": [{ "$ref": "#/components/parameters/Missing" }]
                } } })),
                OpenApiIngestError::UnresolvableRef {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "too many parameters after merge",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "parameters": (0..=MAX_OPERATION_PARAMETERS)
                        .map(|index| json!({ "name": format!("p{index}"), "in": "query" }))
                        .collect::<Vec<_>>()
                } } })),
                OpenApiIngestError::TooManyParameters {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "parameter name over the byte bound",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "parameters": [
                        { "name": "n".repeat(MAX_PARAMETER_NAME_BYTES + 1), "in": "query" }
                    ]
                } } })),
                OpenApiIngestError::ParameterNameTooLong {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "schema subtree over the serialized bound",
                document(json!({ "/a": { "get": {
                    "operationId": "op",
                    "parameters": [{ "name": "q", "in": "query", "schema": {
                        "description": "d".repeat(MAX_SCHEMA_SUBTREE_BYTES)
                    } }]
                } } })),
                OpenApiIngestError::SchemaTooLarge {
                    operation_id: "op".to_string(),
                },
            ),
            (
                "too many operations",
                document(Value::Object(
                    (0..=MAX_CATALOG_OPERATIONS)
                        .map(|index| (format!("/p{index}"), minimal_get(&format!("op{index}"))))
                        .collect(),
                )),
                OpenApiIngestError::TooManyOperations,
            ),
        ];
        for (case, bytes, expected) in cases {
            assert_eq!(
                ingest_openapi_document(&bytes).unwrap_err(),
                expected,
                "{case}"
            );
        }
    }

    #[test]
    fn ref_chains_resolve_within_the_depth_cap_and_refuse_beyond_it() {
        let spec_with_chain = |links: usize| {
            let mut schemas = Map::new();
            for index in 0..links {
                schemas.insert(
                    format!("S{index}"),
                    json!({ "$ref": format!("#/components/schemas/S{}", index + 1) }),
                );
            }
            schemas.insert(format!("S{links}"), json!({ "type": "string" }));
            serde_json::to_vec(&json!({
                "openapi": "3.1.0",
                "info": { "title": "t", "version": "1" },
                "components": { "schemas": schemas },
                "paths": { "/a": { "get": {
                    "operationId": "op",
                    "parameters": [{ "name": "q", "in": "query",
                        "schema": { "$ref": "#/components/schemas/S0" } }]
                } } }
            }))
            .unwrap()
        };

        // S0 → … → S7 (terminal): eight followed refs, exactly at the cap.
        let catalog = ingest_openapi_document(&spec_with_chain(7)).unwrap();
        assert_eq!(
            catalog.operations["op"].parameters[0].schema,
            Some(json!({ "type": "string" }))
        );

        // One more link exceeds the cap; a cyclic graph hits the same refusal.
        assert_eq!(
            ingest_openapi_document(&spec_with_chain(8)).unwrap_err(),
            OpenApiIngestError::RefDepthExceeded {
                operation_id: "op".to_string(),
            }
        );
        let cyclic = document(json!({ "/a": { "get": {
            "operationId": "op",
            "parameters": [{ "name": "q", "in": "query",
                "schema": { "$ref": "#/components/schemas/Loop" } }]
        } } }));
        let mut cyclic: Value = serde_json::from_slice(&cyclic).unwrap();
        cyclic["components"] =
            json!({ "schemas": { "Loop": { "$ref": "#/components/schemas/Loop" } } });
        assert_eq!(
            ingest_openapi_document(&serde_json::to_vec(&cyclic).unwrap()).unwrap_err(),
            OpenApiIngestError::RefDepthExceeded {
                operation_id: "op".to_string(),
            }
        );
    }
}
