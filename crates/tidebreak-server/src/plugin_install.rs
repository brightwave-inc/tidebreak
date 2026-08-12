//! Pinned, instruction-only plugin import.
//!
//! Source bytes are fetched through the same public-HTTPS/denied-network
//! posture as native page extraction, unpacked into an in-memory tree, and
//! parsed with the existing `PLUGIN.md` / `SKILL.md` parsers. Runtime loading
//! never points at that untrusted tree: only the validated manifests and
//! bounded one-level helper scripts are copied into the install's data
//! directories, then the ordinary merged loaders re-read those copies.
//!
//! Three packaging shapes are accepted, chosen by what the archive actually
//! contains rather than by anything the request says:
//!
//! * a root `plugin.json` — a package in the Agent Plugins standard format
//!   (<https://agent-plugins.org>), whose components live at the fixed
//!   locations that specification defines;
//! * a `PLUGIN.md` — Tidebreak's own bundle manifest;
//! * a lone `SKILL.md` — a bare Agent Skills package, wrapped as a one-skill
//!   bundle.
//!
//! The two manifest formats differ in how strictly a bad member is treated,
//! and deliberately so. A `PLUGIN.md` *names* its members, so a member that
//! does not parse means the manifest describes something that is not there and
//! the whole import is refused. A standard package *discovers* its skills
//! structurally, and the specification requires a nonconforming one to be
//! skipped without sinking its siblings, so that is what the standard path
//! does — with every skip reported in the install response.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::web_search::{admit_fetch_address, admit_fetch_url};
use async_trait::async_trait;
use flate2::read::GzDecoder;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tidebreak_code_execution::{
    assess_plugin_compatibility, canonical_mcp_config, is_valid_plugin_name,
    parse_agent_plugin_manifest, parse_plugin_manifest, parse_plugin_mcp_config,
    parse_skill_manifest, LoadedSkill, PluginCategory, PluginCompatibility, PluginInstallStamp,
    PluginMcpConfig, PluginOrigin, PluginPackage, PluginSourceFormat, SkillOrigin, SkillScript,
    AGENT_PLUGIN_MANIFEST_FILE, AGENT_PLUGIN_MCP_FILE, AGENT_PLUGIN_SKILLS_DIR,
    AGENT_PLUGIN_SPEC_VERSION, PLUGIN_INSTALL_STAMP_FILE, PLUGIN_INSTALL_STAMP_SCHEMA,
    PLUGIN_MANIFEST_FILE, SKILL_MANIFEST_FILE, SKILL_SCRIPTS_DIR,
};
use url::{Host, Url};

use tidebreak_code_execution::MAX_WORKSPACE_FILE_BYTES;

pub(crate) const MAX_PLUGIN_INSTALL_BODY_BYTES: usize = 16 * 1024;

const MAX_ARCHIVE_BYTES: usize = 16 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 1_024;
const MAX_SKIPPED_MEMBERS: usize = 32;
/// The one-line bound the internal `PLUGIN.md` grammar holds a description to.
/// A standard manifest's description is type-checked only, so it is normalized
/// to this bound at conversion instead of failing the import.
const MAX_DESCRIPTION_BYTES: usize = 200;
const MAX_REDIRECTS: usize = 5;
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
const USER_AGENT: &str =
    "TidebreakPluginImporter/1.0 (+https://github.com/brightwave-inc/openwave)";

/// A source whose immutable identity is explicit in the request.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum PluginInstallSource {
    /// A public HTTPS git repository. The importer fetches its archive for the
    /// supplied tag or full commit SHA; it never executes the git client.
    Git { url: String, revision: String },
    /// A public HTTPS zip or tar archive whose URL already names the supplied
    /// tag or commit SHA.
    Archive { url: String, revision: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PluginInstallRequest {
    pub source: PluginInstallSource,
}

/// One archive member the instruction-only importer deliberately left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub(crate) struct SkippedPluginMember {
    pub path: String,
    pub reason: String,
}

/// Result of one successful import. The full installed shape is available
/// from `GET /plugins`; this response carries the import-only disclosures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub(crate) struct PluginInstallOutcome {
    pub plugin: String,
    pub revision: String,
    pub compatibility: PluginCompatibility,
    pub skipped: Vec<SkippedPluginMember>,
}

#[derive(Debug, Error)]
pub(crate) enum PluginInstallError {
    #[error("plugin source is invalid: {0}")]
    InvalidSource(String),
    #[error("plugin source could not be fetched: {0}")]
    Fetch(String),
    #[error("plugin archive is invalid: {0}")]
    InvalidArchive(String),
    #[error("plugin content is invalid: {0}")]
    InvalidPlugin(String),
    #[error("plugin conflicts with installed content: {0}")]
    Conflict(String),
    #[error("plugin could not be installed: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub(crate) trait PluginArchiveFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, PluginInstallError>;
}

#[derive(Debug, Default)]
pub(crate) struct HttpsPluginArchiveFetcher;

#[async_trait]
impl PluginArchiveFetcher for HttpsPluginArchiveFetcher {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, PluginInstallError> {
        fetch_archive(url).await
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedPluginSource {
    pub source_url: String,
    pub archive_url: String,
    pub revision: String,
}

pub(crate) fn resolve_source(
    source: &PluginInstallSource,
) -> Result<ResolvedPluginSource, PluginInstallError> {
    let (url, revision) = match source {
        PluginInstallSource::Git { url, revision }
        | PluginInstallSource::Archive { url, revision } => (url, revision),
    };
    if !valid_revision(revision) {
        return Err(PluginInstallError::InvalidSource(
            "revision must be a bounded git tag or full commit SHA".to_owned(),
        ));
    }
    let admitted = admit_fetch_url(url)
        .map_err(|error| PluginInstallError::InvalidSource(error.to_string()))?;
    let source_url = admitted.to_string();
    let archive_url = match source {
        PluginInstallSource::Git { .. } => git_archive_url(admitted, revision)?,
        PluginInstallSource::Archive { .. } => {
            let encoded_slashes = revision.replace('/', "%2F");
            let encoded_slashes_lower = revision.replace('/', "%2f");
            if !admitted.as_str().contains(revision)
                && !admitted.as_str().contains(&encoded_slashes)
                && !admitted.as_str().contains(&encoded_slashes_lower)
            {
                return Err(PluginInstallError::InvalidSource(
                    "archive URL must contain its tag or commit SHA".to_owned(),
                ));
            }
            admitted.to_string()
        }
    };
    Ok(ResolvedPluginSource {
        source_url,
        archive_url,
        revision: revision.clone(),
    })
}

fn valid_revision(revision: &str) -> bool {
    if revision.is_empty()
        || revision.len() > 255
        || revision.starts_with('/')
        || revision.ends_with('/')
        || revision.contains("//")
        || revision.contains("..")
    {
        return false;
    }
    revision.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && !segment.ends_with(".lock")
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
}

fn git_archive_url(mut repository: Url, revision: &str) -> Result<String, PluginInstallError> {
    if repository.query().is_some() {
        return Err(PluginInstallError::InvalidSource(
            "git repository URL must not carry a query".to_owned(),
        ));
    }
    let mut path = repository.path().trim_end_matches('/').to_owned();
    if let Some(without_git) = path.strip_suffix(".git") {
        path = without_git.to_owned();
    }
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments.len() < 2 {
        return Err(PluginInstallError::InvalidSource(
            "git repository URL must name an owner and repository".to_owned(),
        ));
    }
    repository.set_query(None);
    let archive_ref =
        if repository.host_str() == Some("github.com") && !is_full_commit_sha(revision) {
            format!("refs/tags/{revision}")
        } else {
            revision.to_owned()
        };
    repository.set_path(&format!("{path}/archive/{archive_ref}.tar.gz"));
    Ok(repository.to_string())
}

fn is_full_commit_sha(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn fetch_archive(url: &str) -> Result<Vec<u8>, PluginInstallError> {
    let started = Instant::now();
    let mut current = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let admitted = admit_fetch_url(&current)
            .map_err(|error| PluginInstallError::Fetch(error.to_string()))?;
        let remaining = FETCH_TIMEOUT
            .checked_sub(started.elapsed())
            .ok_or_else(|| PluginInstallError::Fetch("request timed out".to_owned()))?;
        let addresses = vetted_addresses(&admitted, remaining).await?;
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .user_agent(USER_AGENT)
            .connect_timeout(remaining.min(Duration::from_secs(10)))
            .timeout(remaining);
        if let Some(Host::Domain(host)) = admitted.host() {
            let pinned = addresses
                .iter()
                .map(|address| SocketAddr::new(*address, 443))
                .collect::<Vec<_>>();
            builder = builder.resolve_to_addrs(host, &pinned);
        }
        let client = builder
            .build()
            .map_err(|error| fetch_transport_error(error.without_url()))?;
        let response = client
            .get(admitted.clone())
            .header(
                reqwest::header::ACCEPT,
                "application/zip, application/gzip, application/x-gzip, application/x-tar, application/octet-stream",
            )
            .send()
            .await
            .map_err(|error| fetch_transport_error(error.without_url()))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| admitted.join(value).ok())
                .ok_or_else(|| PluginInstallError::Fetch("redirect was malformed".to_owned()))?;
            current = location.to_string();
            continue;
        }
        if !response.status().is_success() {
            return Err(PluginInstallError::Fetch(format!(
                "source answered HTTP {}",
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_ARCHIVE_BYTES as u64)
        {
            return Err(PluginInstallError::Fetch(
                "archive exceeds the download byte limit".to_owned(),
            ));
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| fetch_transport_error(error.without_url()))?;
            if body.len().saturating_add(chunk.len()) > MAX_ARCHIVE_BYTES {
                return Err(PluginInstallError::Fetch(
                    "archive exceeds the download byte limit".to_owned(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        return Ok(body);
    }
    Err(PluginInstallError::Fetch(
        "source redirected too many times".to_owned(),
    ))
}

async fn vetted_addresses(
    url: &Url,
    remaining: Duration,
) -> Result<Vec<IpAddr>, PluginInstallError> {
    let addresses = match url.host() {
        Some(Host::Ipv4(address)) => vec![IpAddr::V4(address)],
        Some(Host::Ipv6(address)) => vec![IpAddr::V6(address)],
        Some(Host::Domain(host)) => {
            let resolved = tokio::time::timeout(
                remaining.min(DNS_TIMEOUT),
                tokio::net::lookup_host((host, 443)),
            )
            .await
            .map_err(|_| PluginInstallError::Fetch("source host did not resolve".to_owned()))?
            .map_err(|_| PluginInstallError::Fetch("source host did not resolve".to_owned()))?;
            let mut addresses = Vec::new();
            for address in resolved.map(|socket| socket.ip()) {
                if !addresses.contains(&address) {
                    addresses.push(address);
                }
            }
            addresses
        }
        None => Vec::new(),
    };
    if addresses.is_empty() {
        return Err(PluginInstallError::Fetch(
            "source host did not resolve".to_owned(),
        ));
    }
    if addresses
        .iter()
        .any(|address| admit_fetch_address(*address).is_err())
    {
        return Err(PluginInstallError::Fetch(
            "source host is not a public destination".to_owned(),
        ));
    }
    Ok(addresses)
}

fn fetch_transport_error(error: reqwest::Error) -> PluginInstallError {
    if error.is_timeout() {
        PluginInstallError::Fetch("request timed out".to_owned())
    } else {
        PluginInstallError::Fetch(error.to_string())
    }
}

#[derive(Debug)]
pub(crate) struct PreparedPlugin {
    pub package: PluginPackage,
    pub manifest: String,
    /// The canonical `mcp.json` to retain beside the manifest, regenerated
    /// from the entries that validated. `None` when the package ships no
    /// configuration, when its configuration was rejected whole, or when
    /// nothing in it survived validation.
    pub mcp_config: Option<String>,
    pub skills: Vec<LoadedSkill>,
    pub stamp: PluginInstallStamp,
    pub skipped: Vec<SkippedPluginMember>,
}

#[derive(Debug, Default)]
struct ArchiveTree {
    files: BTreeMap<Vec<String>, Vec<u8>>,
    paths: BTreeSet<Vec<String>>,
}

pub(crate) fn prepare_plugin(
    archive: &[u8],
    source: &ResolvedPluginSource,
) -> Result<PreparedPlugin, PluginInstallError> {
    let tree = unpack_archive(archive)?;
    prepare_tree(tree, source)
}

fn unpack_archive(bytes: &[u8]) -> Result<ArchiveTree, PluginInstallError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(PluginInstallError::InvalidArchive(
            "archive exceeds the download byte limit".to_owned(),
        ));
    }
    let tree = if bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
    {
        unpack_zip(bytes)?
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        unpack_tar(GzDecoder::new(Cursor::new(bytes)))?
    } else {
        unpack_tar(Cursor::new(bytes))?
    };
    strip_common_root(tree)
}

fn unpack_zip(bytes: &[u8]) -> Result<ArchiveTree, PluginInstallError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| {
        PluginInstallError::InvalidArchive("source is not a readable zip archive".to_owned())
    })?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(PluginInstallError::InvalidArchive(
            "archive contains too many entries".to_owned(),
        ));
    }
    let mut tree = ArchiveTree::default();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| {
            PluginInstallError::InvalidArchive("zip entry is unreadable".to_owned())
        })?;
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(PluginInstallError::InvalidArchive(
                "archive expands beyond the byte limit".to_owned(),
            ));
        }
        let path = entry.enclosed_name().ok_or_else(|| {
            PluginInstallError::InvalidArchive("archive path escapes its root".to_owned())
        })?;
        let path = normalized_path(&path)?;
        if path.is_empty() || entry.is_dir() {
            continue;
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 {
                return Err(PluginInstallError::InvalidArchive(
                    "archive contains a link or special file".to_owned(),
                ));
            }
        }
        insert_entry(&mut tree, path, entry.size(), &mut entry)?;
    }
    Ok(tree)
}

fn unpack_tar(reader: impl Read) -> Result<ArchiveTree, PluginInstallError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive.entries().map_err(|_| {
        PluginInstallError::InvalidArchive("source is not a readable tar archive".to_owned())
    })?;
    let mut tree = ArchiveTree::default();
    let mut total = 0_u64;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            return Err(PluginInstallError::InvalidArchive(
                "archive contains too many entries".to_owned(),
            ));
        }
        let mut entry = entry.map_err(|_| {
            PluginInstallError::InvalidArchive("tar entry is unreadable".to_owned())
        })?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(PluginInstallError::InvalidArchive(
                "archive contains a link or special file".to_owned(),
            ));
        }
        let size = entry.header().size().map_err(|_| {
            PluginInstallError::InvalidArchive("tar entry size is invalid".to_owned())
        })?;
        total = total.saturating_add(size);
        if total > MAX_UNPACKED_BYTES {
            return Err(PluginInstallError::InvalidArchive(
                "archive expands beyond the byte limit".to_owned(),
            ));
        }
        let path = entry.path().map_err(|_| {
            PluginInstallError::InvalidArchive("tar entry path is invalid".to_owned())
        })?;
        let path = normalized_path(&path)?;
        if path.is_empty() {
            continue;
        }
        insert_entry(&mut tree, path, size, &mut entry)?;
    }
    Ok(tree)
}

fn normalized_path(path: &Path) -> Result<Vec<String>, PluginInstallError> {
    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => {
                let value = value.to_str().ok_or_else(|| {
                    PluginInstallError::InvalidArchive("archive path is not valid UTF-8".to_owned())
                })?;
                if value.is_empty()
                    || value.len() > 255
                    || value.chars().any(char::is_control)
                    || value.bytes().any(|byte| {
                        matches!(byte, b'\\' | b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|')
                    })
                {
                    return Err(PluginInstallError::InvalidArchive(
                        "archive path is not portable across supported systems".to_owned(),
                    ));
                }
                normalized.push(value.to_owned());
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PluginInstallError::InvalidArchive(
                    "archive path escapes its root".to_owned(),
                ));
            }
        }
    }
    if normalized.iter().map(String::len).sum::<usize>() + normalized.len() > 1_024 {
        return Err(PluginInstallError::InvalidArchive(
            "archive path exceeds the byte limit".to_owned(),
        ));
    }
    Ok(normalized)
}

fn insert_entry(
    tree: &mut ArchiveTree,
    path: Vec<String>,
    size: u64,
    reader: &mut impl Read,
) -> Result<(), PluginInstallError> {
    if !tree.paths.insert(path.clone()) {
        return Err(PluginInstallError::InvalidArchive(
            "archive contains duplicate paths".to_owned(),
        ));
    }
    if !retain_archive_file(&path) {
        return Ok(());
    }
    if size > MAX_WORKSPACE_FILE_BYTES as u64 {
        return Err(PluginInstallError::InvalidArchive(format!(
            "{} exceeds the per-file byte limit",
            path.join("/")
        )));
    }
    let mut content = Vec::with_capacity(size as usize);
    reader
        .take(MAX_WORKSPACE_FILE_BYTES as u64 + 1)
        .read_to_end(&mut content)
        .map_err(|_| {
            PluginInstallError::InvalidArchive(format!(
                "{} could not be decompressed",
                path.join("/")
            ))
        })?;
    if content.len() > MAX_WORKSPACE_FILE_BYTES {
        return Err(PluginInstallError::InvalidArchive(format!(
            "{} exceeds the per-file byte limit",
            path.join("/")
        )));
    }
    tree.files.insert(path, content);
    Ok(())
}

/// Which archive members are worth holding in memory: the manifests both
/// formats are defined by, and one level of helper scripts.
fn retain_archive_file(path: &[String]) -> bool {
    path.last()
        .is_some_and(|name| name == PLUGIN_MANIFEST_FILE || name == SKILL_MANIFEST_FILE)
        || is_package_root_file(path, AGENT_PLUGIN_MANIFEST_FILE)
        || is_package_root_file(path, AGENT_PLUGIN_MCP_FILE)
        || path.iter().any(|component| component == SKILL_SCRIPTS_DIR)
}

/// Whether `path` is `file` sitting where a standard package's root can be.
///
/// That specification puts `plugin.json` at the package root, so the only
/// candidate positions are the top of the archive and one directory in, which
/// is what a repository tarball adds. A file with the same basename anywhere
/// else — a skill's `scripts/plugin.json`, say — is ordinary skill content and
/// must not be mistaken for a package manifest, or a perfectly good
/// `PLUGIN.md` bundle would be routed into the wrong format.
fn is_package_root_file(path: &[String], file: &str) -> bool {
    path.len() <= 2
        && path.last().is_some_and(|name| name == file)
        && !path.iter().any(|component| component == SKILL_SCRIPTS_DIR)
}

fn strip_common_root(tree: ArchiveTree) -> Result<ArchiveTree, PluginInstallError> {
    let common = tree
        .paths
        .iter()
        .next()
        .and_then(|first| first.first())
        .filter(|first| {
            tree.paths
                .iter()
                .all(|path| path.len() >= 2 && path.first().is_some_and(|part| part == *first))
        })
        .cloned();
    let Some(common) = common else {
        return Ok(tree);
    };
    let strip = |mut path: Vec<String>| {
        debug_assert_eq!(path.first(), Some(&common));
        path.remove(0);
        path
    };
    let files = tree
        .files
        .into_iter()
        .map(|(path, content)| (strip(path), content))
        .collect();
    let paths = tree.paths.into_iter().map(strip).collect();
    Ok(ArchiveTree { files, paths })
}

#[derive(Debug)]
struct ParsedSkill {
    manifest_path: Vec<String>,
    root: Vec<String>,
    loaded: LoadedSkill,
    has_scripts: bool,
}

fn prepare_tree(
    tree: ArchiveTree,
    source: &ResolvedPluginSource,
) -> Result<PreparedPlugin, PluginInstallError> {
    // Format is decided by what the archive contains. A root `plugin.json`
    // means the package describes itself in the standard format, and its
    // components are discovered at that specification's fixed locations
    // instead of being named by an Tidebreak manifest. Only the positions a
    // package root can occupy count: a skill's helper script that happens to
    // be named `plugin.json` is skill content, not a manifest.
    let mut standard_manifests = tree
        .files
        .keys()
        .filter(|path| is_package_root_file(path, AGENT_PLUGIN_MANIFEST_FILE))
        .cloned()
        .collect::<Vec<_>>();
    if standard_manifests.len() > 1 {
        return Err(PluginInstallError::InvalidPlugin(
            "archive contains more than one plugin.json".to_owned(),
        ));
    }
    if let Some(manifest_path) = standard_manifests.pop() {
        return prepare_standard_tree(&tree, source, &manifest_path);
    }

    let mut plugin_manifests = Vec::new();
    let mut skills = Vec::new();
    for (path, bytes) in &tree.files {
        match path.last().map(String::as_str) {
            Some(PLUGIN_MANIFEST_FILE) => {
                let source = std::str::from_utf8(bytes).map_err(|_| {
                    PluginInstallError::InvalidPlugin(format!("{} is not UTF-8", path.join("/")))
                })?;
                let package = parse_plugin_manifest(source, PluginOrigin::User)
                    .map_err(|error| PluginInstallError::InvalidPlugin(error.to_string()))?;
                plugin_manifests.push((path.clone(), source.to_owned(), package));
            }
            Some(SKILL_MANIFEST_FILE) => {
                skills.push(parse_archived_skill(&tree, path, bytes)?);
            }
            _ => {}
        }
    }
    if plugin_manifests.len() > 1 {
        return Err(PluginInstallError::InvalidPlugin(
            "archive contains more than one PLUGIN.md".to_owned(),
        ));
    }
    if let Some((manifest_path, source_manifest, mut package)) = plugin_manifests.pop() {
        let root = manifest_path[..manifest_path.len() - 1].to_vec();
        let mut selected = Vec::new();
        let mut skipped = skipped_foreign_members(&tree, &root);
        skipped_bundled_mcp_config(&tree, &root, &mut skipped);
        for prompt in &package.prompts {
            push_skipped(
                &mut skipped,
                format!("prompts/{prompt}"),
                "reusable prompts are not installed by the instruction-only importer",
            );
        }
        package.prompts.clear();
        if package.skills.is_empty() {
            return Err(PluginInstallError::InvalidPlugin(
                "PLUGIN.md contains no installable skills".to_owned(),
            ));
        }
        for name in &package.skills {
            let expected = prefixed_path(&root, &["skills", name, SKILL_MANIFEST_FILE]);
            let skill = skills
                .iter()
                .find(|skill| skill.manifest_path == expected)
                .ok_or_else(|| {
                    PluginInstallError::InvalidPlugin(format!(
                        "PLUGIN.md member {name:?} has no skills/{name}/SKILL.md"
                    ))
                })?;
            if skill.loaded.package.name != *name {
                return Err(PluginInstallError::InvalidPlugin(format!(
                    "skills/{name}/SKILL.md names itself {:?}",
                    skill.loaded.package.name
                )));
            }
            selected.push(skill);
        }
        for skill in &skills {
            if skill.root.starts_with(&root)
                && !selected
                    .iter()
                    .any(|selected| selected.manifest_path == skill.manifest_path)
            {
                push_skipped(
                    &mut skipped,
                    skill.root.join("/"),
                    "skill is not declared by PLUGIN.md",
                );
            }
        }
        let compatibility = assess_plugin_compatibility(
            &selected
                .iter()
                .map(|skill| (&skill.loaded.package, skill.has_scripts))
                .collect::<Vec<_>>(),
        );
        package.compatibility = compatibility.clone();
        let manifest = canonical_plugin_manifest(&package, manifest_body(&source_manifest));
        // The generated installed manifest goes through the same parser before
        // any byte is copied, so pruning unsupported members cannot create a
        // shape the ordinary loader would reject later.
        parse_plugin_manifest(&manifest, PluginOrigin::User)
            .map_err(|error| PluginInstallError::InvalidPlugin(error.to_string()))?;
        return Ok(PreparedPlugin {
            stamp: install_stamp(source, PluginSourceFormat::PluginManifest, compatibility),
            manifest,
            mcp_config: None,
            skills: selected
                .into_iter()
                .map(|skill| skill.loaded.clone())
                .collect(),
            package,
            skipped,
        });
    }

    if skills.len() != 1 {
        return Err(PluginInstallError::InvalidPlugin(if skills.is_empty() {
            "archive contains no PLUGIN.md or SKILL.md".to_owned()
        } else {
            "an archive with multiple skills requires a PLUGIN.md".to_owned()
        }));
    }
    let skill = skills.pop().expect("one skill was checked above");
    // The specific disclosure goes in first: a bare package's generic
    // "outside SKILL.md and one-level scripts/" reason would otherwise be the
    // one a response shows for a file the importer has a better answer about.
    let mut skipped = Vec::new();
    skipped_bundled_mcp_config(&tree, &skill.root, &mut skipped);
    for member in skipped_single_skill_members(&tree, &skill.root) {
        push_skipped(&mut skipped, member.path, &member.reason);
    }
    let compatibility = assess_plugin_compatibility(&[(&skill.loaded.package, skill.has_scripts)]);
    let display_name = display_name(&skill.loaded.package.name);
    let mut package = PluginPackage {
        name: skill.loaded.package.name.clone(),
        display_name,
        description: skill.loaded.package.description.clone(),
        category: PluginCategory::Other,
        skills: vec![skill.loaded.package.name.clone()],
        prompts: Vec::new(),
        router_preamble: None,
        mcp_servers: 0,
        origin: PluginOrigin::User,
        compatibility: compatibility.clone(),
    };
    let manifest = canonical_plugin_manifest(&package, "");
    package = parse_plugin_manifest(&manifest, PluginOrigin::User)
        .map_err(|error| PluginInstallError::InvalidPlugin(error.to_string()))?;
    package.compatibility = compatibility.clone();
    // Avoid an unused-mut warning when the archive has no unsupported files;
    // the mutable binding is used by the capped helper above.
    skipped.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(PreparedPlugin {
        stamp: install_stamp(source, PluginSourceFormat::PluginManifest, compatibility),
        manifest,
        mcp_config: None,
        skills: vec![skill.loaded],
        package,
        skipped,
    })
}

/// Convert one package published in the Agent Plugins standard format into the
/// internal bundle shape.
///
/// Skills are discovered structurally: every immediate child directory of
/// `skills/` that holds a regular `SKILL.md` is one skill, with no recursion
/// below that. A child that does not conform is skipped and reported — the
/// specification grades a nonconforming skill as a per-skill failure, so it
/// must not take its siblings or the rest of the package down with it. A
/// package that yields no skill at all is refused, because an Tidebreak bundle
/// with no members is not a shape the catalog can render.
fn prepare_standard_tree(
    tree: &ArchiveTree,
    source: &ResolvedPluginSource,
    manifest_path: &[String],
) -> Result<PreparedPlugin, PluginInstallError> {
    let root = manifest_path[..manifest_path.len() - 1].to_vec();
    let bytes = tree
        .files
        .get(manifest_path)
        .expect("the manifest path came from the retained files");
    let manifest_source = std::str::from_utf8(bytes).map_err(|_| {
        PluginInstallError::InvalidPlugin(format!("{} is not UTF-8", manifest_path.join("/")))
    })?;
    let parsed = parse_agent_plugin_manifest(manifest_source)
        .map_err(|error| PluginInstallError::InvalidPlugin(error.to_string()))?;
    let name = parsed.manifest.name;
    // The standard's package-name grammar admits `.`; Tidebreak addresses a
    // plugin by a dot-free kebab-case slug everywhere from the toggle route to
    // the installed directory name, so a dotted package is refused with a
    // reason rather than silently renamed.
    if !is_valid_plugin_name(&name) {
        return Err(PluginInstallError::InvalidPlugin(format!(
            "plugin.json names itself {name:?}; Tidebreak installs plugins whose name is a \
             kebab-case slug of lowercase letters, digits, and single dashes"
        )));
    }

    let mut skipped = skipped_foreign_members(tree, &root);
    for ignored in &parsed.ignored {
        push_skipped(
            &mut skipped,
            format!("{AGENT_PLUGIN_MANIFEST_FILE}#{}", ignored.field),
            &ignored.reason,
        );
    }
    let mcp_config = prepare_mcp_config(tree, &root, &mut skipped)?;

    let skills_root = prefixed_path(&root, &[AGENT_PLUGIN_SKILLS_DIR]);
    let mut selected: Vec<ParsedSkill> = Vec::new();
    for child in standard_skill_children(tree, &skills_root) {
        let directory = prefixed_path(&skills_root, &[child.as_str()]);
        let manifest = prefixed_path(&directory, &[SKILL_MANIFEST_FILE]);
        let Some(bytes) = tree.files.get(&manifest) else {
            push_skipped(
                &mut skipped,
                directory.join("/"),
                "directory has no SKILL.md and is not a skill",
            );
            continue;
        };
        match parse_archived_skill(tree, &manifest, bytes) {
            Ok(skill) => selected.push(skill),
            Err(error) => push_skipped(&mut skipped, directory.join("/"), &error.to_string()),
        }
    }
    if selected.is_empty() {
        return Err(PluginInstallError::InvalidPlugin(
            "plugin has no conforming skill under skills/, and skills are the only \
             component type this importer installs"
                .to_owned(),
        ));
    }

    let compatibility = assess_plugin_compatibility(
        &selected
            .iter()
            .map(|skill| (&skill.loaded.package, skill.has_scripts))
            .collect::<Vec<_>>(),
    );
    let mut package = PluginPackage {
        display_name: display_name(&name),
        description: bounded_description(parsed.manifest.description.as_deref(), &name),
        category: parsed.manifest.category.unwrap_or(PluginCategory::Other),
        skills: selected
            .iter()
            .map(|skill| skill.loaded.package.name.clone())
            .collect(),
        // Reusable prompts have no place in the standard format, so unlike the
        // `PLUGIN.md` path there is nothing here to prune and disclose.
        prompts: Vec::new(),
        router_preamble: parsed.manifest.router_preamble,
        mcp_servers: mcp_config.as_ref().map_or(0, |config| config.servers.len()),
        origin: PluginOrigin::User,
        compatibility: compatibility.clone(),
        name,
    };
    let manifest = canonical_plugin_manifest(&package, "");
    // The generated manifest goes through the internal parser before any byte
    // is copied, exactly as the `PLUGIN.md` path does: nothing reaches disk in
    // a shape the ordinary loader would later reject.
    let mcp_servers = package.mcp_servers;
    package = parse_plugin_manifest(&manifest, PluginOrigin::User)
        .map_err(|error| PluginInstallError::InvalidPlugin(error.to_string()))?;
    package.compatibility = compatibility.clone();
    package.mcp_servers = mcp_servers;
    skipped.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(PreparedPlugin {
        stamp: install_stamp(
            source,
            PluginSourceFormat::AgentPlugins {
                spec_version: AGENT_PLUGIN_SPEC_VERSION.to_owned(),
            },
            compatibility,
        ),
        manifest,
        mcp_config: mcp_config
            .filter(|config| !config.servers.is_empty())
            .as_ref()
            .map(canonical_mcp_config),
        skills: selected.into_iter().map(|skill| skill.loaded).collect(),
        package,
        skipped,
    })
}

/// Validate the package's bundled MCP server configuration, if it ships one.
///
/// The specification grades MCP failures narrowly: a top-level problem —
/// unreadable JSON, a schema identifier this client does not implement, a
/// shape that is not `$schema` plus `mcpServers` — disables MCP for this
/// plugin and nothing else, while a single bad entry drops only that entry.
/// Both are reported through the ordinary skip disclosure, so an install
/// response says what will not connect and why.
fn prepare_mcp_config(
    tree: &ArchiveTree,
    root: &[String],
    skipped: &mut Vec<SkippedPluginMember>,
) -> Result<Option<PluginMcpConfig>, PluginInstallError> {
    let path = prefixed_path(root, &[AGENT_PLUGIN_MCP_FILE]);
    let Some(bytes) = tree.files.get(&path) else {
        return Ok(None);
    };
    let source = std::str::from_utf8(bytes).map_err(|_| {
        PluginInstallError::InvalidPlugin(format!("{} is not UTF-8", path.join("/")))
    })?;
    match parse_plugin_mcp_config(source) {
        Ok(parsed) => {
            for server in &parsed.skipped {
                push_skipped(
                    skipped,
                    format!("{AGENT_PLUGIN_MCP_FILE}#{}", server.name),
                    &server.reason,
                );
            }
            Ok(Some(parsed.config))
        }
        Err(error) => {
            push_skipped(
                skipped,
                AGENT_PLUGIN_MCP_FILE.to_owned(),
                &error.to_string(),
            );
            Ok(None)
        }
    }
}

/// The immediate child directory names under `skills_root`, in name order.
///
/// A path one level below the root is a file, not a skill directory, so it is
/// not a child; discovery never descends past this level.
fn standard_skill_children(tree: &ArchiveTree, skills_root: &[String]) -> Vec<String> {
    tree.paths
        .iter()
        .filter(|path| path.starts_with(skills_root) && path.len() > skills_root.len() + 1)
        .map(|path| path[skills_root.len()].clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// One printable, bounded line for a description the standard only type-checks.
///
/// A published description may be several sentences long or carry newlines;
/// the internal manifest grammar accepts exactly one bounded printable line, so
/// it is normalized here rather than rejected. An absent or whitespace-only
/// description falls back to a derived line, since the internal parser has no
/// notion of a bundle without one.
fn bounded_description(description: Option<&str>, name: &str) -> String {
    let normalized = description
        .unwrap_or_default()
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut end = MAX_DESCRIPTION_BYTES.min(normalized.len());
    while end > 0 && !normalized.is_char_boundary(end) {
        end -= 1;
    }
    let bounded = normalized[..end].trim_end();
    if bounded.is_empty() {
        format!("Skills bundled by the {name} plugin.")
    } else {
        bounded.to_owned()
    }
}

fn parse_archived_skill(
    tree: &ArchiveTree,
    manifest_path: &[String],
    bytes: &[u8],
) -> Result<ParsedSkill, PluginInstallError> {
    let manifest = std::str::from_utf8(bytes).map_err(|_| {
        PluginInstallError::InvalidPlugin(format!("{} is not UTF-8", manifest_path.join("/")))
    })?;
    let package = parse_skill_manifest(manifest, SkillOrigin::User)
        .map_err(|error| PluginInstallError::InvalidPlugin(error.to_string()))?;
    let root = manifest_path[..manifest_path.len() - 1].to_vec();
    if root
        .last()
        .is_some_and(|directory| directory != &package.name)
        && !root.is_empty()
    {
        return Err(PluginInstallError::InvalidPlugin(format!(
            "{} names itself {:?}",
            manifest_path.join("/"),
            package.name
        )));
    }
    let scripts_root = prefixed_path(&root, &[SKILL_SCRIPTS_DIR]);
    let has_scripts = tree
        .paths
        .iter()
        .any(|path| path.starts_with(&scripts_root));
    let mut scripts = Vec::new();
    for (path, content) in &tree.files {
        if !path.starts_with(&scripts_root) || path.len() != scripts_root.len() + 1 {
            continue;
        }
        scripts.push(SkillScript {
            name: path.last().expect("script path has a file name").clone(),
            content: content.clone(),
        });
    }
    scripts.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ParsedSkill {
        manifest_path: manifest_path.to_vec(),
        root,
        loaded: LoadedSkill {
            package,
            manifest: manifest.to_owned(),
            scripts,
        },
        has_scripts,
    })
}

fn canonical_plugin_manifest(package: &PluginPackage, body: &str) -> String {
    let skills = package
        .skills
        .iter()
        .map(|skill| format!("\"{skill}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut manifest = format!(
        "---\nname: {}\ndisplay-name: {}\ndescription: {}\ncategory: {}\nskills: [{}]\n",
        package.name,
        package.display_name,
        package.description,
        category_name(package.category),
        skills
    );
    if let Some(preamble) = &package.router_preamble {
        manifest.push_str(&format!("router-preamble: {preamble}\n"));
    }
    manifest.push_str("---\n");
    if !body.trim().is_empty() {
        manifest.push('\n');
        manifest.push_str(body.trim());
        manifest.push('\n');
    }
    manifest
}

fn category_name(category: PluginCategory) -> &'static str {
    match category {
        PluginCategory::Documents => "documents",
        PluginCategory::Data => "data",
        PluginCategory::Visualization => "visualization",
        PluginCategory::Other => "other",
    }
}

fn manifest_body(manifest: &str) -> &str {
    manifest
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or("", |(_, body)| body)
}

fn display_name(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), characters.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn install_stamp(
    source: &ResolvedPluginSource,
    source_format: PluginSourceFormat,
    compatibility: PluginCompatibility,
) -> PluginInstallStamp {
    PluginInstallStamp {
        schema_version: PLUGIN_INSTALL_STAMP_SCHEMA,
        source_url: source.source_url.clone(),
        revision: source.revision.clone(),
        source_format,
        compatibility,
    }
}

fn prefixed_path(root: &[String], suffix: &[&str]) -> Vec<String> {
    root.iter()
        .cloned()
        .chain(suffix.iter().map(|part| (*part).to_owned()))
        .collect()
}

fn skipped_foreign_members(tree: &ArchiveTree, root: &[String]) -> Vec<SkippedPluginMember> {
    let mut skipped = Vec::new();
    for component in [
        ".claude-plugin",
        "agents",
        "commands",
        "hooks",
        "mcp",
        "mcp-servers",
    ] {
        let prefix = prefixed_path(root, &[component]);
        if tree.paths.iter().any(|path| path.starts_with(&prefix)) {
            push_skipped(
                &mut skipped,
                prefix.join("/"),
                "component type is outside the instruction-only importer",
            );
        }
    }
    skipped
}

/// Report an `mcp.json` an archive ships in a format that does not define one.
///
/// Bundled MCP configuration is a component of the Agent Plugins standard
/// format, read from that specification's fixed location. A `PLUGIN.md` bundle
/// or a bare skill package carrying the same file is describing something this
/// importer will not act on, so it is disclosed rather than dropped in silence.
fn skipped_bundled_mcp_config(
    tree: &ArchiveTree,
    root: &[String],
    skipped: &mut Vec<SkippedPluginMember>,
) {
    let path = prefixed_path(root, &[AGENT_PLUGIN_MCP_FILE]);
    if tree.paths.contains(&path) {
        push_skipped(
            skipped,
            path.join("/"),
            "bundled MCP configuration is only read from Agent Plugins packages",
        );
    }
}

fn skipped_single_skill_members(tree: &ArchiveTree, root: &[String]) -> Vec<SkippedPluginMember> {
    let mut skipped = Vec::new();
    let scripts = prefixed_path(root, &[SKILL_SCRIPTS_DIR]);
    for path in &tree.paths {
        if !path.starts_with(root)
            || path == &prefixed_path(root, &[SKILL_MANIFEST_FILE])
            || path.starts_with(&scripts)
        {
            continue;
        }
        let relative = &path[root.len()..];
        if let Some(first) = relative.first() {
            push_skipped(
                &mut skipped,
                prefixed_path(root, &[first]).join("/"),
                "content is outside SKILL.md and one-level scripts/",
            );
        }
    }
    skipped
}

fn push_skipped(skipped: &mut Vec<SkippedPluginMember>, path: String, reason: &str) {
    if skipped.iter().any(|entry| entry.path == path) {
        return;
    }
    if skipped.len() >= MAX_SKIPPED_MEMBERS - 1 {
        if !skipped.iter().any(|entry| entry.path == "…") {
            skipped.push(SkippedPluginMember {
                path: "…".to_owned(),
                reason: "additional unsupported members were omitted from this bounded response"
                    .to_owned(),
            });
        }
        return;
    }
    skipped.push(SkippedPluginMember {
        path,
        reason: reason.to_owned(),
    });
}

#[derive(Debug)]
pub(crate) struct InstalledPluginFiles {
    pub plugin_dir: PathBuf,
    pub skill_dirs: Vec<PathBuf>,
}

pub(crate) fn install_prepared(
    prepared: &PreparedPlugin,
    plugins_root: &Path,
    skills_root: &Path,
) -> Result<InstalledPluginFiles, PluginInstallError> {
    std::fs::create_dir_all(plugins_root)?;
    std::fs::create_dir_all(skills_root)?;
    let plugin_dir = plugins_root.join(&prepared.package.name);
    if plugin_dir.exists() {
        return Err(PluginInstallError::Conflict(format!(
            "plugin {:?} already exists",
            prepared.package.name
        )));
    }
    let skill_dirs = prepared
        .skills
        .iter()
        .map(|skill| skills_root.join(&skill.package.name))
        .collect::<Vec<_>>();
    if let Some(existing) = skill_dirs.iter().find(|path| path.exists()) {
        return Err(PluginInstallError::Conflict(format!(
            "skill {:?} already exists",
            existing
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
        )));
    }

    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let plugin_stage = plugins_root.join(format!(".install-{nonce}"));
    let skill_stages = prepared
        .skills
        .iter()
        .map(|skill| skills_root.join(format!(".install-{nonce}-{}", skill.package.name)))
        .collect::<Vec<_>>();
    let mut installed_skills = Vec::new();
    let result = (|| -> Result<(), PluginInstallError> {
        std::fs::create_dir(&plugin_stage)?;
        std::fs::write(
            plugin_stage.join(PLUGIN_MANIFEST_FILE),
            prepared.manifest.as_bytes(),
        )?;
        std::fs::write(
            plugin_stage.join(PLUGIN_INSTALL_STAMP_FILE),
            serde_json::to_vec_pretty(&prepared.stamp).map_err(|error| {
                PluginInstallError::InvalidPlugin(format!(
                    "compatibility stamp could not be serialized: {error}"
                ))
            })?,
        )?;
        if let Some(config) = &prepared.mcp_config {
            std::fs::write(plugin_stage.join(AGENT_PLUGIN_MCP_FILE), config.as_bytes())?;
        }
        for (skill, stage) in prepared.skills.iter().zip(&skill_stages) {
            std::fs::create_dir(stage)?;
            std::fs::write(stage.join(SKILL_MANIFEST_FILE), skill.manifest.as_bytes())?;
            if !skill.scripts.is_empty() {
                let scripts = stage.join(SKILL_SCRIPTS_DIR);
                std::fs::create_dir(&scripts)?;
                for script in &skill.scripts {
                    std::fs::write(scripts.join(&script.name), &script.content)?;
                }
            }
        }
        for (stage, destination) in skill_stages.iter().zip(&skill_dirs) {
            std::fs::rename(stage, destination)?;
            installed_skills.push(destination.clone());
        }
        std::fs::rename(&plugin_stage, &plugin_dir)?;
        Ok(())
    })();
    if let Err(error) = result {
        for path in installed_skills.iter().chain(skill_stages.iter()) {
            remove_created_directory(path);
        }
        remove_created_directory(&plugin_stage);
        return Err(error);
    }
    Ok(InstalledPluginFiles {
        plugin_dir,
        skill_dirs,
    })
}

pub(crate) fn rollback_install(files: &InstalledPluginFiles) {
    remove_created_directory(&files.plugin_dir);
    for skill in &files.skill_dirs {
        remove_created_directory(skill);
    }
}

fn remove_created_directory(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                "could not remove incomplete plugin install {}: {error}",
                path.display()
            );
        }
    }
}

pub(crate) fn default_fetcher() -> Arc<dyn PluginArchiveFetcher> {
    Arc::new(HttpsPluginArchiveFetcher)
}
