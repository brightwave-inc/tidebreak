//! Built-in skill packages staged into exec workspaces.
//!
//! A skill is a directory whose `SKILL.md` teaches the model how to produce
//! one kind of document through `exec`: which pinned libraries to install,
//! the conventions for saving deliverables, and the quality checks to run
//! before declaring the work done. The host stages each skill file into
//! `<scratch>/.openwave/skills/<name>/SKILL.md` before a command runs and
//! advertises only the parsed (name, description) catalog in the operating
//! prompt; the instruction body reaches the model exclusively through
//! `read_file`, never through prompt composition.
//!
//! Parsing is deliberately strict. A malformed skill is skipped with a
//! host-side warning instead of shipping a half-understood package: the
//! catalog line and the staged file must come from the same successfully
//! validated manifest.

use std::path::Path;

/// Stable workspace-relative directory that staged skills are installed under.
pub const SKILLS_DIR: &str = ".openwave/skills";

/// The manifest file every skill package is defined by.
pub const SKILL_MANIFEST_FILE: &str = "SKILL.md";

const MAX_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 200;
const MAX_PYTHON_DEPS: usize = 8;
const MAX_DEP_BYTES: usize = 100;

/// Host-derived catalog entry parsed from a skill manifest's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPackage {
    /// Kebab-case slug; also the staged directory name.
    pub name: String,
    /// One printable line for the operating-prompt catalog.
    pub description: String,
    /// Exactly pinned `package==version` Python requirements.
    pub python_deps: Vec<String>,
}

/// One validated built-in skill: its catalog entry plus the exact manifest
/// bytes to stage into workspaces.
#[derive(Debug, Clone)]
pub struct BuiltinSkill {
    /// The parsed frontmatter the prompt catalog is built from.
    pub package: SkillPackage,
    /// The full `SKILL.md` source, staged verbatim.
    pub manifest: String,
}

/// Why a skill manifest was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid skill manifest: {0}")]
pub struct SkillParseError(String);

fn invalid(reason: impl Into<String>) -> SkillParseError {
    SkillParseError(reason.into())
}

/// Whether `name` is a well-formed skill slug: bounded kebab-case with no
/// empty segments. This is the same check the parser enforces, exported so
/// prompt composition can refuse a forged entry independently.
#[must_use]
pub fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .split('-')
            .all(|segment| !segment.is_empty() && segment.bytes().all(is_slug_byte))
}

fn is_slug_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// Whether `description` is safe to emit as one prompt catalog line: bounded,
/// non-empty, and free of control characters (so it cannot span lines or
/// smuggle a heading).
#[must_use]
pub fn is_valid_skill_description(description: &str) -> bool {
    !description.is_empty()
        && description.len() <= MAX_DESCRIPTION_BYTES
        && description.trim() == description
        && !description.chars().any(char::is_control)
}

pub(crate) fn is_pinned_python_dep(dep: &str) -> bool {
    if dep.is_empty() || dep.len() > MAX_DEP_BYTES {
        return false;
    }
    let Some((package, version)) = dep.split_once("==") else {
        return false;
    };
    !package.is_empty()
        && !version.is_empty()
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

/// Parse one `SKILL.md` source: strict frontmatter between `---` fences,
/// then a non-empty markdown instruction body.
///
/// Recognized frontmatter keys are exactly `name`, `description`, and the
/// optional single-line `deps: { python: ["package==version", ...] }`.
/// Anything else — unknown keys, duplicates, an unpinned dependency, a
/// control character in the description — rejects the whole manifest.
pub fn parse_skill_manifest(source: &str) -> Result<SkillPackage, SkillParseError> {
    if source.len() > crate::MAX_WORKSPACE_FILE_BYTES {
        return Err(invalid("manifest exceeds the workspace file limit"));
    }
    let rest = source
        .strip_prefix("---\n")
        .ok_or_else(|| invalid("missing opening frontmatter fence"))?;
    let (frontmatter, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| invalid("missing closing frontmatter fence"))?;

    let mut name = None;
    let mut description = None;
    let mut python_deps = None;
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
            "description" => {
                if description.replace(value).is_some() {
                    return Err(invalid("duplicate 'description'"));
                }
            }
            "deps" => {
                if python_deps.replace(parse_python_deps(value)?).is_some() {
                    return Err(invalid("duplicate 'deps'"));
                }
            }
            other => return Err(invalid(format!("unknown frontmatter key {other:?}"))),
        }
    }

    let name = name.ok_or_else(|| invalid("missing 'name'"))?;
    if !is_valid_skill_name(name) {
        return Err(invalid(format!(
            "'name' is not a kebab-case slug: {name:?}"
        )));
    }
    let description = description.ok_or_else(|| invalid("missing 'description'"))?;
    if !is_valid_skill_description(description) {
        return Err(invalid("'description' is not one bounded printable line"));
    }
    if body.trim().is_empty() {
        return Err(invalid("empty instruction body"));
    }
    Ok(SkillPackage {
        name: name.to_owned(),
        description: description.to_owned(),
        python_deps: python_deps.unwrap_or_default(),
    })
}

/// Parse the single-line flow form `{ python: ["a==1", "b==2"] }`.
fn parse_python_deps(value: &str) -> Result<Vec<String>, SkillParseError> {
    let malformed = || invalid("'deps' must be `{ python: [\"package==version\", ...] }`");
    let inner = value
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .ok_or_else(malformed)?
        .trim();
    let list = inner.strip_prefix("python:").ok_or_else(malformed)?.trim();
    let items = list
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or_else(malformed)?
        .trim();
    if items.is_empty() {
        return Err(invalid("'deps' lists no packages"));
    }
    let mut deps = Vec::new();
    for item in items.split(',') {
        let dep = item
            .trim()
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(malformed)?;
        if !is_pinned_python_dep(dep) {
            return Err(invalid(format!(
                "dependency is not exactly pinned as package==version: {dep:?}"
            )));
        }
        deps.push(dep.to_owned());
    }
    if deps.len() > MAX_PYTHON_DEPS {
        return Err(invalid("'deps' lists too many packages"));
    }
    Ok(deps)
}

/// Load every valid skill package under `source`, one directory per skill.
///
/// A malformed package — an unreadable or oversized manifest, frontmatter the
/// strict parser rejects, or a directory whose name disagrees with its
/// manifest — is skipped with a warning so one bad file can never break the
/// prompt or fail an exec. The result is sorted by name for deterministic
/// staging and catalog order.
#[must_use]
pub fn load_builtin_skills(source: &Path) -> Vec<BuiltinSkill> {
    let mut skills = Vec::new();
    let entries = match std::fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                "skill source directory {} is unreadable: {error}",
                source.display()
            );
            return skills;
        }
    };
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
        let manifest_path = entry.path().join(SKILL_MANIFEST_FILE);
        let regular_file = manifest_path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        if !regular_file {
            tracing::warn!("skipping skill '{directory_name}': no regular {SKILL_MANIFEST_FILE}");
            continue;
        }
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!("skipping skill '{directory_name}': manifest unreadable: {error}");
                continue;
            }
        };
        let package = match parse_skill_manifest(&manifest) {
            Ok(package) => package,
            Err(error) => {
                tracing::warn!("skipping skill '{directory_name}': {error}");
                continue;
            }
        };
        if package.name != directory_name {
            tracing::warn!(
                "skipping skill '{directory_name}': manifest names itself {:?}",
                package.name
            );
            continue;
        }
        skills.push(BuiltinSkill { package, manifest });
    }
    skills.sort_by(|a, b| a.package.name.cmp(&b.package.name));
    skills
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "---\n\
name: pdf-documents\n\
description: Create, merge, split, and fill PDF documents.\n\
deps: { python: [\"fpdf2==2.8.3\", \"pypdf==5.1.0\"] }\n\
---\n\
\n\
# PDF documents\n\
Instructions live here.\n";

    #[test]
    fn valid_manifest_parses_into_its_catalog_entry() {
        let package = parse_skill_manifest(VALID).unwrap();
        assert_eq!(package.name, "pdf-documents");
        assert_eq!(
            package.description,
            "Create, merge, split, and fill PDF documents."
        );
        assert_eq!(package.python_deps, ["fpdf2==2.8.3", "pypdf==5.1.0"]);

        let no_deps = "---\nname: charts\ndescription: Plots.\n---\nBody.\n";
        assert_eq!(parse_skill_manifest(no_deps).unwrap().python_deps, [""; 0]);
    }

    #[test]
    fn malformed_manifests_are_rejected_not_half_parsed() {
        for (case, source) in [
            ("no frontmatter", "# Just markdown\n"),
            ("unclosed frontmatter", "---\nname: a\ndescription: b\n"),
            ("missing name", "---\ndescription: b\n---\nBody.\n"),
            ("missing description", "---\nname: a\n---\nBody.\n"),
            (
                "non-kebab name",
                "---\nname: PDF Documents\ndescription: b\n---\nBody.\n",
            ),
            (
                "unknown key",
                "---\nname: a\ndescription: b\nauthor: c\n---\nBody.\n",
            ),
            (
                "duplicate key",
                "---\nname: a\nname: c\ndescription: b\n---\nBody.\n",
            ),
            (
                "unpinned dependency",
                "---\nname: a\ndescription: b\ndeps: { python: [\"fpdf2>=2\"] }\n---\nBody.\n",
            ),
            (
                "block-style deps",
                "---\nname: a\ndescription: b\ndeps:\n---\nBody.\n",
            ),
            ("empty body", "---\nname: a\ndescription: b\n---\n  \n"),
        ] {
            assert!(
                parse_skill_manifest(source).is_err(),
                "{case} should be rejected"
            );
        }
    }

    #[test]
    fn loader_skips_malformed_packages_and_keeps_valid_ones() {
        let source = tempfile::tempdir().unwrap();
        let good = source.path().join("pdf-documents");
        std::fs::create_dir(&good).unwrap();
        std::fs::write(good.join(SKILL_MANIFEST_FILE), VALID).unwrap();
        // Parses, but the directory disagrees with the manifest name.
        let renamed = source.path().join("mislabeled");
        std::fs::create_dir(&renamed).unwrap();
        std::fs::write(renamed.join(SKILL_MANIFEST_FILE), VALID).unwrap();
        let broken = source.path().join("broken");
        std::fs::create_dir(&broken).unwrap();
        std::fs::write(broken.join(SKILL_MANIFEST_FILE), "no frontmatter").unwrap();
        std::fs::write(source.path().join("stray-file"), "not a package").unwrap();

        let skills = load_builtin_skills(source.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].package.name, "pdf-documents");
        assert_eq!(skills[0].manifest, VALID);
    }

    /// Contract: every skill shipped in the repository's `skills/` tree must
    /// pass the strict parser and match its directory, or it would silently
    /// drop out of the staged catalog.
    #[test]
    fn bundled_skill_sources_all_parse() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills");
        let directories = std::fs::read_dir(&source)
            .expect("bundled skills directory exists")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count();
        let skills = load_builtin_skills(&source);
        assert_eq!(
            skills.len(),
            directories,
            "a bundled skill failed strict parsing"
        );
        // The curated document set. A missing name means a skill silently
        // dropped out of the staged catalog.
        assert_eq!(
            skills
                .iter()
                .map(|skill| skill.package.name.as_str())
                .collect::<Vec<_>>(),
            [
                "charts",
                "pdf-documents",
                "presentations",
                "spreadsheets",
                "word-documents",
            ]
        );
        for skill in &skills {
            assert!(
                skill
                    .package
                    .python_deps
                    .iter()
                    .all(|dep| dep.contains("==")),
                "bundled skill {} must pin its dependencies exactly",
                skill.package.name
            );
        }
    }
}
