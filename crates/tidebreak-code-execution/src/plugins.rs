//! Plugins: thin manifests that group skills and prompts into one installable
//! bundle.
//!
//! A skill stays the model's routing unit — the prompt catalog still lists
//! `(name, description)` lines and the instruction body still reaches the
//! model only through `read_file` of the staged `SKILL.md`. A plugin adds a
//! layer above that atom: a directory whose `PLUGIN.md` names a set of member
//! skills, carries the display identity a UI needs, and may declare one
//! *router preamble* — a single line emitted above its skills in the catalog
//! when they are alternatives for the same intent and the model needs help
//! choosing between them.
//!
//! A bundle may also claim member **prompts** (see [`crate::prompts`]), which
//! are the opposite kind of thing: inert user-side text a person inserts into
//! the composer, with no catalog line and no staged bytes. They are members
//! here only so a bundle can ship the starting messages that go with its
//! skills, and so switching the bundle off takes them out of the library
//! together. A bundle may carry skills, prompts, or both.
//!
//! Plugins are packaging, not a prompt concept. A skill claimed by no plugin
//! keeps working exactly as before, which is what keeps user-authored skills
//! in the data directory unaffected by this layer.
//!
//! Parsing follows `skills.rs` exactly: hand-rolled strict frontmatter, a
//! closed key set, byte bounds and printability on every emitted string, and
//! skip-with-warning per package so one bad manifest can never break the
//! prompt. Membership is validated against the skills that actually loaded, so
//! a plugin naming a skill that isn't there is skipped whole rather than
//! advertising a group the catalog cannot render.
//!
//! User-authored bundles under the data directory load through that same
//! parser and merge behind the built-in tree — see [`merged_plugins`] for the
//! precedence rules, which mirror the ones user skills already follow.
//!
//! Capability badges follow the same posture from the other direction: they
//! are derived from what the member skills actually declare, and the closed
//! key set means a manifest cannot state them at all.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

use crate::prompts::{is_valid_prompt_name, LoadedPrompt};
use crate::skills::{is_valid_skill_name, LoadedSkill, SkillPackage};

/// The manifest file every plugin package is defined by.
pub const PLUGIN_MANIFEST_FILE: &str = "PLUGIN.md";

/// Host-owned metadata written beside an imported plugin manifest.
///
/// It is deliberately separate from `PLUGIN.md`: that manifest's closed key
/// set remains content-authored, while source provenance and compatibility are
/// facts the importer derives. A hand-authored user plugin without this file
/// stays loadable, but is reported as unchecked rather than silently receiving
/// an assurance it never earned.
pub const PLUGIN_INSTALL_STAMP_FILE: &str = ".tidebreak-install.json";

/// Current on-disk install-stamp schema.
pub const PLUGIN_INSTALL_STAMP_SCHEMA: u32 = 1;

const MAX_INSTALL_STAMP_BYTES: usize = 64 * 1024;
const MAX_INSTALL_SOURCE_URL_BYTES: usize = 2_048;
// A plugin may carry 16 skills, each with eight Python pins and a scripts/
// finding: 16 * (8 + 1) = 144. Leave a little schema headroom while keeping a
// hand-edited stamp sharply bounded.
const MAX_COMPATIBILITY_ISSUES: usize = 160;

const MAX_NAME_BYTES: usize = 64;
const MAX_DISPLAY_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 200;
const MAX_ROUTER_PREAMBLE_BYTES: usize = 300;
const MAX_MEMBER_SKILLS: usize = 16;
const MAX_MEMBER_PROMPTS: usize = 32;

/// What kind of work a plugin bundles, from a closed vocabulary.
///
/// Closed on purpose, like [`crate::HostDep`]: an unknown value rejects the
/// manifest instead of parsing into a string no grouping or badge can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PluginCategory {
    /// Documents authored for reading: text, fixed-layout, slides.
    Documents,
    /// Tabular and structured data work.
    Data,
    /// Charts, plots, and other rendered visuals.
    Visualization,
    /// Anything the vocabulary above does not describe yet.
    Other,
}

impl PluginCategory {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "documents" => Some(Self::Documents),
            "data" => Some(Self::Data),
            "visualization" => Some(Self::Visualization),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// What a plugin can actually do, from a closed vocabulary.
///
/// A badge is **derived by the host from the plugin's contents** and is never
/// read from a manifest: there is no `capabilities` key, and the parser's
/// closed key set rejects one outright, so a bundle cannot understate what it
/// carries or claim reach it does not have. This is the same honesty invariant
/// the model registry enforces on modality flags.
///
/// Badges have two consumers. A UI shows them on a plugin's detail view, and
/// the permission layer keys install/enable confirmation on the heavier ones —
/// which is what keeps day-to-day skill invocation prompt-free.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
)]
#[serde(rename_all = "kebab-case")]
pub enum PluginCapability {
    /// Produces deliverables in the workspace. Every skill teaches `exec`
    /// work that saves files, so any bundle with a member carries this.
    WriteFiles,
    /// Reaches the network while running. Today that is the sandbox `pip` or
    /// `npm` install a declared package dependency implies.
    Network,
    /// Needs a host-managed install outside the sandbox (a declared
    /// [`crate::HostDep`], e.g. the LibreOffice converter).
    HostInstall,
    /// Drives a live surface — a running document, a browser, a device.
    ///
    /// No component kind derives this yet: it is reserved for the live-control
    /// components the plugin format is meant to grow into, and is listed here
    /// so the vocabulary a UI and the permission layer render against is
    /// closed and complete from the start rather than widening later.
    LiveControl,
    /// Bundles an MCP server, and so whatever that server exposes.
    ///
    /// Derived from the validated `mcp.json` an import retained beside the
    /// manifest — see [`PluginPackage::mcp_servers`] — never from anything the
    /// manifest says.
    Mcp,
}

/// The static sandbox-compatibility conclusion recorded at import time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PluginCompatibilityStatus {
    /// Every declared Python pin is present in the prepared sandbox image and
    /// no member carries helper scripts whose external assumptions are opaque
    /// to the manifest parser.
    Compatible,
    /// The plugin can be installed, but one or more static findings need a
    /// visible disclaimer before a user relies on it.
    Limited,
    /// No importer stamp was present or its metadata was invalid. This is the
    /// honest state for a bundle the user authored or copied by hand.
    Unchecked,
}

/// One reason an imported plugin is not statically sandbox-compatible.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PluginCompatibilityIssue {
    /// A skill declares an exact Python pin that is not in the prepared
    /// sandbox image's hash-checked package closure. It may require runtime
    /// package-manager access instead.
    MissingSandboxDependency { skill: String, dependency: String },
    /// A skill ships helper scripts. Their bytes are staged, but static
    /// manifest inspection cannot prove which binaries, credentials, or
    /// network access they assume.
    ScriptsPresent { skill: String },
}

/// Renderer-safe compatibility disclosure for one plugin.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct PluginCompatibility {
    pub status: PluginCompatibilityStatus,
    pub issues: Vec<PluginCompatibilityIssue>,
}

impl PluginCompatibility {
    #[must_use]
    pub fn compatible() -> Self {
        Self {
            status: PluginCompatibilityStatus::Compatible,
            issues: Vec::new(),
        }
    }

    #[must_use]
    pub fn unchecked() -> Self {
        Self {
            status: PluginCompatibilityStatus::Unchecked,
            issues: Vec::new(),
        }
    }
}

/// The packaging format an imported bundle was read from.
///
/// Recorded so a later reader can tell a bundle converted from a standard
/// Agent Plugins package from one whose author wrote `PLUGIN.md` directly, and
/// which version of that specification the package targeted.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginSourceFormat {
    /// An Tidebreak `PLUGIN.md` bundle, or a bare Agent Skills package wrapped
    /// as one. The default, so stamps written before this field existed keep
    /// reading back as what they are.
    #[default]
    PluginManifest,
    /// A package published in the Agent Plugins format
    /// (<https://agent-plugins.org>), converted at import.
    AgentPlugins { spec_version: String },
}

/// The host-owned metadata persisted beside an imported `PLUGIN.md`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginInstallStamp {
    pub schema_version: u32,
    pub source_url: String,
    pub revision: String,
    /// Absent in stamps written before the importer read standard packages.
    #[serde(default)]
    pub source_format: PluginSourceFormat,
    pub compatibility: PluginCompatibility,
}

/// Compare parsed member declarations with the package closure baked into the
/// prepared sandbox image, and flag opaque helper scripts.
///
/// `has_scripts` is supplied separately from `SkillPackage` because scripts
/// never affect prompt composition and therefore do not belong in that catalog
/// type. Import is the boundary where both facts are available together.
///
/// Only Python pins are compared: the image build's requirements file is the
/// source of truth for what is baked in, and there is no equivalent npm
/// closure to check declared npm pins against yet.
#[must_use]
pub fn assess_plugin_compatibility(skills: &[(&SkillPackage, bool)]) -> PluginCompatibility {
    let mut issues = Vec::new();
    for (skill, has_scripts) in skills {
        for dependency in &skill.python_deps {
            if !SANDBOX_IMAGE_PYTHON_CLOSURE.contains(&normalized_python_pin(dependency)) {
                issues.push(PluginCompatibilityIssue::MissingSandboxDependency {
                    skill: skill.name.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
        if *has_scripts {
            issues.push(PluginCompatibilityIssue::ScriptsPresent {
                skill: skill.name.clone(),
            });
        }
    }
    PluginCompatibility {
        status: if issues.is_empty() {
            PluginCompatibilityStatus::Compatible
        } else {
            PluginCompatibilityStatus::Limited
        },
        issues,
    }
}

/// Exact normalized `package==version` pins present in the documents sandbox
/// image. The requirements file is the image build's source of truth; using it
/// here prevents a second hand-maintained compatibility list from drifting.
static SANDBOX_IMAGE_PYTHON_CLOSURE: LazyLock<BTreeSet<String>> = LazyLock::new(|| {
    include_str!("../../tidebreak-sandbox-agent/documents-requirements.txt")
        .lines()
        .filter_map(|line| {
            let pin = line.split_whitespace().next()?;
            pin.contains("==").then(|| normalized_python_pin(pin))
        })
        .collect()
});

fn normalized_python_pin(pin: &str) -> String {
    let Some((package, version)) = pin.split_once("==") else {
        return pin.to_ascii_lowercase();
    };
    let package = package
        .chars()
        .map(|character| match character {
            '.' | '_' => '-',
            other => other.to_ascii_lowercase(),
        })
        .collect::<String>();
    format!("{package}=={version}")
}

/// The badges `plugin` earns from what it actually contains.
///
/// `members` is any set of loaded skill packages; only those the plugin
/// actually claims are considered, so a caller may pass the whole catalog.
/// The result is deduplicated and sorted, so the same bundle always renders
/// the same badge row.
#[must_use]
pub fn derived_capabilities(
    plugin: &PluginPackage,
    members: &[&SkillPackage],
) -> Vec<PluginCapability> {
    let members: Vec<&SkillPackage> = members
        .iter()
        .copied()
        .filter(|skill| plugin.skills.contains(&skill.name))
        .collect();
    let mut capabilities = BTreeSet::new();
    if !members.is_empty() {
        // A skill is instructions for producing a deliverable through `exec`;
        // one member is enough for the bundle to write files.
        capabilities.insert(PluginCapability::WriteFiles);
    }
    if members
        .iter()
        .any(|skill| !skill.python_deps.is_empty() || !skill.npm_deps.is_empty())
    {
        // Declared package dependencies are installed with `pip` or `npm`
        // inside the sandbox, which is a network reach under the current
        // install flow.
        capabilities.insert(PluginCapability::Network);
    }
    if members.iter().any(|skill| !skill.host_deps.is_empty()) {
        capabilities.insert(PluginCapability::HostInstall);
    }
    if plugin.mcp_servers > 0 {
        // The bundle carries at least one server entry that validated at
        // import. What that server exposes is not knowable from here, which is
        // exactly what the badge is telling the user.
        capabilities.insert(PluginCapability::Mcp);
    }
    capabilities.into_iter().collect()
}

/// Which source a validated plugin package was loaded from.
///
/// Origin is host-derived from the load path, never from manifest content —
/// the closed key set has no `origin` key at all — so a user bundle cannot
/// claim to ship with the app. A management surface uses it to attribute the
/// bundles the user wrote themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PluginOrigin {
    /// Shipped with the application from a trusted resource directory.
    Builtin,
    /// Authored by the user in the per-install plugins directory.
    User,
}

/// Host-derived bundle entry parsed from a plugin manifest's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPackage {
    /// Kebab-case slug; also the directory name.
    pub name: String,
    /// Human-facing name for surfaces that show the bundle.
    pub display_name: String,
    /// One printable line describing what the bundle covers.
    pub description: String,
    /// What kind of work the bundle is for.
    pub category: PluginCategory,
    /// Member skill slugs, in manifest order, each one a loaded skill.
    pub skills: Vec<String>,
    /// Member prompt slugs, in manifest order, each one a loaded prompt.
    /// Empty for a skills-only bundle, which is every bundle we ship.
    pub prompts: Vec<String>,
    /// Optional line emitted above the member skills in the prompt catalog,
    /// telling the model how to choose among them.
    pub router_preamble: Option<String>,
    /// How many MCP servers the bundle's retained configuration declares.
    ///
    /// Host-derived like [`Self::compatibility`]: the loader counts the
    /// entries in the `mcp.json` an import validated and retained, and the
    /// manifest's closed key set has nothing that could state it. Zero for
    /// every bundle that ships no server configuration.
    pub mcp_servers: usize,
    /// Where the package was loaded from.
    pub origin: PluginOrigin,
    /// Static sandbox-compatibility disclosure. Imported user bundles read a
    /// host-owned stamp; hand-authored bundles remain visibly unchecked.
    pub compatibility: PluginCompatibility,
}

/// One validated plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPlugin {
    /// The parsed manifest this plugin's grouping is derived from.
    pub package: PluginPackage,
}

/// Why a plugin manifest was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid plugin manifest: {0}")]
pub struct PluginParseError(String);

fn invalid(reason: impl Into<String>) -> PluginParseError {
    PluginParseError(reason.into())
}

/// Whether `name` is a well-formed plugin slug: bounded kebab-case with no
/// empty segments — the same grammar skill names use.
#[must_use]
pub fn is_valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .split('-')
            .all(|segment| !segment.is_empty() && segment.bytes().all(is_slug_byte))
}

fn is_slug_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// Whether `line` is safe to emit as one bounded prompt or UI line: non-empty,
/// trimmed, within `limit` bytes, and free of control characters so it cannot
/// span lines or smuggle a heading.
fn is_valid_line(line: &str, limit: usize) -> bool {
    !line.is_empty()
        && line.len() <= limit
        && line.trim() == line
        && !line.chars().any(char::is_control)
}

/// Whether `preamble` is safe to emit as the plugin's catalog line.
///
/// Exported so prompt composition can refuse a forged entry independently of
/// the parser, the way it re-checks skill names and descriptions.
#[must_use]
pub fn is_valid_plugin_router_preamble(preamble: &str) -> bool {
    is_valid_line(preamble, MAX_ROUTER_PREAMBLE_BYTES)
}

/// Parse one `PLUGIN.md` source: strict frontmatter between `---` fences.
///
/// Recognized keys are exactly `name`, `display-name`, `description`,
/// `category`, the single-line flow lists `skills: ["a", "b"]` and
/// `prompts: ["c"]`, and the optional `router-preamble`. Either member list
/// may be omitted, but a bundle that claims nothing is rejected. Anything
/// else — an unknown key, a duplicate, a member that is not a well-formed
/// slug, a control character in a rendered line — rejects the whole manifest.
/// The body below the frontmatter is documentation for whoever opens the file;
/// it is never staged and never reaches the model, so it may be empty.
///
/// `origin` is supplied by the caller from the path the manifest was read
/// from. Parsing itself is identical for both origins: a user bundle is held
/// to exactly the built-in manifest grammar, so a plugin that loads here is one
/// the catalog can render whoever wrote it.
pub fn parse_plugin_manifest(
    source: &str,
    origin: PluginOrigin,
) -> Result<PluginPackage, PluginParseError> {
    if source.len() > crate::MAX_WORKSPACE_FILE_BYTES {
        return Err(invalid("manifest exceeds the workspace file limit"));
    }
    let (frontmatter, _body) = crate::skills::split_frontmatter(source).map_err(invalid)?;

    let mut name = None;
    let mut display_name = None;
    let mut description = None;
    let mut category = None;
    let mut skills = None;
    let mut prompts = None;
    let mut router_preamble = None;
    for line in frontmatter.lines() {
        if line.trim().is_empty() {
            return Err(invalid("blank line inside frontmatter"));
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| invalid(format!("frontmatter line without a key: {line:?}")))?;
        let value = value.trim();
        match key {
            "name" => {
                if name.replace(value).is_some() {
                    return Err(invalid("duplicate 'name'"));
                }
            }
            "display-name" => {
                if display_name.replace(value).is_some() {
                    return Err(invalid("duplicate 'display-name'"));
                }
            }
            "description" => {
                if description.replace(value).is_some() {
                    return Err(invalid("duplicate 'description'"));
                }
            }
            "category" => {
                if category.replace(value).is_some() {
                    return Err(invalid("duplicate 'category'"));
                }
            }
            "skills" => {
                let members =
                    parse_members(value, "skills", MAX_MEMBER_SKILLS, is_valid_skill_name)?;
                if skills.replace(members).is_some() {
                    return Err(invalid("duplicate 'skills'"));
                }
            }
            "prompts" => {
                let members =
                    parse_members(value, "prompts", MAX_MEMBER_PROMPTS, is_valid_prompt_name)?;
                if prompts.replace(members).is_some() {
                    return Err(invalid("duplicate 'prompts'"));
                }
            }
            "router-preamble" => {
                if router_preamble.replace(value).is_some() {
                    return Err(invalid("duplicate 'router-preamble'"));
                }
            }
            other => return Err(invalid(format!("unknown frontmatter key {other:?}"))),
        }
    }

    let name = name.ok_or_else(|| invalid("missing 'name'"))?;
    if !is_valid_plugin_name(name) {
        return Err(invalid(format!(
            "'name' is not a kebab-case slug: {name:?}"
        )));
    }
    let display_name = display_name.ok_or_else(|| invalid("missing 'display-name'"))?;
    if !is_valid_line(display_name, MAX_DISPLAY_NAME_BYTES) {
        return Err(invalid("'display-name' is not one bounded printable line"));
    }
    let description = description.ok_or_else(|| invalid("missing 'description'"))?;
    if !is_valid_line(description, MAX_DESCRIPTION_BYTES) {
        return Err(invalid("'description' is not one bounded printable line"));
    }
    let category = category.ok_or_else(|| invalid("missing 'category'"))?;
    let Some(category) = PluginCategory::parse(category) else {
        return Err(invalid(format!("unknown 'category': {category:?}")));
    };
    // Either list alone is a legitimate bundle — a skills-only one is what we
    // ship — but a manifest claiming neither describes nothing.
    let skills = skills.unwrap_or_default();
    let prompts = prompts.unwrap_or_default();
    if skills.is_empty() && prompts.is_empty() {
        return Err(invalid("bundle claims no 'skills' and no 'prompts'"));
    }
    let router_preamble = router_preamble
        .map(|preamble| {
            is_valid_plugin_router_preamble(preamble)
                .then(|| preamble.to_owned())
                .ok_or_else(|| invalid("'router-preamble' is not one bounded printable line"))
        })
        .transpose()?;
    Ok(PluginPackage {
        name: name.to_owned(),
        display_name: display_name.to_owned(),
        description: description.to_owned(),
        category,
        skills,
        prompts,
        router_preamble,
        // Host-derived from the retained configuration, which the loader
        // reads separately; a manifest cannot state it.
        mcp_servers: 0,
        origin,
        compatibility: match origin {
            PluginOrigin::Builtin => PluginCompatibility::compatible(),
            PluginOrigin::User => PluginCompatibility::unchecked(),
        },
    })
}

fn installed_compatibility(directory: &Path, origin: PluginOrigin) -> PluginCompatibility {
    if origin == PluginOrigin::Builtin {
        return PluginCompatibility::compatible();
    }
    let path = directory.join(PLUGIN_INSTALL_STAMP_FILE);
    let regular_file = path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
    if !regular_file {
        return PluginCompatibility::unchecked();
    }
    let bytes = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= MAX_INSTALL_STAMP_BYTES => bytes,
        Ok(_) => {
            tracing::warn!(
                "plugin install stamp {} exceeds the byte limit",
                path.display()
            );
            return PluginCompatibility::unchecked();
        }
        Err(error) => {
            tracing::warn!(
                "plugin install stamp {} is unreadable: {error}",
                path.display()
            );
            return PluginCompatibility::unchecked();
        }
    };
    let stamp: PluginInstallStamp = match serde_json::from_slice(&bytes) {
        Ok(stamp) => stamp,
        Err(error) => {
            tracing::warn!(
                "plugin install stamp {} is invalid: {error}",
                path.display()
            );
            return PluginCompatibility::unchecked();
        }
    };
    if stamp.schema_version != PLUGIN_INSTALL_STAMP_SCHEMA
        || stamp.source_url.is_empty()
        || stamp.source_url.len() > MAX_INSTALL_SOURCE_URL_BYTES
        || stamp.revision.is_empty()
        || stamp.revision.len() > 255
        || !valid_source_format(&stamp.source_format)
        || !valid_compatibility(&stamp.compatibility)
    {
        tracing::warn!(
            "plugin install stamp {} fails the metadata contract",
            path.display()
        );
        return PluginCompatibility::unchecked();
    }
    stamp.compatibility
}

fn valid_source_format(format: &PluginSourceFormat) -> bool {
    match format {
        PluginSourceFormat::PluginManifest => true,
        PluginSourceFormat::AgentPlugins { spec_version } => {
            !spec_version.is_empty()
                && spec_version.len() <= 32
                && spec_version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        }
    }
}

fn valid_compatibility(compatibility: &PluginCompatibility) -> bool {
    if compatibility.issues.len() > MAX_COMPATIBILITY_ISSUES {
        return false;
    }
    (match compatibility.status {
        PluginCompatibilityStatus::Compatible => compatibility.issues.is_empty(),
        PluginCompatibilityStatus::Limited => !compatibility.issues.is_empty(),
        PluginCompatibilityStatus::Unchecked => false,
    }) && compatibility.issues.iter().all(|issue| match issue {
        PluginCompatibilityIssue::MissingSandboxDependency { skill, dependency } => {
            is_valid_skill_name(skill) && crate::skills::is_pinned_python_dep(dependency)
        }
        PluginCompatibilityIssue::ScriptsPresent { skill } => is_valid_skill_name(skill),
    })
}

/// Parse the single-line flow form `["word-documents", "pdf-documents"]`.
///
/// Every item is a double-quoted slug checked by `valid`; the slug grammar
/// admits neither `"` nor `]`, so the list cannot be malformed into something
/// that parses. An empty list is rejected rather than treated as absent: it
/// reads as an intent the manifest does not express.
fn parse_members(
    value: &str,
    key: &str,
    limit: usize,
    valid: fn(&str) -> bool,
) -> Result<Vec<String>, PluginParseError> {
    let malformed = || {
        invalid(format!(
            "'{key}' must be `[\"name\", ...]` with at least one member"
        ))
    };
    let items = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(malformed)?
        .trim();
    if items.is_empty() {
        return Err(malformed());
    }
    let mut members: Vec<String> = Vec::new();
    for item in items.split(',') {
        let member = item
            .trim()
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(malformed)?;
        if !valid(member) {
            return Err(invalid(format!(
                "'{key}' member is not a kebab-case slug: {member:?}"
            )));
        }
        if members.iter().any(|existing| existing == member) {
            return Err(invalid(format!("duplicate '{key}' member: {member:?}")));
        }
        members.push(member.to_owned());
    }
    if members.len() > limit {
        return Err(invalid(format!("'{key}' lists too many members")));
    }
    Ok(members)
}

/// Load every valid plugin under `source`, one directory per plugin, against
/// the skills and prompts that actually loaded, tagging each with `origin`.
///
/// A plugin is skipped with a warning — never half-applied — when its manifest
/// is unreadable or rejected, when the directory name disagrees with the
/// manifest, when a member is not among `skills` or `prompts`, or when a
/// member is already claimed by another plugin. Claims resolve in name order
/// so the same tree always produces the same grouping. Members no plugin
/// claims stay standalone; that is the supported shape for a bare skill or
/// prompt directory, not a degraded one.
#[must_use]
pub fn load_plugins(
    source: &Path,
    skills: &[LoadedSkill],
    prompts: &[LoadedPrompt],
    origin: PluginOrigin,
) -> Vec<LoadedPlugin> {
    let entries = match std::fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                "plugin source directory {} is unreadable: {error}",
                source.display()
            );
            return Vec::new();
        }
    };
    let known_skills: BTreeSet<&str> = skills
        .iter()
        .map(|skill| skill.package.name.as_str())
        .collect();
    let known_prompts: BTreeSet<&str> = prompts
        .iter()
        .map(|prompt| prompt.package.name.as_str())
        .collect();
    let mut parsed: Vec<PluginPackage> = Vec::new();
    for entry in entries.flatten() {
        let Ok(directory_name) = entry.file_name().into_string() else {
            continue;
        };
        let is_directory = entry
            .path()
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if !is_directory {
            continue;
        }
        let manifest_path = entry.path().join(PLUGIN_MANIFEST_FILE);
        let regular_file = manifest_path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        if !regular_file {
            tracing::warn!("skipping plugin '{directory_name}': no regular {PLUGIN_MANIFEST_FILE}");
            continue;
        }
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!("skipping plugin '{directory_name}': manifest unreadable: {error}");
                continue;
            }
        };
        let mut package = match parse_plugin_manifest(&manifest, origin) {
            Ok(package) => package,
            Err(error) => {
                tracing::warn!("skipping plugin '{directory_name}': {error}");
                continue;
            }
        };
        if package.name != directory_name {
            tracing::warn!(
                "skipping plugin '{directory_name}': manifest names itself {:?}",
                package.name
            );
            continue;
        }
        package.compatibility = installed_compatibility(&entry.path(), origin);
        package.mcp_servers = crate::agent_plugins::load_plugin_mcp_config(&entry.path())
            .map_or(0, |config| config.servers.len());
        if let Some(missing) = package
            .skills
            .iter()
            .find(|skill| !known_skills.contains(skill.as_str()))
        {
            tracing::warn!(
                "skipping plugin '{directory_name}': member skill {missing:?} did not load"
            );
            continue;
        }
        if let Some(missing) = package
            .prompts
            .iter()
            .find(|prompt| !known_prompts.contains(prompt.as_str()))
        {
            tracing::warn!(
                "skipping plugin '{directory_name}': member prompt {missing:?} did not load"
            );
            continue;
        }
        parsed.push(package);
    }

    parsed.sort_by(|a, b| a.name.cmp(&b.name));
    let mut claimed_skills: BTreeSet<String> = BTreeSet::new();
    let mut claimed_prompts: BTreeSet<String> = BTreeSet::new();
    let mut plugins = Vec::new();
    for package in parsed {
        if let Some(taken) = package
            .skills
            .iter()
            .find(|skill| claimed_skills.contains(skill.as_str()))
        {
            tracing::warn!(
                "skipping plugin '{}': skill {taken:?} is already claimed by another plugin",
                package.name
            );
            continue;
        }
        if let Some(taken) = package
            .prompts
            .iter()
            .find(|prompt| claimed_prompts.contains(prompt.as_str()))
        {
            tracing::warn!(
                "skipping plugin '{}': prompt {taken:?} is already claimed by another plugin",
                package.name
            );
            continue;
        }
        claimed_skills.extend(package.skills.iter().cloned());
        claimed_prompts.extend(package.prompts.iter().cloned());
        plugins.push(LoadedPlugin { package });
    }
    plugins
}

/// The built-in plugins plus the user-authored bundles under `user_dir`,
/// merged into one deterministic catalog.
///
/// User bundles go through the same strict loader and the same membership
/// checks as built-ins, resolved against `skills` and `prompts` — which should
/// be the *merged* sets, so a user bundle may group the skills the user wrote.
/// Three rules give the built-in tree the floor, each one a skip-with-warning
/// so one bad bundle never takes the catalog down:
///
/// * a built-in name is reserved: a user bundle claiming it is dropped rather
///   than shadowing curated grouping;
/// * a skill a built-in bundle already claims cannot be re-claimed;
/// * neither can a prompt.
///
/// A member that belongs to no built-in bundle is fair game, which is what
/// lets a user bundle group their own skills. A missing user directory is an
/// empty user set, not an error. The result is sorted by name.
#[must_use]
pub fn merged_plugins(
    builtins: &[LoadedPlugin],
    user_dir: Option<&Path>,
    skills: &[LoadedSkill],
    prompts: &[LoadedPrompt],
) -> Vec<LoadedPlugin> {
    let mut plugins = builtins.to_vec();
    let Some(dir) = user_dir.filter(|dir| dir.is_dir()) else {
        return plugins;
    };
    let builtin_skills: BTreeSet<&str> = builtins
        .iter()
        .flat_map(|plugin| plugin.package.skills.iter().map(String::as_str))
        .collect();
    let builtin_prompts: BTreeSet<&str> = builtins
        .iter()
        .flat_map(|plugin| plugin.package.prompts.iter().map(String::as_str))
        .collect();
    for user_plugin in load_plugins(dir, skills, prompts, PluginOrigin::User) {
        let package = &user_plugin.package;
        let name = &package.name;
        if builtins.iter().any(|plugin| plugin.package.name == *name) {
            tracing::warn!("skipping user plugin '{name}': a built-in plugin owns that name");
            continue;
        }
        if let Some(taken) = package
            .skills
            .iter()
            .find(|skill| builtin_skills.contains(skill.as_str()))
        {
            tracing::warn!(
                "skipping user plugin '{name}': skill {taken:?} is claimed by a built-in plugin"
            );
            continue;
        }
        if let Some(taken) = package
            .prompts
            .iter()
            .find(|prompt| builtin_prompts.contains(prompt.as_str()))
        {
            tracing::warn!(
                "skipping user plugin '{name}': prompt {taken:?} is claimed by a built-in plugin"
            );
            continue;
        }
        plugins.push(user_plugin);
    }
    plugins.sort_by(|a, b| a.package.name.cmp(&b.package.name));
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompts::{load_prompts, PromptOrigin, PROMPT_MANIFEST_FILE};
    use crate::skills::{load_skills, SkillOrigin, SKILL_MANIFEST_FILE};
    use crate::HostDep;

    const VALID: &str = "---\n\
name: documents\n\
display-name: Documents\n\
description: Word, PDF, and slide deliverables.\n\
category: documents\n\
skills: [\"word-documents\", \"pdf-documents\"]\n\
router-preamble: Pick by the file the user needs.\n\
---\n\
\n\
# Documents\n";

    fn write_skill(dir: &Path, name: &str) {
        let skill = dir.join(name);
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(
            skill.join(SKILL_MANIFEST_FILE),
            format!("---\nname: {name}\ndescription: Does {name} work.\n---\nBody.\n"),
        )
        .unwrap();
    }

    fn write_prompt(dir: &Path, name: &str) {
        let prompt = dir.join(name);
        std::fs::create_dir(&prompt).unwrap();
        std::fs::write(
            prompt.join(PROMPT_MANIFEST_FILE),
            format!("---\nname: {name}\ndescription: Starts a {name}.\n---\nBody.\n"),
        )
        .unwrap();
    }

    fn write_plugin(dir: &Path, name: &str, manifest: &str) {
        let plugin = dir.join(name);
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join(PLUGIN_MANIFEST_FILE), manifest).unwrap();
    }

    #[test]
    fn valid_manifest_parses_into_its_bundle_entry() {
        let package = parse_plugin_manifest(VALID, PluginOrigin::Builtin).unwrap();
        assert_eq!(package.name, "documents");
        assert_eq!(package.display_name, "Documents");
        assert_eq!(package.category, PluginCategory::Documents);
        assert_eq!(package.skills, ["word-documents", "pdf-documents"]);
        assert_eq!(package.prompts, [""; 0]);
        assert_eq!(
            package.router_preamble.as_deref(),
            Some("Pick by the file the user needs.")
        );

        // The preamble is optional; everything else is required.
        let minimal = "---\nname: charts\ndisplay-name: Charts\ndescription: Plots.\n\
                       category: visualization\nskills: [\"charts\"]\n---\n";
        assert_eq!(
            parse_plugin_manifest(minimal, PluginOrigin::Builtin)
                .unwrap()
                .router_preamble,
            None
        );

        // Either member list alone is a bundle: prompts-only is as valid as
        // the skills-only shape we ship.
        let prompts_only = "---\nname: writing\ndisplay-name: Writing\ndescription: Starters.\n\
                            category: other\nprompts: [\"weekly-update\"]\n---\n";
        let package = parse_plugin_manifest(prompts_only, PluginOrigin::Builtin).unwrap();
        assert_eq!(package.skills, [""; 0]);
        assert_eq!(package.prompts, ["weekly-update"]);
    }

    #[test]
    fn malformed_manifests_are_rejected_not_half_parsed() {
        let head = "---\nname: a\ndisplay-name: A\ndescription: b\ncategory: other\n";
        for (case, source) in [
            ("no frontmatter", "# Just markdown\n".to_owned()),
            ("unclosed frontmatter", format!("{head}skills: [\"c\"]\n")),
            (
                "missing name",
                "---\ndisplay-name: A\ndescription: b\ncategory: other\nskills: [\"c\"]\n---\n"
                    .to_owned(),
            ),
            (
                "missing display-name",
                "---\nname: a\ndescription: b\ncategory: other\nskills: [\"c\"]\n---\n".to_owned(),
            ),
            ("no members at all", format!("{head}---\n")),
            (
                "empty prompts list",
                format!("{head}skills: [\"c\"]\nprompts: []\n---\n"),
            ),
            (
                "non-kebab prompt member",
                format!("{head}prompts: [\"Weekly Update\"]\n---\n"),
            ),
            (
                "unknown category",
                "---\nname: a\ndisplay-name: A\ndescription: b\ncategory: wizardry\nskills: [\"c\"]\n---\n".to_owned(),
            ),
            ("empty skills list", format!("{head}skills: []\n---\n")),
            (
                "non-kebab member",
                format!("{head}skills: [\"Word Documents\"]\n---\n"),
            ),
            (
                "duplicate member",
                format!("{head}skills: [\"c\", \"c\"]\n---\n"),
            ),
            (
                "block-style skills",
                format!("{head}skills:\n  - c\n---\n"),
            ),
            (
                "unknown key",
                format!("{head}skills: [\"c\"]\nversion: 1\n---\n"),
            ),
            (
                "multi-line preamble",
                format!("{head}skills: [\"c\"]\nrouter-preamble: one\\nline\n---\n")
                    .replace("\\n", "\u{0b}"),
            ),
            (
                "oversized preamble",
                format!("{head}skills: [\"c\"]\nrouter-preamble: {}\n---\n", "x".repeat(301)),
            ),
        ] {
            assert!(
                parse_plugin_manifest(&source, PluginOrigin::Builtin).is_err(),
                "{case} should be rejected"
            );
        }
    }

    /// Contract: membership is checked against the skills and prompts that
    /// actually loaded, and a member belongs to at most one plugin — a second
    /// claimant is skipped whole rather than producing overlapping groups.
    /// Prompts are members on exactly those terms, so both rules are asserted
    /// against them too.
    #[test]
    fn loader_rejects_dangling_members_and_double_claims() {
        let skills_dir = tempfile::tempdir().unwrap();
        for name in ["charts", "pdf-documents", "word-documents"] {
            write_skill(skills_dir.path(), name);
        }
        let skills = load_skills(skills_dir.path(), SkillOrigin::Builtin);
        let prompts_dir = tempfile::tempdir().unwrap();
        write_prompt(prompts_dir.path(), "weekly-update");
        let prompts = load_prompts(prompts_dir.path(), PromptOrigin::Builtin);

        let plugins_dir = tempfile::tempdir().unwrap();
        write_plugin(
            plugins_dir.path(),
            "documents",
            &VALID.replace(
                "router-preamble:",
                "prompts: [\"weekly-update\"]\nrouter-preamble:",
            ),
        );
        // Claims a skill 'documents' already owns: skipped entirely, so its
        // uncontested member stays standalone rather than half-grouped.
        write_plugin(
            plugins_dir.path(),
            "zzz-later",
            "---\nname: zzz-later\ndisplay-name: Later\ndescription: Steals a member.\n\
             category: other\nskills: [\"charts\", \"pdf-documents\"]\n---\n",
        );
        // Claims a prompt 'documents' already owns.
        write_plugin(
            plugins_dir.path(),
            "zzz-writing",
            "---\nname: zzz-writing\ndisplay-name: Writing\ndescription: Steals a prompt.\n\
             category: other\nprompts: [\"weekly-update\"]\n---\n",
        );
        // Names a skill that did not load.
        write_plugin(
            plugins_dir.path(),
            "ghosts",
            "---\nname: ghosts\ndisplay-name: Ghosts\ndescription: Dangling member.\n\
             category: other\nskills: [\"spreadsheets\"]\n---\n",
        );
        // Names a prompt that did not load.
        write_plugin(
            plugins_dir.path(),
            "phantoms",
            "---\nname: phantoms\ndisplay-name: Phantoms\ndescription: Dangling prompt.\n\
             category: other\nprompts: [\"standup\"]\n---\n",
        );
        // Directory disagrees with the manifest.
        write_plugin(plugins_dir.path(), "mislabeled", VALID);

        let plugins = load_plugins(plugins_dir.path(), &skills, &prompts, PluginOrigin::Builtin);
        assert_eq!(
            plugins
                .iter()
                .map(|plugin| plugin.package.name.as_str())
                .collect::<Vec<_>>(),
            ["documents"]
        );
        assert_eq!(
            plugins[0].package.skills,
            ["word-documents", "pdf-documents"]
        );
        assert_eq!(plugins[0].package.prompts, ["weekly-update"]);
    }

    /// Contract: the built-in tree has the floor when user-authored bundles
    /// merge in. Every rule here is a skip-with-warning that is easy to reverse
    /// by accident — a shadowed name or a re-claimed member would silently
    /// re-group curated skills — and origin is host-derived from the load path,
    /// which is what a management surface attributes the user's own bundles by.
    #[test]
    fn user_plugins_merge_behind_the_builtin_tree() {
        let skills_dir = tempfile::tempdir().unwrap();
        for name in ["charts", "word-documents", "pdf-documents"] {
            write_skill(skills_dir.path(), name);
        }
        let user_skills_dir = tempfile::tempdir().unwrap();
        write_skill(user_skills_dir.path(), "meeting-notes");
        let mut skills = load_skills(skills_dir.path(), SkillOrigin::Builtin);
        skills.extend(load_skills(user_skills_dir.path(), SkillOrigin::User));

        let builtin_dir = tempfile::tempdir().unwrap();
        write_plugin(builtin_dir.path(), "documents", VALID);
        let builtins = load_plugins(builtin_dir.path(), &skills, &[], PluginOrigin::Builtin);

        let user_dir = tempfile::tempdir().unwrap();
        // Groups the user's own skill: the supported shape, and the only one
        // that survives here.
        write_plugin(
            user_dir.path(),
            "notes",
            "---\nname: notes\ndisplay-name: My notes\ndescription: How I take notes.\n\
             category: other\nskills: [\"meeting-notes\"]\n---\n",
        );
        // Reserved: a built-in bundle owns this name.
        write_plugin(
            user_dir.path(),
            "documents",
            "---\nname: documents\ndisplay-name: Mine\ndescription: Shadows the built-in.\n\
             category: other\nskills: [\"charts\"]\n---\n",
        );
        // Re-claims a skill the built-in 'documents' bundle already owns.
        write_plugin(
            user_dir.path(),
            "poaching",
            "---\nname: poaching\ndisplay-name: Poaching\ndescription: Steals a member.\n\
             category: other\nskills: [\"pdf-documents\"]\n---\n",
        );
        // Invalid manifests never take the catalog down with them.
        write_plugin(user_dir.path(), "broken", "not frontmatter at all\n");

        let merged = merged_plugins(&builtins, Some(user_dir.path()), &skills, &[]);
        assert_eq!(
            merged
                .iter()
                .map(|plugin| (plugin.package.name.as_str(), plugin.package.origin))
                .collect::<Vec<_>>(),
            [
                ("documents", PluginOrigin::Builtin),
                ("notes", PluginOrigin::User),
            ]
        );
        // The built-in bundle kept both of its members.
        assert_eq!(
            merged[0].package.skills,
            ["word-documents", "pdf-documents"]
        );

        // A missing user directory is an empty user set, not an error.
        let missing = user_dir.path().join("does-not-exist");
        assert_eq!(
            merged_plugins(&builtins, Some(&missing), &skills, &[]).len(),
            1
        );
    }

    /// Contract: a badge row is a function of what the member skills declare,
    /// and a manifest cannot state one — the closed key set is what enforces
    /// that, so the rejection is asserted here beside the derivation it
    /// protects.
    #[test]
    fn capabilities_derive_from_member_deps_and_never_from_the_manifest() {
        let skill = |name: &str, python: &[&str], npm: &[&str], host: &[HostDep]| SkillPackage {
            name: name.to_owned(),
            description: "Does work.".to_owned(),
            python_deps: python.iter().map(|dep| (*dep).to_owned()).collect(),
            npm_deps: npm.iter().map(|dep| (*dep).to_owned()).collect(),
            host_deps: host.to_vec(),
            origin: SkillOrigin::Builtin,
        };
        let plain = skill("plain", &[], &[], &[]);
        let pip = skill("pip", &["python-docx==1.2.0"], &[], &[]);
        let npm = skill("npm", &[], &["mermaid@11.4.1"], &[]);
        let hosted = skill("hosted", &[], &[], &[HostDep::LibreOffice]);
        let both = skill("both", &[], &["pptxgenjs@4.0.1"], &[HostDep::LibreOffice]);
        // Not a member of any plugin under test: a caller may pass the whole
        // catalog, and a non-member must never contribute a badge.
        let outsider = skill("outsider", &["numpy==2.3.4"], &[], &[HostDep::LibreOffice]);

        for (case, members, expected) in [
            ("no members", vec![], vec![]),
            (
                "instructions only",
                vec![&plain],
                vec![PluginCapability::WriteFiles],
            ),
            (
                "python deps imply the pip install's network reach",
                vec![&pip],
                vec![PluginCapability::WriteFiles, PluginCapability::Network],
            ),
            (
                "npm deps imply the same package-manager network reach",
                vec![&npm],
                vec![PluginCapability::WriteFiles, PluginCapability::Network],
            ),
            (
                "host deps imply a managed install outside the sandbox",
                vec![&hosted],
                vec![PluginCapability::WriteFiles, PluginCapability::HostInstall],
            ),
            (
                "one member is enough for each badge",
                vec![&plain, &both],
                vec![
                    PluginCapability::WriteFiles,
                    PluginCapability::Network,
                    PluginCapability::HostInstall,
                ],
            ),
        ] {
            let plugin = PluginPackage {
                name: "bundle".to_owned(),
                display_name: "Bundle".to_owned(),
                description: "A bundle.".to_owned(),
                category: PluginCategory::Other,
                skills: members.iter().map(|skill| skill.name.clone()).collect(),
                prompts: Vec::new(),
                router_preamble: None,
                mcp_servers: 0,
                origin: PluginOrigin::Builtin,
                compatibility: PluginCompatibility::compatible(),
            };
            let mut passed: Vec<&SkillPackage> = members;
            passed.push(&outsider);
            let mut expected = expected;
            expected.sort_unstable();
            assert_eq!(
                derived_capabilities(&plugin, &passed),
                expected,
                "{case} should derive exactly these badges"
            );
        }

        // `mcp` comes from the server configuration an import validated and
        // retained, counted by the loader — not from anything the manifest
        // says. `live-control` still has no deriving source at all.
        let mut everything = PluginPackage {
            name: "bundle".to_owned(),
            display_name: "Bundle".to_owned(),
            description: "A bundle.".to_owned(),
            category: PluginCategory::Other,
            skills: vec!["both".to_owned()],
            prompts: Vec::new(),
            router_preamble: None,
            mcp_servers: 0,
            origin: PluginOrigin::Builtin,
            compatibility: PluginCompatibility::compatible(),
        };
        let derived = derived_capabilities(&everything, &[&both]);
        assert!(!derived.contains(&PluginCapability::LiveControl));
        assert!(!derived.contains(&PluginCapability::Mcp));
        everything.mcp_servers = 1;
        assert!(derived_capabilities(&everything, &[&both]).contains(&PluginCapability::Mcp));

        // A manifest that tries to declare its own badges is rejected whole.
        assert!(parse_plugin_manifest(
            "---\nname: a\ndisplay-name: A\ndescription: b\ncategory: other\n\
             skills: [\"c\"]\ncapabilities: [\"network\"]\n---\n",
            PluginOrigin::Builtin,
        )
        .is_err());
    }

    /// Contract: every plugin shipped in the repository's `plugins/` tree must
    /// parse against the bundled skills, and the three of them must cover the
    /// curated document set exactly — a dropped plugin or a renamed member
    /// would otherwise show up only as an ungrouped catalog.
    #[test]
    fn bundled_plugin_sources_cover_every_bundled_skill() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let skills = load_skills(&root.join("skills"), SkillOrigin::Builtin);
        let source = root.join("plugins");
        let directories = std::fs::read_dir(&source)
            .expect("bundled plugins directory exists")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count();
        let plugins = load_plugins(&source, &skills, &[], PluginOrigin::Builtin);
        assert_eq!(
            plugins.len(),
            directories,
            "a bundled plugin failed strict loading"
        );
        assert_eq!(
            plugins
                .iter()
                .map(|plugin| plugin.package.name.as_str())
                .collect::<Vec<_>>(),
            ["charts", "documents", "spreadsheets"]
        );
        let mut covered: Vec<&str> = plugins
            .iter()
            .flat_map(|plugin| plugin.package.skills.iter().map(String::as_str))
            .collect();
        covered.sort_unstable();
        assert_eq!(
            covered,
            skills
                .iter()
                .map(|skill| skill.package.name.as_str())
                .collect::<Vec<_>>(),
            "every bundled skill must belong to exactly one bundled plugin"
        );
        // The document bundle is the one whose members are alternatives for
        // the same intent, so its routing line is load-bearing.
        let documents = plugins
            .iter()
            .find(|plugin| plugin.package.name == "documents")
            .unwrap();
        assert!(documents.package.router_preamble.is_some());
    }
}
