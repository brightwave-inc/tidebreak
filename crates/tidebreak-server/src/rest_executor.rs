//! Governed REST executor: perform one declared operation against a
//! `rest_api` connected app on behalf of a caller.
//!
//! The executor is the only code that turns a caller's operation request into
//! bytes on the wire, and every guarantee it makes is server-side and
//! fail-closed:
//!
//! - The request is validated against the ingested
//!   [`OperationCatalog`](crate::openapi_catalog::OperationCatalog) *before any
//!   I/O* — an undeclared operation, an undeclared parameter, a missing
//!   required parameter, or a value outside its declared schema refuses the
//!   request outright. The catalog, not the caller, decides what is
//!   executable.
//! - Egress mirrors the native web-search fetcher's posture: the base URL is
//!   admitted (https, or plain HTTP only to an explicit loopback IP literal
//!   after opt-in), no userinfo, no fragment, the host's DNS answer is
//!   resolved and vetted per request with the same denied-network list, the
//!   connection is pinned to exactly the vetted addresses, redirects are never
//!   followed, and request and response byte counts and wall time are capped.
//!   Unlike the web-search policy, an explicit port is allowed — a REST base
//!   URL may legitimately pin one — and refusals distinguish an unresolvable
//!   host from a denied address, because the base URL is operator
//!   configuration rather than a model-chosen name; resolved addresses are
//!   still never echoed.
//! - The credential is a *reference* into the profile secret store. Its value
//!   is resolved only here, at request time, injected as a header, and never
//!   appears in `Debug` output, error text, or logs — the transport request's
//!   `Debug` impl redacts every header value, and transport failures are
//!   stripped of URLs before their message is kept.
//!
//! Response bounds are sized for interactive UIs, not model context windows —
//! deliberately larger than the MCP tool-result clamp — per the connected-apps
//! design (`docs/connected-apps.md`).
//!
//! This slice is standalone: inputs are plain values ([`RestApiTarget`],
//! [`RestOperationRequest`]); wiring to the connected-app record arrives in
//! later slices.

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::web_search::admit_fetch_address;
use tidebreak_core::SecretProvider;

use crate::openapi_catalog::{CatalogOperation, HttpMethod, OperationCatalog, ParameterLocation};

/// Largest JSON-serialized request body sent, in bytes. Matches the
/// app-invoke route's body bound so a binding cannot smuggle more through the
/// executor than the invoke surface accepts.
pub const MAX_REST_REQUEST_BODY_BYTES: usize = 256 * 1024;
/// Largest response body retained, in bytes. Deliberately larger than the
/// 1 MiB MCP tool-result clamp: responses feed interactive UIs, not model
/// context windows.
pub const MAX_REST_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Longest base URL accepted, in bytes.
pub const MAX_REST_BASE_URL_BYTES: usize = 2048;
/// Longest rendered path or query parameter value, in bytes. With the
/// catalog's parameter-count bound this caps the assembled URL.
pub const MAX_REST_PARAMETER_VALUE_BYTES: usize = 4 * 1024;
/// Longest header parameter value, in bytes. Headers travel uncompressed on
/// every hop, so their bound is tighter than the URL parameters'.
pub const MAX_REST_HEADER_VALUE_BYTES: usize = 1024;
/// Shortest per-request timeout a caller may ask for.
pub const MIN_REST_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest per-request timeout a caller may ask for.
pub const MAX_REST_TIMEOUT: Duration = Duration::from_secs(60);
/// Timeout used when the caller does not supply one.
pub const DEFAULT_REST_TIMEOUT: Duration = Duration::from_secs(30);
/// The honest, distinct user agent every executed operation announces.
pub const REST_EXECUTOR_USER_AGENT: &str =
    "TidebreakConnectedApp/1.0 (+https://github.com/brightwave-inc/tidebreak)";

/// The REST API one operation executes against: where to send the request and
/// which stored credential (if any) authenticates it.
///
/// The base URL may carry a path prefix (`https://api.example.com/v2`); the
/// operation's path template appends to it. Credential-less targets (public
/// APIs) execute through exactly the same governed path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestApiTarget {
    /// Base URL the operation path appends to. Admitted per request; https
    /// only unless [`Self::allow_loopback_http`] admits a loopback IP
    /// literal. No userinfo, no fragment, explicit ports allowed.
    pub base_url: String,
    /// Stored credential reference and placement, when the API needs one.
    pub credential: Option<RestCredential>,
    /// Explicit consent to send this record's traffic as plain HTTP to a
    /// loopback IP literal (127.0.0.0/8 or `::1`). DNS names, including
    /// `localhost`, never qualify.
    #[serde(default)]
    pub allow_loopback_http: bool,
}

/// A credential *reference*: the profile secret-store key and where the value
/// goes. The value itself is resolved from the store only inside the
/// executor, at request time — it never travels in this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestCredential {
    /// Profile secret-store key holding the credential value.
    pub secret_name: String,
    /// Header the resolved value is injected into.
    pub placement: CredentialPlacement,
}

/// Where the resolved credential value is placed on the request.
///
/// Externally tagged and closed: `"bearer"` or `{"header": "X-Api-Key"}`; an
/// unknown variant refuses to deserialize. A named header must be a valid
/// header token and may name `Authorization` explicitly, but never a header
/// the executor owns or that alters routing (see [`RestExecuteError::ForbiddenHeader`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum CredentialPlacement {
    /// `Authorization: Bearer {value}`.
    Bearer,
    /// `{name}: {value}` for an explicitly named header.
    Header(String),
}

/// One caller-supplied operation request, validated against the catalog
/// before anything else happens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestOperationRequest {
    /// The catalog `operationId` to execute.
    pub operation_id: String,
    /// Supplied parameter values as a JSON object of name → value. Every name
    /// must be declared by the operation; values must be JSON scalars.
    pub parameters: Value,
    /// JSON request body, only when the operation declares one.
    pub body: Option<Value>,
}

/// The bounded response of one executed operation, returned verbatim —
/// including redirect statuses, which the executor reports rather than
/// follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestOperationResponse {
    /// HTTP status code.
    pub status: u16,
    /// Raw `Content-Type` header value, when present.
    pub content_type: Option<String>,
    /// Response body, at most [`MAX_REST_RESPONSE_BYTES`].
    pub body: Vec<u8>,
}

/// Why one operation request was refused or failed.
///
/// Closed, one variant per refusal class, and deliberately quiet: no variant
/// ever carries the credential value, a resolved address, or unbounded
/// attacker-controlled text. Parameter and header names are echoed because
/// they come from the caller's own request or the operator's own catalog and
/// are byte-bounded by ingest.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RestExecuteError {
    #[error("operation {operation_id} is not declared by the connected app")]
    UnknownOperation { operation_id: String },
    #[error("request parameters must be a JSON object of parameter name to value")]
    ParametersNotAnObject,
    #[error("parameter {name} is not declared by the operation")]
    UndeclaredParameter { name: String },
    #[error("required parameter {name} was not supplied")]
    MissingParameter { name: String },
    #[error("parameter {name} does not match its declared schema")]
    InvalidParameter { name: String },
    #[error("parameter {name} cannot be carried in its declared location")]
    UnrepresentableParameter { name: String },
    #[error("header {name} may not be set by a parameter or credential placement")]
    ForbiddenHeader { name: String },
    #[error("the operation does not declare a request body")]
    UndeclaredBody,
    #[error("the operation requires a request body")]
    MissingBody,
    #[error("request body does not match the declared schema")]
    InvalidBody,
    #[error("request body exceeds {} bytes", MAX_REST_REQUEST_BODY_BYTES)]
    BodyTooLarge,
    #[error("connected app base URL is not admissible: {reason}")]
    InadmissibleBaseUrl { reason: &'static str },
    /// The host resolved, but to address space the executor refuses to dial.
    /// Which address it was stays inside the check that refused it.
    #[error("connected app address is not an allowed destination")]
    DeniedAddress,
    #[error("connected app host could not be resolved")]
    UnresolvableHost,
    #[error("connected app credential is not present in the secret store")]
    MissingCredential,
    #[error("connected app credential could not be read from the secret store")]
    SecretStoreUnavailable,
    /// The stored value cannot travel as a header (control bytes or
    /// non-ASCII). Refused here so it can never reach a transport error that
    /// might echo it.
    #[error("connected app credential value cannot be carried in a header")]
    UnusableCredential,
    #[error("response exceeded {} bytes", MAX_REST_RESPONSE_BYTES)]
    ResponseTooLarge,
    #[error("request exceeded its time budget")]
    Timeout,
    #[error("request transport failed: {0}")]
    Transport(String),
}

/// One fully validated, vetted outbound request handed to the transport.
///
/// By the time this exists, every refusal has already happened: the URL is
/// admitted and assembled, the addresses are the vetted resolution of its
/// host, and the headers include the resolved credential. Implementations
/// must connect only to `addresses` and never follow redirects.
#[derive(Clone)]
pub struct RestTransportRequest {
    /// HTTP method of the declared operation.
    pub method: HttpMethod,
    /// Fully assembled request URL.
    pub url: Url,
    /// Vetted addresses the connection must be pinned to. Empty only for an
    /// IP-literal host already admitted by the URL check — the literal is
    /// then in the URL itself.
    pub addresses: Vec<IpAddr>,
    /// Header name/value pairs, including the injected credential.
    pub headers: Vec<(String, String)>,
    /// JSON-serialized request body, when the operation carries one.
    pub body: Option<Vec<u8>>,
    /// Whole-request wall-time budget, already clamped.
    pub timeout: Duration,
}

impl fmt::Debug for RestTransportRequest {
    /// Header values are redacted wholesale: one of them is the resolved
    /// credential, and `Debug` output flows into logs and error context that
    /// must never carry it. Names are kept — they are configuration, and they
    /// are what a diagnostic needs.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RestTransportRequest")
            .field("method", &self.method)
            .field("url", &self.url.as_str())
            .field(
                "addresses",
                &format_args!("<{} vetted>", self.addresses.len()),
            )
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| (name.as_str(), "***"))
                    .collect::<Vec<_>>(),
            )
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Transport seam: dispatch one vetted request. Implementations must pin the
/// connection to the request's addresses (which rules out an ambient proxy —
/// a proxied connection dials the proxy, not the vetted address), refuse to
/// follow redirects, and cap the retained body at
/// [`MAX_REST_RESPONSE_BYTES`].
#[async_trait]
pub trait RestTransport: Send + Sync {
    async fn execute(
        &self,
        request: &RestTransportRequest,
    ) -> Result<RestOperationResponse, RestExecuteError>;
}

/// Fresh DNS resolution for one host, called once per executed request so the
/// vetting always judges the answer the connection will actually use.
#[async_trait]
pub trait RestHostResolver: Send + Sync {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, RestExecuteError>;
}

/// The governed executor, backed by an injected transport, resolver, and
/// secret store. It has no default constructor: the host must explicitly
/// decide the transport policy and where credentials come from.
pub struct RestExecutor<T, R> {
    transport: T,
    resolver: R,
    secrets: Arc<dyn SecretProvider>,
}

impl<T: RestTransport, R: RestHostResolver> RestExecutor<T, R> {
    pub fn new(transport: T, resolver: R, secrets: Arc<dyn SecretProvider>) -> Self {
        Self {
            transport,
            resolver,
            secrets,
        }
    }

    /// Execute one declared operation, or refuse.
    ///
    /// All catalog validation happens before any I/O — before DNS, before the
    /// secret store is read, before a connection exists. `timeout` is clamped
    /// to `[`[`MIN_REST_TIMEOUT`]`, `[`MAX_REST_TIMEOUT`]`]` and defaults to
    /// [`DEFAULT_REST_TIMEOUT`].
    pub async fn execute(
        &self,
        target: &RestApiTarget,
        catalog: &OperationCatalog,
        request: &RestOperationRequest,
        timeout: Option<Duration>,
    ) -> Result<RestOperationResponse, RestExecuteError> {
        let Some(operation) = catalog.operations.get(&request.operation_id) else {
            return Err(RestExecuteError::UnknownOperation {
                operation_id: request.operation_id.clone(),
            });
        };

        // The header the credential placement will write, decided up front so
        // parameter validation can refuse a declared header that collides
        // with it: the executor owns that header for this request.
        let placement_header = match target.credential.as_ref().map(|c| &c.placement) {
            None => None,
            Some(CredentialPlacement::Bearer) => Some("authorization".to_owned()),
            Some(CredentialPlacement::Header(name)) => {
                require_placement_header_name(name)?;
                Some(name.to_ascii_lowercase())
            }
        };

        let rendered = render_parameters(operation, request, placement_header.as_deref())?;
        let body_bytes = serialize_body(operation, request)?;

        let base = admit_base_url(&target.base_url, target.allow_loopback_http)?;
        let url = assemble_url(&base, &rendered)?;
        pin_to_admitted_origin(&base, &url)?;

        // Vet the destination. A domain host is resolved freshly and every
        // answer must clear the denied-network list — refusing the whole name
        // when any answer is private, because picking just a public answer
        // would make admission depend on resolver ordering and reopen a DNS
        // rebinding path on the connection that follows.
        let addresses = match url.domain() {
            Some(domain) => {
                let resolved = self.resolver.resolve(domain).await?;
                if resolved.is_empty() {
                    return Err(RestExecuteError::UnresolvableHost);
                }
                if resolved
                    .iter()
                    .any(|address| admit_fetch_address(*address).is_err())
                {
                    return Err(RestExecuteError::DeniedAddress);
                }
                resolved
            }
            None => {
                // `admit_base_url` guaranteed a host, so a non-domain host is
                // an IP literal (the URL parser normalizes decimal, hex, and
                // octal encodings into one). It was admitted there; the
                // transport dials the literal itself.
                Vec::new()
            }
        };

        let mut headers = rendered.headers;
        if body_bytes.is_some() {
            headers.push(("content-type".to_owned(), "application/json".to_owned()));
        }
        // The credential value exists only from here to the transport call:
        // resolved at request time, placed as a header, never stored, never
        // formatted.
        if let Some(credential) = &target.credential {
            let value = self
                .secrets
                .get_secret(&credential.secret_name)
                .await
                .map_err(|_| RestExecuteError::SecretStoreUnavailable)?
                .ok_or(RestExecuteError::MissingCredential)?;
            if value.is_empty() || !is_printable_ascii(&value) {
                return Err(RestExecuteError::UnusableCredential);
            }
            let (name, value) = match &credential.placement {
                CredentialPlacement::Bearer => {
                    ("authorization".to_owned(), format!("Bearer {value}"))
                }
                CredentialPlacement::Header(name) => (name.to_ascii_lowercase(), value),
            };
            headers.push((name, value));
        }

        let timeout = timeout
            .unwrap_or(DEFAULT_REST_TIMEOUT)
            .clamp(MIN_REST_TIMEOUT, MAX_REST_TIMEOUT);

        let transport_request = RestTransportRequest {
            method: operation.method,
            url,
            addresses,
            headers,
            body: body_bytes,
            timeout,
        };
        let response = self.transport.execute(&transport_request).await?;
        // Custom transports are held to the same cap as the real one.
        if response.body.len() > MAX_REST_RESPONSE_BYTES {
            return Err(RestExecuteError::ResponseTooLarge);
        }
        Ok(response)
    }
}

/// Parameters rendered into their wire locations.
struct RenderedParameters {
    /// Substituted, percent-encoded operation path (template placeholders
    /// resolved).
    path: String,
    /// Fully encoded query pairs, in declaration order.
    query: Vec<(String, String)>,
    /// Header parameters (names lowercased), before the credential joins.
    headers: Vec<(String, String)>,
}

fn render_parameters(
    operation: &CatalogOperation,
    request: &RestOperationRequest,
    placement_header: Option<&str>,
) -> Result<RenderedParameters, RestExecuteError> {
    let Some(supplied) = request.parameters.as_object() else {
        return Err(RestExecuteError::ParametersNotAnObject);
    };
    // Undeclared names refuse before anything is rendered: the catalog is the
    // whole vocabulary, and a name it does not declare must not reach the
    // wire in any location.
    for name in supplied.keys() {
        if !operation
            .parameters
            .iter()
            .any(|parameter| parameter.name == *name)
        {
            return Err(RestExecuteError::UndeclaredParameter { name: name.clone() });
        }
    }

    let mut path_values: Vec<(String, String)> = Vec::new();
    let mut query: Vec<(String, String)> = Vec::new();
    let mut headers: Vec<(String, String)> = Vec::new();
    for parameter in &operation.parameters {
        let Some(value) = supplied.get(&parameter.name) else {
            if parameter.required {
                return Err(RestExecuteError::MissingParameter {
                    name: parameter.name.clone(),
                });
            }
            continue;
        };
        if let Some(schema) = &parameter.schema {
            // Catalog schemas are self-contained by ingest, so no resolver is
            // configured; a schema that still fails to compile refuses the
            // request rather than waving the value through.
            let valid = jsonschema::validator_for(schema)
                .map(|validator| validator.is_valid(value))
                .unwrap_or(false);
            if !valid {
                return Err(RestExecuteError::InvalidParameter {
                    name: parameter.name.clone(),
                });
            }
        }
        // Only JSON scalars render into a URL or header deterministically;
        // arrays, objects, and null are refused rather than guessed at.
        let Some(text) = scalar_text(value) else {
            return Err(RestExecuteError::UnrepresentableParameter {
                name: parameter.name.clone(),
            });
        };
        match parameter.location {
            ParameterLocation::Path | ParameterLocation::Query => {
                if text.len() > MAX_REST_PARAMETER_VALUE_BYTES {
                    return Err(RestExecuteError::UnrepresentableParameter {
                        name: parameter.name.clone(),
                    });
                }
                if parameter.location == ParameterLocation::Path {
                    // A value that *is* a dot segment cannot be carried:
                    // URL parsers (this one and the server's) decode every
                    // encoding of `.` / `..` and apply dot-segment removal,
                    // so it would rewrite the path instead of traveling as a
                    // value. Everything else — including values merely
                    // *containing* `/` or `..` — is percent-encoded and
                    // travels verbatim.
                    if text == "." || text == ".." {
                        return Err(RestExecuteError::UnrepresentableParameter {
                            name: parameter.name.clone(),
                        });
                    }
                    path_values.push((parameter.name.clone(), text));
                } else {
                    query.push((
                        percent_encode_strict(&parameter.name),
                        percent_encode_strict(&text),
                    ));
                }
            }
            ParameterLocation::Header => {
                // A declared header still faces the forbidden-name check: a
                // spec must not be able to declare `Host` as a "parameter",
                // and nothing may write the header the credential placement
                // owns for this request.
                require_header_parameter_name(&parameter.name, placement_header)?;
                if text.len() > MAX_REST_HEADER_VALUE_BYTES || !is_printable_ascii(&text) {
                    return Err(RestExecuteError::UnrepresentableParameter {
                        name: parameter.name.clone(),
                    });
                }
                headers.push((parameter.name.to_ascii_lowercase(), text));
            }
        }
    }

    let path = substitute_path_template(&operation.path_template, &path_values)?;
    Ok(RenderedParameters {
        path,
        query,
        headers,
    })
}

fn serialize_body(
    operation: &CatalogOperation,
    request: &RestOperationRequest,
) -> Result<Option<Vec<u8>>, RestExecuteError> {
    let declared = operation.request_body.as_ref();
    match (declared, request.body.as_ref()) {
        (None, None) => Ok(None),
        (None, Some(_)) => Err(RestExecuteError::UndeclaredBody),
        (Some(declared), None) => {
            if declared.required {
                return Err(RestExecuteError::MissingBody);
            }
            Ok(None)
        }
        (Some(declared), Some(body)) => {
            if let Some(schema) = &declared.schema {
                let valid = jsonschema::validator_for(schema)
                    .map(|validator| validator.is_valid(body))
                    .unwrap_or(false);
                if !valid {
                    return Err(RestExecuteError::InvalidBody);
                }
            }
            let bytes = serde_json::to_vec(body).map_err(|_| RestExecuteError::InvalidBody)?;
            if bytes.len() > MAX_REST_REQUEST_BODY_BYTES {
                return Err(RestExecuteError::BodyTooLarge);
            }
            Ok(Some(bytes))
        }
    }
}

/// Admit an operator-configured base URL, or say precisely why not.
///
/// Same shape as the web-search admission with one deliberate difference: an
/// explicit port is allowed, because a REST base URL may pin one; default-port
/// normalization (`:443` disappearing) is left to the URL parser. A fragment
/// or a query is refused rather than stripped — a base URL is configuration,
/// and configuration that cannot mean anything should be corrected, not
/// silently rewritten.
pub(crate) fn admit_base_url(
    base_url: &str,
    allow_loopback_http: bool,
) -> Result<Url, RestExecuteError> {
    admit_connected_app_url(base_url, UrlQueryPolicy::Refuse, allow_loopback_http).map_err(
        |refusal| match refusal {
            UrlAdmissionRefusal::Reason(reason) => RestExecuteError::InadmissibleBaseUrl { reason },
            UrlAdmissionRefusal::DeniedAddress => RestExecuteError::DeniedAddress,
        },
    )
}

/// Whether an admitted URL may carry a query. A *base* URL with a query is
/// configuration that cannot mean anything; a document URL legitimately needs
/// one (`?format=json`, versioned exports).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UrlQueryPolicy {
    Refuse,
    Allow,
}

pub(crate) enum UrlAdmissionRefusal {
    Reason(&'static str),
    DeniedAddress,
}

pub(crate) fn admit_https_url(
    url: &str,
    query: UrlQueryPolicy,
) -> Result<Url, UrlAdmissionRefusal> {
    admit_connected_app_url(url, query, false)
}

fn admit_connected_app_url(
    url: &str,
    query: UrlQueryPolicy,
    allow_loopback_http: bool,
) -> Result<Url, UrlAdmissionRefusal> {
    let refuse = |reason| Err(UrlAdmissionRefusal::Reason(reason));
    if url.len() > MAX_REST_BASE_URL_BYTES {
        return refuse("URL exceeds the byte limit");
    }
    let Ok(parsed) = Url::parse(url) else {
        return refuse("URL is not valid");
    };
    let http_loopback = match parsed.scheme() {
        "https" => false,
        "http" => true,
        _ => return refuse("scheme must be https"),
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return refuse("URL must not carry userinfo");
    }
    if parsed.fragment().is_some() {
        return refuse("URL must not carry a fragment");
    }
    if query == UrlQueryPolicy::Refuse && parsed.query().is_some() {
        return refuse("URL must not carry a query");
    }
    match parsed.host_str() {
        None | Some("") => return refuse("URL has no host"),
        Some(_) => {}
    }
    if http_loopback {
        // Hostname resolution must never widen this exemption: `localhost`
        // and every other DNS name stay refused for http.
        if parsed.domain().is_some() {
            return refuse("http is only allowed for loopback IP literals; use 127.0.0.1 or [::1]");
        }
        let address = parse_ip_literal_host(&parsed)?;
        if !is_loopback_http_literal(address) {
            return refuse("scheme must be https");
        }
        if !allow_loopback_http {
            return refuse("http on a loopback address requires allow_loopback_http");
        }
        return Ok(parsed);
    }
    // An IP-literal host — in any encoding the URL parser dials as an
    // address — is vetted against the denied-network list right here; a
    // DNS-named host is vetted after resolution instead.
    if parsed.domain().is_none() {
        let address = parse_ip_literal_host(&parsed)?;
        if admit_fetch_address(address).is_err() {
            return Err(UrlAdmissionRefusal::DeniedAddress);
        }
    }
    Ok(parsed)
}

/// Parse an IP-literal URL host. The URL parser has already rejected a
/// missing host; this only fails on encodings it does not treat as addresses.
fn parse_ip_literal_host(parsed: &Url) -> Result<IpAddr, UrlAdmissionRefusal> {
    let literal = parsed
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    literal
        .parse::<IpAddr>()
        .map_err(|_| UrlAdmissionRefusal::Reason("URL host is not a name or address"))
}

/// Loopback HTTP is 127.0.0.0/8 or exactly `::1`. Mapped IPv6 (`::ffff:127.0.0.1`)
/// is not an exemption — use the v4 literal or `[::1]`.
fn is_loopback_http_literal(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Every executed request must stay on the admitted base origin (scheme,
/// host, port). Path and query may change; a redirect or template must not.
fn pin_to_admitted_origin(base: &Url, url: &Url) -> Result<(), RestExecuteError> {
    if url.scheme() != base.scheme()
        || url.host() != base.host()
        || url.port_or_known_default() != base.port_or_known_default()
    {
        return Err(RestExecuteError::InadmissibleBaseUrl {
            reason: "assembled request left the admitted origin",
        });
    }
    Ok(())
}

/// Join the admitted base URL's path prefix with the substituted operation
/// path and attach the encoded query.
fn assemble_url(base: &Url, rendered: &RenderedParameters) -> Result<Url, RestExecuteError> {
    let mut url = base.clone();
    let prefix = base.path().trim_end_matches('/');
    let operation_path = if rendered.path.starts_with('/') {
        rendered.path.clone()
    } else {
        format!("/{}", rendered.path)
    };
    let assembled = format!("{prefix}{operation_path}");
    url.set_path(&assembled);
    // Backstop against dot-segment collapse the per-value refusal did not
    // see (a template-borne `..`, say): if normalization changed the path's
    // segment structure, the request would not go where the catalog and base
    // URL said, so it does not go anywhere.
    let expected_segments = assembled.split('/').skip(1).count();
    if url.path_segments().map(Iterator::count) != Some(expected_segments) {
        return Err(RestExecuteError::InadmissibleBaseUrl {
            reason: "assembled request path did not survive URL normalization",
        });
    }
    if !rendered.query.is_empty() {
        let joined = rendered
            .query
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("&");
        url.set_query(Some(&joined));
    }
    Ok(url)
}

/// Substitute `{name}` placeholders with pre-rendered values, percent-encoding
/// everything outside the unreserved set — so a hostile value cannot introduce
/// a `/`, a `..` segment, a `?`, or a `#` into the request target. Encoding,
/// not rejection, is the policy: the value survives verbatim on the server
/// side while the URL structure stays fixed.
fn substitute_path_template(
    template: &str,
    values: &[(String, String)],
) -> Result<String, RestExecuteError> {
    let mut substituted = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        substituted.push_str(&rest[..start]);
        let Some(length) = rest[start..].find('}') else {
            // Ingest refuses malformed templates; an unterminated brace that
            // somehow survives is kept literal rather than guessed at.
            substituted.push_str(&rest[start..]);
            return Ok(substituted);
        };
        let name = &rest[start + 1..start + length];
        let Some((_, value)) = values.iter().find(|(candidate, _)| candidate == name) else {
            return Err(RestExecuteError::MissingParameter {
                name: name.to_owned(),
            });
        };
        substituted.push_str(&percent_encode_strict(value));
        rest = &rest[start + length + 1..];
    }
    substituted.push_str(rest);
    Ok(substituted)
}

/// Percent-encode every byte outside RFC 3986's unreserved set. Stricter than
/// a component encoder needs to be, which is the point: nothing a value can
/// contain survives as URL structure.
fn percent_encode_strict(value: &str) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            other => {
                let _ = write!(encoded, "%{other:02X}");
            }
        }
    }
    encoded
}

/// The one deterministic text rendering of a JSON scalar; `None` for
/// anything that has no single obvious wire form.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Printable ASCII only (0x20..=0x7E): refuses the control bytes that would
/// smuggle header folding or CRLF injection, and non-ASCII that transports
/// encode inconsistently.
fn is_printable_ascii(value: &str) -> bool {
    value.bytes().all(|byte| (0x20..=0x7E).contains(&byte))
}

/// RFC 7230 token charset for header names.
fn is_header_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// Headers that alter routing or message framing, refused everywhere: neither
/// a credential placement nor a declared header parameter may set them,
/// because a request that rewrites its own destination or framing escapes the
/// vetting that admitted it.
fn is_routing_or_framing_header(lowercase: &str) -> bool {
    matches!(
        lowercase,
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "te"
            | "trailer"
            | "upgrade"
            | "expect"
    ) || lowercase.starts_with("proxy-")
}

/// Headers the executor writes itself. A parameter may not shadow them;
/// `Authorization` is reachable only through an explicit credential
/// placement.
fn is_executor_owned_header(lowercase: &str) -> bool {
    matches!(lowercase, "authorization" | "content-type" | "user-agent")
}

/// Admit a credential placement's named header: token charset, no routing or
/// framing header, no executor-owned header — except `Authorization`, which a
/// placement (and only a placement) may name explicitly.
fn require_placement_header_name(name: &str) -> Result<(), RestExecuteError> {
    let refuse = || {
        Err(RestExecuteError::ForbiddenHeader {
            name: name.to_owned(),
        })
    };
    if !is_header_token(name) {
        return refuse();
    }
    let lowercase = name.to_ascii_lowercase();
    if is_routing_or_framing_header(&lowercase) {
        return refuse();
    }
    if is_executor_owned_header(&lowercase) && lowercase != "authorization" {
        return refuse();
    }
    Ok(())
}

/// Admit a declared header parameter's name: token charset, no routing or
/// framing header, no executor-owned header (including `Authorization`), and
/// never the header the credential placement owns for this request.
fn require_header_parameter_name(
    name: &str,
    placement_header: Option<&str>,
) -> Result<(), RestExecuteError> {
    let refuse = || {
        Err(RestExecuteError::ForbiddenHeader {
            name: name.to_owned(),
        })
    };
    if !is_header_token(name) {
        return refuse();
    }
    let lowercase = name.to_ascii_lowercase();
    if is_routing_or_framing_header(&lowercase) || is_executor_owned_header(&lowercase) {
        return refuse();
    }
    if placement_header == Some(lowercase.as_str()) {
        return refuse();
    }
    Ok(())
}

/// REST transport that builds one pinned `reqwest` client per request.
///
/// Building per request is what lets the connection be pinned: the vetted
/// addresses are installed as the host's only resolution, so the connect
/// cannot re-resolve to something the vetting never saw.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReqwestRestTransport;

#[async_trait]
impl RestTransport for ReqwestRestTransport {
    async fn execute(
        &self,
        request: &RestTransportRequest,
    ) -> Result<RestOperationResponse, RestExecuteError> {
        use futures::StreamExt;

        let mut builder = reqwest::Client::builder()
            // Load-bearing, not tidiness: reqwest defaults to discovering a
            // proxy from `HTTPS_PROXY`/`ALL_PROXY` and system configuration,
            // and a proxied connection dials the *proxy* — the pinned
            // addresses below are never consulted, so the deny list and the
            // per-request vetting silently become advisory, a loopback proxy
            // becomes reachable, and proxy userinfo would be turned into a
            // `Proxy-Authorization` header on the request. Removing this line
            // removes the address pinning.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(request.url.scheme() != "http")
            .user_agent(REST_EXECUTOR_USER_AGENT)
            .connect_timeout(request.timeout.min(Duration::from_secs(10)))
            .timeout(request.timeout);
        if let Some(domain) = request.url.domain() {
            let port = request.url.port_or_known_default().unwrap_or(443);
            let pinned: Vec<std::net::SocketAddr> = request
                .addresses
                .iter()
                .map(|address| std::net::SocketAddr::new(*address, port))
                .collect();
            builder = builder.resolve_to_addrs(domain, &pinned);
        }
        let client = builder.build().map_err(transport_failure)?;
        let mut outbound = client.request(reqwest_method(request.method), request.url.clone());
        for (name, value) in &request.headers {
            outbound = outbound.header(name.as_str(), value.as_str());
        }
        if let Some(body) = &request.body {
            outbound = outbound.body(body.clone());
        }
        let response = outbound.send().await.map_err(transport_failure)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        // An honest oversized Content-Length is refused before a byte is
        // read; a lying one is caught by the check-before-extend below, so it
        // never turns into an unbounded allocation either way.
        if response
            .content_length()
            .is_some_and(|length| length > MAX_REST_RESPONSE_BYTES as u64)
        {
            return Err(RestExecuteError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_failure)?;
            push_bounded(&mut body, &chunk)?;
        }
        Ok(RestOperationResponse {
            status,
            content_type,
            body,
        })
    }
}

/// Retain one streamed chunk, refusing the response the moment it would cross
/// [`MAX_REST_RESPONSE_BYTES`] — before the bytes are kept, so a lying
/// `Content-Length` never turns into an unbounded allocation.
fn push_bounded(body: &mut Vec<u8>, chunk: &[u8]) -> Result<(), RestExecuteError> {
    if body.len().saturating_add(chunk.len()) > MAX_REST_RESPONSE_BYTES {
        return Err(RestExecuteError::ResponseTooLarge);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

/// Map a transport failure without letting it carry anything sensitive: the
/// URL (which reqwest embeds in its messages) is stripped before the text is
/// kept, and a timeout is told apart so callers can act on it.
fn transport_failure(error: reqwest::Error) -> RestExecuteError {
    if error.is_timeout() {
        return RestExecuteError::Timeout;
    }
    RestExecuteError::Transport(error.without_url().to_string())
}

fn reqwest_method(method: HttpMethod) -> reqwest::Method {
    match method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Put => reqwest::Method::PUT,
        HttpMethod::Post => reqwest::Method::POST,
        HttpMethod::Delete => reqwest::Method::DELETE,
        HttpMethod::Patch => reqwest::Method::PATCH,
        HttpMethod::Head => reqwest::Method::HEAD,
        HttpMethod::Options => reqwest::Method::OPTIONS,
    }
}

/// Host resolver backed by the operating system's resolver.
#[derive(Clone, Copy, Debug, Default)]
pub struct TokioRestHostResolver;

#[async_trait]
impl RestHostResolver for TokioRestHostResolver {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, RestExecuteError> {
        let resolved = tokio::net::lookup_host((host, 443))
            .await
            .map_err(|_| RestExecuteError::UnresolvableHost)?;
        let mut addresses = Vec::new();
        for address in resolved.map(|socket| socket.ip()) {
            if !addresses.contains(&address) {
                addresses.push(address);
            }
        }
        Ok(addresses)
    }
}

/// Most redirect hops a spec fetch will follow.
pub const MAX_SPEC_FETCH_REDIRECTS: usize = 5;

/// Whole-fetch wall-time budget across every redirect hop.
pub const SPEC_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a config-time OpenAPI document fetch was refused or failed.
///
/// Closed and renderer-safe, the same posture as [`RestExecuteError`]: no
/// variant echoes response bytes, and transport text is kept URL-free.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpecFetchError {
    #[error("document URL is inadmissible: {reason}")]
    InadmissibleUrl { reason: &'static str },
    #[error("document URL resolves into a denied network range")]
    DeniedAddress,
    #[error("document URL's host did not resolve")]
    UnresolvableHost,
    #[error("fetch followed more than {MAX_SPEC_FETCH_REDIRECTS} redirects")]
    TooManyRedirects,
    #[error("a redirect carried no usable location")]
    MalformedRedirect,
    #[error("the server answered HTTP {status}")]
    HttpStatus { status: u16 },
    #[error(
        "fetched document exceeds {} bytes",
        crate::openapi_catalog::MAX_OPENAPI_DOCUMENT_BYTES
    )]
    DocumentTooLarge,
    #[error("fetch exceeded its time budget")]
    Timeout,
    #[error("fetch transport failed: {0}")]
    Transport(String),
    #[error("a redirect left the original origin")]
    CrossOriginRedirect,
}

/// Body and declared media type of a fetched OpenAPI document.
#[derive(Debug)]
pub(crate) struct FetchedSpecDocument {
    pub body: Vec<u8>,
    pub content_type: Option<String>,
}

/// Whether a spec fetch may follow a redirect onto another origin.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpecRedirectPolicy {
    /// Follow admitted hops, including a new origin (preview / upsert).
    FollowAdmitted,
    /// Stay on the original origin. Discovery uses this so a well-known
    /// path cannot be turned into a cross-origin fetch.
    SameOrigin,
}

/// Fetch an OpenAPI document from an operator-supplied URL, at configuration
/// time, with the executor's egress hygiene: https admission (or loopback
/// HTTP under the same opt-in as the base URL), fresh per-hop resolution
/// vetted against the denied-network list, a pinned no-proxy client, and a
/// bounded body.
///
/// Unlike operation execution, redirects are followed — vendors move and
/// version their published documents — but explicitly, one admitted hop at a
/// time, so every `Location` gets the same vetting as the original URL and no
/// credential is ever attached to any hop.
/// Loopback-http documents (only reachable with `allow_loopback_http`)
/// never follow redirects: a 302 must not walk the opt-in off that origin.
pub(crate) async fn fetch_spec_document(
    url: &str,
    allow_loopback_http: bool,
) -> Result<Vec<u8>, SpecFetchError> {
    fetch_spec_document_detailed(url, SpecRedirectPolicy::FollowAdmitted, allow_loopback_http)
        .await
        .map(|fetched| fetched.body)
}

/// Fetch a document the way [`fetch_spec_document`] does, keeping the
/// declared content type so discovery can tell JSON from HTML or YAML.
#[cfg(test)]
pub(crate) mod spec_fetch_mock {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    type MockFn = Arc<dyn Fn(&str) -> Result<FetchedSpecDocument, SpecFetchError> + Send + Sync>;

    static MOCK: OnceLock<Mutex<Option<MockFn>>> = OnceLock::new();

    fn slot() -> &'static Mutex<Option<MockFn>> {
        MOCK.get_or_init(|| Mutex::new(None))
    }

    pub(crate) fn install(mock: MockFn) {
        *slot().lock().expect("spec fetch mock") = Some(mock);
    }

    pub(crate) fn clear() {
        *slot().lock().expect("spec fetch mock") = None;
    }

    pub(crate) fn try_fetch(url: &str) -> Option<Result<FetchedSpecDocument, SpecFetchError>> {
        slot()
            .lock()
            .expect("spec fetch mock")
            .as_ref()
            .map(|mock| mock(url))
    }
}

pub(crate) async fn fetch_spec_document_detailed(
    url: &str,
    redirects: SpecRedirectPolicy,
    allow_loopback_http: bool,
) -> Result<FetchedSpecDocument, SpecFetchError> {
    let started = std::time::Instant::now();
    let mut current = url.to_string();
    let origin = admit_spec_fetch_url(&current, allow_loopback_http)?;
    #[cfg(test)]
    if let Some(mocked) = spec_fetch_mock::try_fetch(&current) {
        let _ = redirects;
        let _ = origin;
        return mocked;
    }
    for _ in 0..=MAX_SPEC_FETCH_REDIRECTS {
        let admitted = admit_spec_fetch_url(&current, allow_loopback_http)?;
        if redirects == SpecRedirectPolicy::SameOrigin
            && (admitted.scheme() != origin.scheme()
                || admitted.host() != origin.host()
                || admitted.port_or_known_default() != origin.port_or_known_default())
        {
            return Err(SpecFetchError::CrossOriginRedirect);
        }
        let remaining = SPEC_FETCH_TIMEOUT
            .checked_sub(started.elapsed())
            .ok_or(SpecFetchError::Timeout)?;

        // Same vetting shape as operation execution: every fresh answer for a
        // domain host must clear the denied-network list, and the connection
        // is pinned to exactly those answers. An IP-literal host was vetted
        // by the admission above.
        let addresses = match admitted.domain() {
            Some(domain) => {
                let resolved = TokioRestHostResolver
                    .resolve(domain)
                    .await
                    .map_err(|_| SpecFetchError::UnresolvableHost)?;
                if resolved.is_empty() {
                    return Err(SpecFetchError::UnresolvableHost);
                }
                if resolved
                    .iter()
                    .any(|address| admit_fetch_address(*address).is_err())
                {
                    return Err(SpecFetchError::DeniedAddress);
                }
                resolved
            }
            None => Vec::new(),
        };

        let https_only = admitted.scheme() != "http";
        let loopback_http = allow_loopback_http && !https_only;
        let mut builder = reqwest::Client::builder()
            // Load-bearing for the same reason as the operation transport: a
            // discovered proxy would dial the proxy, not the vetted address.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(https_only)
            .http1_only()
            .user_agent(REST_EXECUTOR_USER_AGENT)
            .connect_timeout(remaining.min(Duration::from_secs(10)))
            .timeout(remaining);
        if let Some(domain) = admitted.domain() {
            let port =
                admitted
                    .port_or_known_default()
                    .unwrap_or(if https_only { 443 } else { 80 });
            let pinned: Vec<std::net::SocketAddr> = addresses
                .iter()
                .map(|address| std::net::SocketAddr::new(*address, port))
                .collect();
            builder = builder.resolve_to_addrs(domain, &pinned);
        }
        let client = builder.build().map_err(spec_transport_failure)?;
        let response = client
            .get(admitted.clone())
            .header(
                reqwest::header::ACCEPT,
                "application/json, application/yaml, text/yaml;q=0.8",
            )
            .send()
            .await
            .map_err(spec_transport_failure)?;

        let status = response.status();
        if status.is_redirection() {
            if loopback_http {
                return Err(SpecFetchError::HttpStatus {
                    status: status.as_u16(),
                });
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| admitted.join(location).ok())
                .ok_or(SpecFetchError::MalformedRedirect)?;
            if redirects == SpecRedirectPolicy::SameOrigin
                && (location.scheme() != origin.scheme()
                    || location.host() != origin.host()
                    || location.port_or_known_default() != origin.port_or_known_default())
            {
                return Err(SpecFetchError::CrossOriginRedirect);
            }
            current = location.to_string();
            continue;
        }
        if !status.is_success() {
            return Err(SpecFetchError::HttpStatus {
                status: status.as_u16(),
            });
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        let cap = crate::openapi_catalog::MAX_OPENAPI_DOCUMENT_BYTES;
        if response
            .content_length()
            .is_some_and(|length| length > cap as u64)
        {
            return Err(SpecFetchError::DocumentTooLarge);
        }
        use futures::StreamExt;
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(spec_transport_failure)?;
            if body.len().saturating_add(chunk.len()) > cap {
                return Err(SpecFetchError::DocumentTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(FetchedSpecDocument { body, content_type });
    }
    Err(SpecFetchError::TooManyRedirects)
}

/// Admit a document URL. Tests may fetch `http://127.0.0.1` so a local mock
/// server can stand in for a vendor origin; production still requires https
/// and the denied-network list.
fn admit_spec_fetch_url(url: &str, allow_loopback_http: bool) -> Result<Url, SpecFetchError> {
    if cfg!(test) {
        if let Ok(parsed) = Url::parse(url) {
            let loopback = matches!(parsed.host_str(), Some("127.0.0.1") | Some("[::1]"));
            if parsed.scheme() == "http" && loopback {
                return Ok(parsed);
            }
        }
    }
    admit_connected_app_url(url, UrlQueryPolicy::Allow, allow_loopback_http).map_err(|refusal| {
        match refusal {
            UrlAdmissionRefusal::Reason(reason) => SpecFetchError::InadmissibleUrl { reason },
            UrlAdmissionRefusal::DeniedAddress => SpecFetchError::DeniedAddress,
        }
    })
}

/// Map a spec-fetch transport failure, URL-stripped, timeout told apart.
fn spec_transport_failure(error: reqwest::Error) -> SpecFetchError {
    if error.is_timeout() {
        return SpecFetchError::Timeout;
    }
    SpecFetchError::Transport(error.without_url().to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::openapi_catalog::{CatalogParameter, CatalogRequestBody};

    const SECRET_VALUE: &str = "sk-live-URXvXW0hVqzJm";
    const PUBLIC_V4: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34));
    const PRIVATE_V4: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5));

    struct FakeSecrets(HashMap<String, String>);

    impl FakeSecrets {
        fn with(name: &str, value: &str) -> Arc<Self> {
            Arc::new(Self(HashMap::from([(name.to_owned(), value.to_owned())])))
        }

        fn empty() -> Arc<Self> {
            Arc::new(Self(HashMap::new()))
        }
    }

    #[async_trait]
    impl SecretProvider for FakeSecrets {
        async fn get_secret(&self, key: &str) -> tidebreak_core::Result<Option<String>> {
            Ok(self.0.get(key).cloned())
        }

        async fn set_secret(&self, _key: &str, _value: &str) -> tidebreak_core::Result<()> {
            Ok(())
        }

        async fn delete_secret(&self, _key: &str) -> tidebreak_core::Result<()> {
            Ok(())
        }
    }

    struct FakeTransport {
        requests: Mutex<Vec<RestTransportRequest>>,
        response: Result<RestOperationResponse, RestExecuteError>,
    }

    impl FakeTransport {
        fn ok() -> Self {
            Self::respond(Ok(RestOperationResponse {
                status: 200,
                content_type: Some("application/json".to_owned()),
                body: b"{}".to_vec(),
            }))
        }

        fn respond(response: Result<RestOperationResponse, RestExecuteError>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                response,
            }
        }

        fn sole_request(&self) -> RestTransportRequest {
            let requests = self.requests.lock().unwrap();
            assert_eq!(requests.len(), 1, "expected exactly one dispatched request");
            requests[0].clone()
        }

        fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl RestTransport for &FakeTransport {
        async fn execute(
            &self,
            request: &RestTransportRequest,
        ) -> Result<RestOperationResponse, RestExecuteError> {
            self.requests.lock().unwrap().push(request.clone());
            self.response.clone()
        }
    }

    struct FakeResolver(Vec<IpAddr>);

    #[async_trait]
    impl RestHostResolver for FakeResolver {
        async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, RestExecuteError> {
            Ok(self.0.clone())
        }
    }

    fn catalog() -> OperationCatalog {
        let operations = BTreeMap::from([
            (
                "getIssue".to_owned(),
                CatalogOperation {
                    operation_id: "getIssue".to_owned(),
                    method: HttpMethod::Get,
                    path_template: "/repos/{owner}/issues/{number}".to_owned(),
                    parameters: vec![
                        CatalogParameter {
                            name: "owner".to_owned(),
                            location: ParameterLocation::Path,
                            required: true,
                            schema: Some(json!({"type": "string"})),
                        },
                        CatalogParameter {
                            name: "number".to_owned(),
                            location: ParameterLocation::Path,
                            required: true,
                            schema: None,
                        },
                        CatalogParameter {
                            name: "state".to_owned(),
                            location: ParameterLocation::Query,
                            required: false,
                            schema: Some(json!({"type": "string", "enum": ["open", "closed"]})),
                        },
                        CatalogParameter {
                            name: "x-request-tag".to_owned(),
                            location: ParameterLocation::Header,
                            required: false,
                            schema: None,
                        },
                    ],
                    request_body: None,
                },
            ),
            (
                "createIssue".to_owned(),
                CatalogOperation {
                    operation_id: "createIssue".to_owned(),
                    method: HttpMethod::Post,
                    path_template: "/issues".to_owned(),
                    parameters: Vec::new(),
                    request_body: Some(CatalogRequestBody {
                        required: true,
                        schema: Some(json!({
                            "type": "object",
                            "required": ["title"],
                            "properties": {"title": {"type": "string"}}
                        })),
                    }),
                },
            ),
            (
                "hostHeader".to_owned(),
                CatalogOperation {
                    operation_id: "hostHeader".to_owned(),
                    method: HttpMethod::Get,
                    path_template: "/ping".to_owned(),
                    parameters: vec![CatalogParameter {
                        name: "Host".to_owned(),
                        location: ParameterLocation::Header,
                        required: true,
                        schema: None,
                    }],
                    request_body: None,
                },
            ),
        ]);
        OperationCatalog {
            document_sha256: "0".repeat(64),
            operations,
        }
    }

    fn target(credential: Option<RestCredential>) -> RestApiTarget {
        RestApiTarget {
            base_url: "https://api.example.com/v2".to_owned(),
            credential,
            allow_loopback_http: false,
        }
    }

    fn bearer(secret_name: &str) -> Option<RestCredential> {
        Some(RestCredential {
            secret_name: secret_name.to_owned(),
            placement: CredentialPlacement::Bearer,
        })
    }

    fn get_issue(parameters: Value) -> RestOperationRequest {
        RestOperationRequest {
            operation_id: "getIssue".to_owned(),
            parameters,
            body: None,
        }
    }

    fn full_parameters() -> Value {
        json!({"owner": "octocat", "number": 42})
    }

    async fn run(
        transport: &FakeTransport,
        resolver: FakeResolver,
        secrets: Arc<FakeSecrets>,
        target: &RestApiTarget,
        request: &RestOperationRequest,
    ) -> Result<RestOperationResponse, RestExecuteError> {
        RestExecutor::new(transport, resolver, secrets)
            .execute(target, &catalog(), request, None)
            .await
    }

    #[tokio::test]
    async fn catalog_validation_refuses_before_any_io() {
        let transport = FakeTransport::ok();
        let cases: Vec<(RestOperationRequest, RestExecuteError)> = vec![
            (
                RestOperationRequest {
                    operation_id: "deleteEverything".to_owned(),
                    parameters: json!({}),
                    body: None,
                },
                RestExecuteError::UnknownOperation {
                    operation_id: "deleteEverything".to_owned(),
                },
            ),
            (
                get_issue(json!([1, 2])),
                RestExecuteError::ParametersNotAnObject,
            ),
            (
                get_issue(json!({"owner": "octocat", "number": 1, "verbose": true})),
                RestExecuteError::UndeclaredParameter {
                    name: "verbose".to_owned(),
                },
            ),
            (
                get_issue(json!({"owner": "octocat"})),
                RestExecuteError::MissingParameter {
                    name: "number".to_owned(),
                },
            ),
            (
                get_issue(json!({"owner": 7, "number": 1})),
                RestExecuteError::InvalidParameter {
                    name: "owner".to_owned(),
                },
            ),
            (
                get_issue(json!({"owner": "octocat", "number": 1, "state": "wontfix"})),
                RestExecuteError::InvalidParameter {
                    name: "state".to_owned(),
                },
            ),
            (
                get_issue(json!({"owner": "octocat", "number": {"nested": true}})),
                RestExecuteError::UnrepresentableParameter {
                    name: "number".to_owned(),
                },
            ),
            (
                RestOperationRequest {
                    operation_id: "getIssue".to_owned(),
                    parameters: full_parameters(),
                    body: Some(json!({"stray": true})),
                },
                RestExecuteError::UndeclaredBody,
            ),
            (
                RestOperationRequest {
                    operation_id: "createIssue".to_owned(),
                    parameters: json!({}),
                    body: None,
                },
                RestExecuteError::MissingBody,
            ),
            (
                RestOperationRequest {
                    operation_id: "createIssue".to_owned(),
                    parameters: json!({}),
                    body: Some(json!({"title": 7})),
                },
                RestExecuteError::InvalidBody,
            ),
            (
                RestOperationRequest {
                    operation_id: "createIssue".to_owned(),
                    parameters: json!({}),
                    body: Some(json!({
                        "title": "x".repeat(MAX_REST_REQUEST_BODY_BYTES)
                    })),
                },
                RestExecuteError::BodyTooLarge,
            ),
        ];
        for (request, expected) in cases {
            let refused = run(
                &transport,
                FakeResolver(vec![PUBLIC_V4]),
                FakeSecrets::empty(),
                &target(None),
                &request,
            )
            .await
            .unwrap_err();
            assert_eq!(refused, expected);
        }
        assert_eq!(
            transport.request_count(),
            0,
            "a refused request must never reach the transport"
        );
    }

    #[tokio::test]
    async fn hostile_path_parameters_are_encoded_not_traversed() {
        let transport = FakeTransport::ok();
        run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &target(None),
            &get_issue(json!({
                "owner": "../../../admin?x=1#frag",
                "number": "weiß/ソ",
                "state": "open",
            })),
        )
        .await
        .unwrap();
        let dispatched = transport.sole_request();
        // The url crate decodes `%2E` back to a literal dot, which is why a
        // pure dot-segment value is refused below; the mixed hostile value
        // stays a single opaque segment either way.
        assert_eq!(
            dispatched.url.as_str(),
            "https://api.example.com/v2/repos/..%2F..%2F..%2Fadmin%3Fx%3D1%23frag\
             /issues/wei%C3%9F%2F%E3%82%BD?state=open"
        );
        // The hostile value introduced no new path segments and no query or
        // fragment of its own.
        assert_eq!(dispatched.url.path_segments().unwrap().count(), 5);

        // A value that *is* `..` would survive encoding as a real dot
        // segment and pop its parent, so it is refused outright.
        let refused = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &target(None),
            &get_issue(json!({"owner": "..", "number": 1})),
        )
        .await
        .unwrap_err();
        assert_eq!(
            refused,
            RestExecuteError::UnrepresentableParameter {
                name: "owner".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn forbidden_headers_are_refused_for_parameters_and_placements() {
        // A spec-declared `Host` header parameter never reaches the wire.
        let transport = FakeTransport::ok();
        let refused = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &target(None),
            &RestOperationRequest {
                operation_id: "hostHeader".to_owned(),
                parameters: json!({"Host": "evil.internal"}),
                body: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            refused,
            RestExecuteError::ForbiddenHeader {
                name: "Host".to_owned()
            }
        );

        // Placements: routing/framing and executor-owned names are refused
        // (case-insensitively), as is a non-token name; `Authorization` is
        // reachable only as an explicit placement, and a benign named header
        // is fine.
        for name in [
            "Host",
            "content-LENGTH",
            "Transfer-Encoding",
            "Connection",
            "Proxy-Authorization",
            "Content-Type",
            "not a token",
        ] {
            let refused = run(
                &transport,
                FakeResolver(vec![PUBLIC_V4]),
                FakeSecrets::with("k", SECRET_VALUE),
                &target(Some(RestCredential {
                    secret_name: "k".to_owned(),
                    placement: CredentialPlacement::Header(name.to_owned()),
                })),
                &get_issue(full_parameters()),
            )
            .await
            .unwrap_err();
            assert!(
                matches!(refused, RestExecuteError::ForbiddenHeader { .. }),
                "{name} was not refused"
            );
        }
        assert_eq!(transport.request_count(), 0);
        for name in ["Authorization", "X-Api-Key", "Cookie"] {
            run(
                &transport,
                FakeResolver(vec![PUBLIC_V4]),
                FakeSecrets::with("k", SECRET_VALUE),
                &target(Some(RestCredential {
                    secret_name: "k".to_owned(),
                    placement: CredentialPlacement::Header(name.to_owned()),
                })),
                &get_issue(full_parameters()),
            )
            .await
            .unwrap_or_else(|error| panic!("{name} placement refused: {error}"));
        }
    }

    #[tokio::test]
    async fn address_vetting_is_all_or_nothing() {
        let transport = FakeTransport::ok();
        let refused = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4, PRIVATE_V4]),
            FakeSecrets::empty(),
            &target(None),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, RestExecuteError::DeniedAddress);
        assert_eq!(
            transport.request_count(),
            0,
            "a partially private resolution must never be dialed"
        );

        let refused = run(
            &transport,
            FakeResolver(Vec::new()),
            FakeSecrets::empty(),
            &target(None),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, RestExecuteError::UnresolvableHost);
    }

    #[tokio::test]
    async fn base_url_admission_allows_ports_and_refuses_the_rest() {
        let transport = FakeTransport::ok();
        for (base_url, expected) in [
            (
                "http://api.example.com",
                "http is only allowed for loopback IP literals; use 127.0.0.1 or [::1]",
            ),
            ("https://u:p@api.example.com", "URL must not carry userinfo"),
            (
                "https://api.example.com/#frag",
                "URL must not carry a fragment",
            ),
            ("https://api.example.com/?q=1", "URL must not carry a query"),
            ("not a url", "URL is not valid"),
        ] {
            let refused = run(
                &transport,
                FakeResolver(vec![PUBLIC_V4]),
                FakeSecrets::empty(),
                &RestApiTarget {
                    base_url: base_url.to_owned(),
                    credential: None,
                    allow_loopback_http: false,
                },
                &get_issue(full_parameters()),
            )
            .await
            .unwrap_err();
            assert_eq!(
                refused,
                RestExecuteError::InadmissibleBaseUrl { reason: expected },
                "{base_url}"
            );
        }
        // A loopback IP literal is refused as a destination, not as a URL.
        let refused = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &RestApiTarget {
                base_url: "https://127.0.0.1:8443".to_owned(),
                credential: None,
                allow_loopback_http: false,
            },
            &get_issue(full_parameters()),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, RestExecuteError::DeniedAddress);
        assert_eq!(transport.request_count(), 0);

        // An explicit non-default port is allowed — a REST base URL may pin
        // one — and survives into the dispatched URL.
        run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &RestApiTarget {
                base_url: "https://api.example.com:8443/v2".to_owned(),
                credential: None,
                allow_loopback_http: false,
            },
            &get_issue(full_parameters()),
        )
        .await
        .unwrap();
        assert_eq!(
            transport.sole_request().url.as_str(),
            "https://api.example.com:8443/v2/repos/octocat/issues/42"
        );
    }

    #[tokio::test]
    async fn redirects_are_reported_not_followed() {
        let transport = FakeTransport::respond(Ok(RestOperationResponse {
            status: 302,
            content_type: None,
            body: Vec::new(),
        }));
        let response = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &target(None),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(
            transport.request_count(),
            1,
            "a redirect status must not trigger a second request"
        );
    }

    #[tokio::test]
    async fn oversized_transport_response_is_refused() {
        // The executor holds custom transports to the same cap as the real
        // one; the real transport's own cap is push_bounded, tested below.
        let transport = FakeTransport::respond(Ok(RestOperationResponse {
            status: 200,
            content_type: None,
            body: vec![0; MAX_REST_RESPONSE_BYTES + 1],
        }));
        let refused = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &target(None),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, RestExecuteError::ResponseTooLarge);
    }

    #[test]
    fn push_bounded_refuses_before_retaining_a_lying_chunk() {
        let mut body = vec![0_u8; MAX_REST_RESPONSE_BYTES - 1];
        assert_eq!(
            push_bounded(&mut body, &[0, 0]).unwrap_err(),
            RestExecuteError::ResponseTooLarge
        );
        // The over-cap chunk was refused before extending, not truncated in.
        assert_eq!(body.len(), MAX_REST_RESPONSE_BYTES - 1);
        assert!(push_bounded(&mut body, &[0]).is_ok());
    }

    #[tokio::test]
    async fn credential_is_injected_and_never_rendered() {
        // Bearer placement.
        let transport = FakeTransport::ok();
        run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::with("sentry_token", SECRET_VALUE),
            &target(bearer("sentry_token")),
            &get_issue(json!({"owner": "octocat", "number": 1, "x-request-tag": "t-1"})),
        )
        .await
        .unwrap();
        let dispatched = transport.sole_request();
        assert!(dispatched
            .headers
            .contains(&("authorization".to_owned(), format!("Bearer {SECRET_VALUE}"))));
        assert!(dispatched
            .headers
            .contains(&("x-request-tag".to_owned(), "t-1".to_owned())));
        // Debug output redacts every header value: the credential must not
        // survive into logs through an incidental {:?}.
        let debugged = format!("{dispatched:?}");
        assert!(!debugged.contains(SECRET_VALUE), "{debugged}");
        assert!(debugged.contains("authorization"), "{debugged}");

        // Named-header placement.
        let transport = FakeTransport::ok();
        run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::with("sentry_token", SECRET_VALUE),
            &target(Some(RestCredential {
                secret_name: "sentry_token".to_owned(),
                placement: CredentialPlacement::Header("X-Api-Key".to_owned()),
            })),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap();
        assert!(transport
            .sole_request()
            .headers
            .contains(&("x-api-key".to_owned(), SECRET_VALUE.to_owned())));

        // A transport failure's full rendering never echoes the credential.
        let transport = FakeTransport::respond(Err(RestExecuteError::Transport(
            "connection reset by peer".to_owned(),
        )));
        let error = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::with("sentry_token", SECRET_VALUE),
            &target(bearer("sentry_token")),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap_err();
        let rendered = format!("{error} / {error:?}");
        assert!(!rendered.contains(SECRET_VALUE), "{rendered}");
    }

    #[tokio::test]
    async fn missing_secret_is_a_distinct_refusal() {
        let transport = FakeTransport::ok();
        let refused = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
            &target(bearer("absent_token")),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, RestExecuteError::MissingCredential);
        assert_eq!(transport.request_count(), 0);

        // A secret that cannot travel as a header is refused before the
        // transport, not surfaced as a transport failure that might echo it.
        let refused = run(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::with("k", "line\nbreak"),
            &target(bearer("k")),
            &get_issue(full_parameters()),
        )
        .await
        .unwrap_err();
        assert_eq!(refused, RestExecuteError::UnusableCredential);
        assert_eq!(transport.request_count(), 0);
    }

    #[tokio::test]
    async fn timeout_is_clamped_and_defaulted() {
        let transport = FakeTransport::ok();
        let executor = RestExecutor::new(
            &transport,
            FakeResolver(vec![PUBLIC_V4]),
            FakeSecrets::empty(),
        );
        for asked in [
            None,
            Some(Duration::from_millis(5)),
            Some(Duration::from_secs(600)),
        ] {
            executor
                .execute(
                    &target(None),
                    &catalog(),
                    &get_issue(full_parameters()),
                    asked,
                )
                .await
                .unwrap();
        }
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests[0].timeout, DEFAULT_REST_TIMEOUT);
        assert_eq!(requests[1].timeout, MIN_REST_TIMEOUT);
        assert_eq!(requests[2].timeout, MAX_REST_TIMEOUT);
    }

    #[test]
    fn admit_base_url_loopback_http_requires_flag_and_ip_literal() {
        assert!(admit_base_url("http://127.0.0.1:23373/v0", true).is_ok());
        assert!(admit_base_url("http://127.9.9.9:1", true).is_ok());
        assert!(admit_base_url("http://[::1]:8080/", true).is_ok());

        let localhost = admit_base_url("http://localhost:23373/v0", true).unwrap_err();
        assert_eq!(
            localhost,
            RestExecuteError::InadmissibleBaseUrl {
                reason: "http is only allowed for loopback IP literals; use 127.0.0.1 or [::1]",
            }
        );

        let private = admit_base_url("http://10.0.0.1:80", true).unwrap_err();
        assert_eq!(
            private,
            RestExecuteError::InadmissibleBaseUrl {
                reason: "scheme must be https",
            }
        );

        let no_flag = admit_base_url("http://127.0.0.1:23373/v0", false).unwrap_err();
        assert_eq!(
            no_flag,
            RestExecuteError::InadmissibleBaseUrl {
                reason: "http on a loopback address requires allow_loopback_http",
            }
        );
    }

    #[tokio::test]
    async fn loopback_http_executor_does_not_follow_redirects() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        {
            let probe = TcpListener::bind("127.0.0.1:0").unwrap();
            let probe_addr = probe.local_addr().unwrap();
            std::thread::spawn(move || {
                let _ = probe.accept();
            });
            if std::net::TcpStream::connect_timeout(
                &probe_addr,
                std::time::Duration::from_millis(300),
            )
            .is_err()
            {
                return;
            }
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let second = TcpListener::bind("127.0.0.1:0").unwrap();
        let second_port = second.local_addr().unwrap().port();
        let second_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_hits_clone = second_hits.clone();
        std::thread::spawn(move || {
            second.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                if let Ok((mut stream, _)) = second.accept() {
                    second_hits_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut buf = [0u8; 512];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Drain the request before answering: closing a socket with
            // unread bytes resets the connection and the client reports a
            // transport error instead of the 302.
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{second_port}/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(body.as_bytes());
        });

        let response = RestExecutor::new(
            ReqwestRestTransport,
            FakeResolver(Vec::new()),
            FakeSecrets::empty(),
        )
        .execute(
            &RestApiTarget {
                base_url: format!("http://127.0.0.1:{}", addr.port()),
                credential: None,
                allow_loopback_http: true,
            },
            &catalog(),
            &get_issue(full_parameters()),
            Some(Duration::from_secs(2)),
        )
        .await
        .unwrap();
        assert_eq!(response.status, 302);
        assert_eq!(
            second_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a 302 to another loopback port must not be followed"
        );
    }
}
