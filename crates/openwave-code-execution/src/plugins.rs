//! Plugins: thin manifests that group skills into one installable bundle.
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

use std::collections::BTreeSet;
use std::path::Path;

use crate::skills::{is_valid_skill_name, LoadedSkill};

/// The manifest file every plugin package is defined by.
pub const PLUGIN_MANIFEST_FILE: &str = "PLUGIN.md";

const MAX_NAME_BYTES: usize = 64;
const MAX_DISPLAY_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 200;
const MAX_ROUTER_PREAMBLE_BYTES: usize = 300;
const MAX_MEMBER_SKILLS: usize = 16;

/// What kind of work a plugin bundles, from a closed vocabulary.
///
/// Closed on purpose, like [`crate::HostDep`]: an unknown value rejects the
/// manifest instead of parsing into a string no grouping or badge can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn parse(value: &str) -> Option<Self> {
        match value {
            "documents" => Some(Self::Documents),
            "data" => Some(Self::Data),
            "visualization" => Some(Self::Visualization),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
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
    /// Optional line emitted above the member skills in the prompt catalog,
    /// telling the model how to choose among them.
    pub router_preamble: Option<String>,
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
/// `category`, the single-line flow list `skills: ["a", "b"]`, and the
/// optional `router-preamble`. Anything else — an unknown key, a duplicate, a
/// member that is not a well-formed skill slug, a control character in a
/// rendered line — rejects the whole manifest. The body below the frontmatter
/// is documentation for whoever opens the file; it is never staged and never
/// reaches the model, so it may be empty.
pub fn parse_plugin_manifest(source: &str) -> Result<PluginPackage, PluginParseError> {
    if source.len() > crate::MAX_WORKSPACE_FILE_BYTES {
        return Err(invalid("manifest exceeds the workspace file limit"));
    }
    let rest = source
        .strip_prefix("---\n")
        .ok_or_else(|| invalid("missing opening frontmatter fence"))?;
    let (frontmatter, _body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| invalid("missing closing frontmatter fence"))?;

    let mut name = None;
    let mut display_name = None;
    let mut description = None;
    let mut category = None;
    let mut skills = None;
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
                if skills.replace(parse_member_skills(value)?).is_some() {
                    return Err(invalid("duplicate 'skills'"));
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
    let skills = skills.ok_or_else(|| invalid("missing 'skills'"))?;
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
        router_preamble,
    })
}

/// Parse the single-line flow form `["word-documents", "pdf-documents"]`.
///
/// Every item is a double-quoted skill slug; the slug grammar admits neither
/// `"` nor `]`, so the list cannot be malformed into something that parses.
fn parse_member_skills(value: &str) -> Result<Vec<String>, PluginParseError> {
    let malformed = || invalid("'skills' must be `[\"skill-name\", ...]` with at least one member");
    let items = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(malformed)?
        .trim();
    if items.is_empty() {
        return Err(malformed());
    }
    let mut skills: Vec<String> = Vec::new();
    for item in items.split(',') {
        let skill = item
            .trim()
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(malformed)?;
        if !is_valid_skill_name(skill) {
            return Err(invalid(format!(
                "'skills' member is not a kebab-case slug: {skill:?}"
            )));
        }
        if skills.iter().any(|existing| existing == skill) {
            return Err(invalid(format!("duplicate 'skills' member: {skill:?}")));
        }
        skills.push(skill.to_owned());
    }
    if skills.len() > MAX_MEMBER_SKILLS {
        return Err(invalid("'skills' lists too many members"));
    }
    Ok(skills)
}

/// Load every valid plugin under `source`, one directory per plugin, against
/// the skills that actually loaded.
///
/// A plugin is skipped with a warning — never half-applied — when its manifest
/// is unreadable or rejected, when the directory name disagrees with the
/// manifest, when a member skill is not among `skills`, or when a member is
/// already claimed by another plugin. Claims resolve in name order so the same
/// tree always produces the same grouping. Skills no plugin claims stay
/// standalone; that is the supported shape for a bare skill directory, not a
/// degraded one.
#[must_use]
pub fn load_plugins(source: &Path, skills: &[LoadedSkill]) -> Vec<LoadedPlugin> {
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
    let known: BTreeSet<&str> = skills
        .iter()
        .map(|skill| skill.package.name.as_str())
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
        let package = match parse_plugin_manifest(&manifest) {
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
        if let Some(missing) = package
            .skills
            .iter()
            .find(|skill| !known.contains(skill.as_str()))
        {
            tracing::warn!(
                "skipping plugin '{directory_name}': member skill {missing:?} did not load"
            );
            continue;
        }
        parsed.push(package);
    }

    parsed.sort_by(|a, b| a.name.cmp(&b.name));
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    let mut plugins = Vec::new();
    for package in parsed {
        if let Some(taken) = package
            .skills
            .iter()
            .find(|skill| claimed.contains(skill.as_str()))
        {
            tracing::warn!(
                "skipping plugin '{}': skill {taken:?} is already claimed by another plugin",
                package.name
            );
            continue;
        }
        claimed.extend(package.skills.iter().cloned());
        plugins.push(LoadedPlugin { package });
    }
    plugins
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{load_skills, SkillOrigin, SKILL_MANIFEST_FILE};

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

    fn write_plugin(dir: &Path, name: &str, manifest: &str) {
        let plugin = dir.join(name);
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(plugin.join(PLUGIN_MANIFEST_FILE), manifest).unwrap();
    }

    #[test]
    fn valid_manifest_parses_into_its_bundle_entry() {
        let package = parse_plugin_manifest(VALID).unwrap();
        assert_eq!(package.name, "documents");
        assert_eq!(package.display_name, "Documents");
        assert_eq!(package.category, PluginCategory::Documents);
        assert_eq!(package.skills, ["word-documents", "pdf-documents"]);
        assert_eq!(
            package.router_preamble.as_deref(),
            Some("Pick by the file the user needs.")
        );

        // The preamble is optional; everything else is required.
        let minimal = "---\nname: charts\ndisplay-name: Charts\ndescription: Plots.\n\
                       category: visualization\nskills: [\"charts\"]\n---\n";
        assert_eq!(
            parse_plugin_manifest(minimal).unwrap().router_preamble,
            None
        );
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
            ("missing skills", format!("{head}---\n")),
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
                parse_plugin_manifest(&source).is_err(),
                "{case} should be rejected"
            );
        }
    }

    /// Contract: membership is checked against the skills that actually
    /// loaded, and one skill belongs to at most one plugin — a second
    /// claimant is skipped whole rather than producing overlapping groups.
    #[test]
    fn loader_rejects_dangling_members_and_double_claims() {
        let skills_dir = tempfile::tempdir().unwrap();
        for name in ["charts", "pdf-documents", "word-documents"] {
            write_skill(skills_dir.path(), name);
        }
        let skills = load_skills(skills_dir.path(), SkillOrigin::Builtin);

        let plugins_dir = tempfile::tempdir().unwrap();
        write_plugin(plugins_dir.path(), "documents", VALID);
        // Claims a skill 'documents' already owns: skipped entirely, so its
        // uncontested member stays standalone rather than half-grouped.
        write_plugin(
            plugins_dir.path(),
            "zzz-later",
            "---\nname: zzz-later\ndisplay-name: Later\ndescription: Steals a member.\n\
             category: other\nskills: [\"charts\", \"pdf-documents\"]\n---\n",
        );
        // Names a skill that did not load.
        write_plugin(
            plugins_dir.path(),
            "ghosts",
            "---\nname: ghosts\ndisplay-name: Ghosts\ndescription: Dangling member.\n\
             category: other\nskills: [\"spreadsheets\"]\n---\n",
        );
        // Directory disagrees with the manifest.
        write_plugin(plugins_dir.path(), "mislabeled", VALID);

        let plugins = load_plugins(plugins_dir.path(), &skills);
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
        let plugins = load_plugins(&source, &skills);
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
