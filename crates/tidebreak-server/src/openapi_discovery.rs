//! Probe a bounded list of well-known OpenAPI document locations relative to
//! an operator-supplied https origin or base URL.

use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::openapi_catalog::{enumerate_openapi_operations, OpenApiIngestError};
use crate::rest_executor::{
    fetch_spec_document_detailed, SpecFetchError, SpecRedirectPolicy, UrlAdmissionRefusal,
};

/// Locations tried relative to the origin and, when present, the supplied
/// path prefix.
pub const WELL_KNOWN_OPENAPI_PATHS: &[&str] = &[
    "/openapi.json",
    "/openapi/v3.json",
    "/openapi.yaml",
    "/swagger.json",
    "/swagger/v1/swagger.json",
    "/v3/api-docs",
    "/api-docs",
    "/api/openapi.json",
    "/.well-known/openapi.json",
    "/docs/openapi.json",
];

/// Concurrent probes against one origin.
const DISCOVERY_CONCURRENCY: usize = 4;
/// Wall-time budget for the whole probe set.
pub const DISCOVERY_DEADLINE: Duration = Duration::from_secs(5);

/// `POST /connected-apps/rest/spec-discovery` body.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecDiscoveryRequest {
    /// https origin or base URL the well-known paths are joined to.
    pub origin: String,
}

/// What probing one origin found.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct SpecDiscoveryInfo {
    /// Candidate documents that answered, usable or not.
    pub candidates: Vec<SpecDiscoveryCandidate>,
    /// Every location that was considered, in probe order.
    pub tried: Vec<String>,
}

/// One well-known location that returned a document.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct SpecDiscoveryCandidate {
    pub url: String,
    /// Selectable operations when the document enumerates as OpenAPI 3 JSON.
    pub operation_count: Option<usize>,
    /// Why the document cannot be used, when it cannot.
    pub unsupported_reason: Option<String>,
}

/// Why discovery refused the origin before probing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecDiscoveryError {
    InadmissibleUrl { reason: &'static str },
    DeniedAddress,
}

impl std::fmt::Display for SpecDiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InadmissibleUrl { reason } => write!(f, "origin is inadmissible: {reason}"),
            Self::DeniedAddress => write!(f, "origin resolves into a denied network range"),
        }
    }
}

/// Join well-known paths to the origin and, when the URL carries a path
/// prefix, to that prefix as well. Duplicates (origin already has no prefix)
/// are dropped, preserving first-seen order.
pub fn discovery_probe_urls(origin: &Url) -> Vec<String> {
    let mut origin_root = origin.clone();
    origin_root.set_path("/");
    origin_root.set_query(None);
    origin_root.set_fragment(None);
    let prefix = origin.path().trim_end_matches('/');
    let prefixed = if prefix.is_empty() || prefix == "/" {
        None
    } else {
        let mut prefixed = origin.clone();
        prefixed.set_path(prefix);
        prefixed.set_query(None);
        prefixed.set_fragment(None);
        Some(prefixed)
    };
    let mut urls = Vec::new();
    for path in WELL_KNOWN_OPENAPI_PATHS {
        push_unique(&mut urls, join_path(&origin_root, path));
        if let Some(base) = &prefixed {
            push_unique(&mut urls, join_path(base, path));
        }
    }
    urls
}

fn join_path(base: &Url, path: &str) -> String {
    let mut url = base.clone();
    let prefix = base.path().trim_end_matches('/');
    url.set_path(&format!("{prefix}{path}"));
    url.to_string()
}

fn push_unique(urls: &mut Vec<String>, url: String) {
    if !urls.iter().any(|existing| existing == &url) {
        urls.push(url);
    }
}

fn admit_discovery_origin(origin: &str) -> Result<Url, SpecDiscoveryError> {
    if cfg!(test) {
        if let Ok(parsed) = Url::parse(origin) {
            let loopback = matches!(parsed.host_str(), Some("127.0.0.1") | Some("[::1]"));
            if parsed.scheme() == "http" && loopback {
                return Ok(parsed);
            }
        }
    }
    crate::rest_executor::admit_https_url(origin, crate::rest_executor::UrlQueryPolicy::Refuse)
        .map_err(|refusal| match refusal {
            UrlAdmissionRefusal::Reason(reason) => SpecDiscoveryError::InadmissibleUrl { reason },
            UrlAdmissionRefusal::DeniedAddress => SpecDiscoveryError::DeniedAddress,
        })
}

/// Probe well-known locations. Hits that enumerate as OpenAPI 3 JSON are
/// usable; YAML, HTML, and Swagger 2.0 are reported as found-but-unsupported.
/// The first usable document cancels probes that have not started yet.
pub async fn discover_openapi_documents(
    origin: &str,
) -> Result<SpecDiscoveryInfo, SpecDiscoveryError> {
    let admitted = admit_discovery_origin(origin)?;
    let tried = discovery_probe_urls(&admitted);
    let permit = Arc::new(Semaphore::new(DISCOVERY_CONCURRENCY));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tasks = Vec::new();
    for url in tried.clone() {
        let permit = Arc::clone(&permit);
        let stop = Arc::clone(&stop);
        tasks.push(tokio::spawn(async move {
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            let Ok(_guard) = permit.acquire().await else {
                return None;
            };
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            let candidate = probe_location(&url).await;
            if candidate
                .as_ref()
                .is_some_and(|found| found.operation_count.is_some())
            {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            candidate
        }));
    }

    let collect = async {
        let mut candidates = Vec::new();
        for task in tasks {
            if let Ok(Some(candidate)) = task.await {
                candidates.push(candidate);
            }
        }
        candidates
    };
    let candidates = tokio::time::timeout(DISCOVERY_DEADLINE, collect)
        .await
        .unwrap_or_default();
    Ok(SpecDiscoveryInfo { candidates, tried })
}

async fn probe_location(url: &str) -> Option<SpecDiscoveryCandidate> {
    match fetch_spec_document_detailed(url, SpecRedirectPolicy::SameOrigin).await {
        Ok(fetched) => Some(classify_document(
            url,
            fetched.content_type.as_deref(),
            &fetched.body,
        )),
        Err(SpecFetchError::HttpStatus { .. })
        | Err(SpecFetchError::CrossOriginRedirect)
        | Err(SpecFetchError::Timeout)
        | Err(SpecFetchError::Transport(_))
        | Err(SpecFetchError::UnresolvableHost)
        | Err(SpecFetchError::MalformedRedirect)
        | Err(SpecFetchError::TooManyRedirects) => None,
        Err(SpecFetchError::InadmissibleUrl { reason }) => Some(SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: None,
            unsupported_reason: Some(format!("document URL is inadmissible: {reason}")),
        }),
        Err(SpecFetchError::DeniedAddress) => Some(SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: None,
            unsupported_reason: Some("document URL resolves into a denied network range".into()),
        }),
        Err(SpecFetchError::DocumentTooLarge) => Some(SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: None,
            unsupported_reason: Some(SpecFetchError::DocumentTooLarge.to_string()),
        }),
    }
}

fn classify_document(url: &str, content_type: Option<&str>, body: &[u8]) -> SpecDiscoveryCandidate {
    let media = content_type.unwrap_or("").to_ascii_lowercase();
    if media.contains("html") {
        return SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: None,
            unsupported_reason: Some(
                "this URL returned an HTML page, not a JSON OpenAPI document".into(),
            ),
        };
    }
    let path_yaml = url.rsplit('/').next().is_some_and(|name| {
        let lower = name.to_ascii_lowercase();
        lower.ends_with(".yaml") || lower.ends_with(".yml")
    });
    if path_yaml || media.contains("yaml") || looks_like_yaml(body) {
        return SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: None,
            unsupported_reason: Some(
                "YAML OpenAPI documents are not supported; convert to JSON".into(),
            ),
        };
    }
    match enumerate_openapi_operations(body) {
        Ok(inventory) => SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: Some(inventory.operations.len()),
            unsupported_reason: None,
        },
        Err(OpenApiIngestError::SwaggerNotSupported) => SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: None,
            unsupported_reason: Some(OpenApiIngestError::SwaggerNotSupported.to_string()),
        },
        Err(error) => SpecDiscoveryCandidate {
            url: url.to_owned(),
            operation_count: None,
            unsupported_reason: Some(error.to_string()),
        },
    }
}

fn looks_like_yaml(body: &[u8]) -> bool {
    let text = std::str::from_utf8(body).unwrap_or("");
    let trimmed = text.trim_start();
    trimmed.starts_with("---")
        || (trimmed.starts_with("openapi:") && !trimmed.starts_with('{'))
        || (trimmed.starts_with("swagger:") && !trimmed.starts_with('{'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_and_origin_are_both_probed_without_duplicates() {
        let origin = Url::parse("https://api.example.com/v2").unwrap();
        let urls = discovery_probe_urls(&origin);
        assert!(urls.contains(&"https://api.example.com/openapi.json".into()));
        assert!(urls.contains(&"https://api.example.com/v2/openapi.json".into()));
        assert!(urls.contains(&"https://api.example.com/openapi.yaml".into()));
        let bare = discovery_probe_urls(&Url::parse("https://api.example.com/").unwrap());
        assert_eq!(bare.len(), WELL_KNOWN_OPENAPI_PATHS.len());
        assert!(bare
            .iter()
            .all(|url| url.starts_with("https://api.example.com/")));
    }
}
