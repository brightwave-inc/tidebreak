//! Skill packages staged into exec workspaces.
//!
//! A skill is a directory whose `SKILL.md` teaches the model how to do one
//! kind of work through `exec`: which pinned libraries to install, the
//! conventions for saving deliverables, and the quality checks to run
//! before declaring the work done. The directory may also carry a `scripts/`
//! subdirectory of helper files. The host stages each skill into
//! `<scratch>/.tidebreak/skills/<name>/` before a command runs and
//! advertises only the parsed (name, description) catalog in the operating
//! prompt; the instruction body reaches the model exclusively through
//! `read_file`, never through prompt composition, and script bytes never
//! reach it at all.
//!
//! Skills come from two sources: the built-in packages shipped with the
//! application, and user-authored packages the host loads from a per-install
//! directory. Both go through the same parser, and a name collision
//! resolves in the built-in's favor so a user package can never shadow
//! curated instructions.
//!
//! Parsing is strict about everything that reaches the prompt or drives host
//! behavior, and tolerant only of unknown frontmatter keys in a user package —
//! the open Agent Skills format carries fields we have no use for. A malformed
//! skill is skipped with a host-side warning instead of shipping a
//! half-understood package: the catalog line and the staged files must come
//! from the same successfully validated manifest.

use std::path::Path;

/// Stable workspace-relative directory that staged skills are installed under.
pub const SKILLS_DIR: &str = ".tidebreak/skills";

/// The manifest file every skill package is defined by.
pub const SKILL_MANIFEST_FILE: &str = "SKILL.md";

/// The optional subdirectory of helper files staged beside the manifest.
pub const SKILL_SCRIPTS_DIR: &str = "scripts";

const MAX_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 200;
const MAX_DEPS_PER_LIST: usize = 8;
const MAX_DEP_BYTES: usize = 100;
const MAX_SKILL_SCRIPTS: usize = 16;

/// A host-provided tool a skill depends on, from a closed vocabulary.
///
/// Unlike language-package deps — free-form pins the sandbox installs — a host
/// dep names a capability only the host can provide (a managed install outside
/// the sandbox). The vocabulary is closed on purpose: an unknown value rejects
/// the whole manifest rather than parsing into a string nothing can act on.
///
/// Not every variant is declarable. [`HostDep::parse`] is the manifest surface
/// and maps only what a skill author is expected to write; the rest are
/// host-derived from other parts of the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostDep {
    /// LibreOffice, for converting office documents to renderable PDFs.
    LibreOffice,
    /// A Node.js runtime, for skills whose npm packages need an interpreter.
    ///
    /// Deliberately not parseable from a manifest: a skill declares the npm
    /// packages it uses, and needing Node to run them follows from that. A
    /// spellable `"node"` would let one manifest ask for the runtime without
    /// the packages and another need it without saying so, and the two
    /// statements could disagree.
    Node,
}

impl HostDep {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "libreoffice" => Some(Self::LibreOffice),
            _ => None,
        }
    }
}

/// Which source a validated skill package was loaded from.
///
/// Origin is host-derived from the load path, never from manifest content, so
/// a user package cannot claim to be built-in. The prompt catalog uses it to
/// attribute user-authored entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum SkillOrigin {
    /// Shipped with the application from a trusted resource directory.
    Builtin,
    /// Authored by the user in the per-install skills directory.
    User,
}

/// Host-derived catalog entry parsed from a skill manifest's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPackage {
    /// Kebab-case slug; also the staged directory name.
    pub name: String,
    /// One printable line for the operating-prompt catalog.
    pub description: String,
    /// Exactly pinned `package==version` Python requirements.
    pub python_deps: Vec<String>,
    /// Exactly pinned `package@version` npm requirements, scoped names
    /// included.
    pub npm_deps: Vec<String>,
    /// Host-provided tools the skill's instructions depend on.
    pub host_deps: Vec<HostDep>,
    /// Where the package was loaded from.
    pub origin: SkillOrigin,
}

/// One validated skill: its catalog entry plus the exact manifest bytes to
/// stage into workspaces.
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    /// The parsed frontmatter the prompt catalog is built from.
    pub package: SkillPackage,
    /// The full `SKILL.md` source, staged verbatim.
    pub manifest: String,
    /// Helper files from the package's `scripts/` directory, staged verbatim
    /// beside the manifest. Their bytes never enter prompt composition.
    pub scripts: Vec<SkillScript>,
}

/// One helper file staged into a skill's `scripts/` directory.
#[derive(Debug, Clone)]
pub struct SkillScript {
    /// The file's own name; always a single path component.
    pub name: String,
    /// The file's bytes, staged verbatim. Scripts are arbitrary text or
    /// binary payloads the sandbox may run, so they are never decoded here.
    pub content: Vec<u8>,
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

/// Whether `dep` is an exactly pinned npm requirement: `package@version`, or
/// `@scope/package@version` for a scoped name.
///
/// The version must be a literal `major.minor.patch`, optionally carrying a
/// prerelease or build suffix. Everything a range could hide behind — `^1.2.3`,
/// `1.x`, `latest`, a bare name — fails here, so a declared dependency always
/// names one immutable package version.
pub(crate) fn is_pinned_npm_dep(dep: &str) -> bool {
    if dep.is_empty() || dep.len() > MAX_DEP_BYTES {
        return false;
    }
    // A scope's `@` is a prefix, so the last `@` is always the version's.
    let Some((package, version)) = dep.rsplit_once('@') else {
        return false;
    };
    is_npm_package_name(package) && is_exact_npm_version(version)
}

fn is_npm_package_name(name: &str) -> bool {
    match name.strip_prefix('@') {
        Some(scoped) => scoped.split_once('/').is_some_and(|(scope, unscoped)| {
            is_npm_name_segment(scope) && is_npm_name_segment(unscoped)
        }),
        None => is_npm_name_segment(name),
    }
}

fn is_npm_name_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn is_exact_npm_version(version: &str) -> bool {
    let (core, suffix) = version.split_at(version.find(['-', '+']).unwrap_or(version.len()));
    let mut components = core.split('.');
    let mut numeric = || {
        components.next().is_some_and(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
    };
    numeric()
        && numeric()
        && numeric()
        && components.next().is_none()
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
}

/// Parse one `SKILL.md` source: strict frontmatter between `---` fences,
/// then a non-empty markdown instruction body. `origin` records which source
/// the caller is loading from; it never comes from the manifest itself.
///
/// Recognized frontmatter keys are exactly `name`, `description`, and the
/// optional single-line `deps: { python: ["package==version", ...], npm:
/// ["package@version", ...], host: ["libreoffice"] }` (any list may be
/// omitted, but not all of them). Duplicates, an unpinned dependency, an
/// unknown host tool, or a control character in the description reject the
/// whole manifest regardless of origin.
///
/// Unknown keys split by origin. A [`SkillOrigin::Builtin`] manifest is a
/// package we ship, so an unrecognized key there is a bug worth failing on. A
/// [`SkillOrigin::User`] manifest is written against the open Agent Skills
/// format, which carries keys we have no use for (`license`, `allowed-tools`,
/// a nested `metadata:` block); those are ignored with a host-side warning,
/// along with the indented lines that belong to them, so an ordinary published
/// skill loads unmodified. Nothing ignored reaches the prompt: the catalog is
/// still built from `name` and `description` alone.
pub fn parse_skill_manifest(
    source: &str,
    origin: SkillOrigin,
) -> Result<SkillPackage, SkillParseError> {
    if source.len() > crate::MAX_WORKSPACE_FILE_BYTES {
        return Err(invalid("manifest exceeds the workspace file limit"));
    }
    let (frontmatter, body) = split_frontmatter(source).map_err(invalid)?;

    let mut name = None;
    let mut description = None;
    let mut deps = None;
    let tolerant = matches!(origin, SkillOrigin::User);
    // An ignored key's value may continue over following lines (a YAML block
    // or list). Those lines carry no key of their own, so tolerating them is
    // what makes ignoring the key mean ignoring the whole entry.
    let mut inside_ignored_value = false;
    for line in frontmatter.lines() {
        if line.trim().is_empty() {
            return Err(invalid("blank line inside frontmatter"));
        }
        let Some((key, value)) = line.split_once(':') else {
            if tolerant && inside_ignored_value {
                continue;
            }
            return Err(invalid(format!("frontmatter line without a key: {line:?}")));
        };
        let value = value.trim();
        inside_ignored_value = false;
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
                if deps.replace(parse_deps(value)?).is_some() {
                    return Err(invalid("duplicate 'deps'"));
                }
            }
            other => {
                if !tolerant {
                    return Err(invalid(format!("unknown frontmatter key {other:?}")));
                }
                tracing::warn!("ignoring unknown frontmatter key {other:?} in a user skill");
                inside_ignored_value = true;
            }
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
    let SkillDeps {
        python_deps,
        npm_deps,
        host_deps,
    } = deps.unwrap_or_default();
    Ok(SkillPackage {
        name: name.to_owned(),
        description: description.to_owned(),
        python_deps,
        npm_deps,
        host_deps,
        origin,
    })
}

/// Split `---`-fenced YAML frontmatter. Windows checkouts and user files may
/// use CRLF, so both fence encodings are accepted.
pub(crate) fn split_frontmatter(source: &str) -> Result<(&str, &str), &'static str> {
    let rest = source
        .strip_prefix("---\r\n")
        .or_else(|| source.strip_prefix("---\n"))
        .ok_or("missing opening frontmatter fence")?;
    rest.split_once("\r\n---\r\n")
        .or_else(|| rest.split_once("\n---\n"))
        .ok_or("missing closing frontmatter fence")
}

/// The three dependency lists one `deps` entry may declare.
#[derive(Debug, Default)]
struct SkillDeps {
    python_deps: Vec<String>,
    npm_deps: Vec<String>,
    host_deps: Vec<HostDep>,
}

/// Parse the single-line flow form
/// `{ python: ["a==1"], npm: ["b@1.0.0"], host: ["libreoffice"] }`.
///
/// Each key appears at most once and at least one must be present; every item
/// is a double-quoted string. The quoted grammars downstream admit neither
/// `"` nor `]`, so scanning for the closing bracket cannot land inside an
/// item.
fn parse_deps(value: &str) -> Result<SkillDeps, SkillParseError> {
    let malformed = || {
        invalid(
            "'deps' must be `{ python: [\"package==version\", ...], \
             npm: [\"package@version\", ...], host: [\"libreoffice\"] }` \
             with at least one list",
        )
    };
    let mut inner = value
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .ok_or_else(malformed)?
        .trim();
    let mut python: Option<Vec<String>> = None;
    let mut npm: Option<Vec<String>> = None;
    let mut host: Option<Vec<HostDep>> = None;
    while !inner.is_empty() {
        let (key, rest) = inner.split_once(':').ok_or_else(malformed)?;
        let rest = rest.trim_start().strip_prefix('[').ok_or_else(malformed)?;
        let (list, tail) = rest.split_once(']').ok_or_else(malformed)?;
        match key.trim() {
            "python" => {
                let parsed = parse_pinned_deps(
                    list.trim(),
                    "python",
                    "package==version",
                    is_pinned_python_dep,
                )?;
                if python.replace(parsed).is_some() {
                    return Err(invalid("duplicate 'python' list in 'deps'"));
                }
            }
            "npm" => {
                let parsed =
                    parse_pinned_deps(list.trim(), "npm", "package@version", is_pinned_npm_dep)?;
                if npm.replace(parsed).is_some() {
                    return Err(invalid("duplicate 'npm' list in 'deps'"));
                }
            }
            "host" => {
                if host.replace(parse_host_deps(list.trim())?).is_some() {
                    return Err(invalid("duplicate 'host' list in 'deps'"));
                }
            }
            other => return Err(invalid(format!("unknown 'deps' key {other:?}"))),
        }
        inner = tail.trim_start();
        if let Some(after) = inner.strip_prefix(',') {
            inner = after.trim_start();
            if inner.is_empty() {
                return Err(malformed());
            }
        } else if !inner.is_empty() {
            return Err(malformed());
        }
    }
    if python.is_none() && npm.is_none() && host.is_none() {
        return Err(invalid("'deps' lists nothing"));
    }
    Ok(SkillDeps {
        python_deps: python.unwrap_or_default(),
        npm_deps: npm.unwrap_or_default(),
        host_deps: host.unwrap_or_default(),
    })
}

/// Parse the items of one exactly pinned dependency list, where `form` is the
/// pin shape the ecosystem's `is_pinned` predicate accepts.
fn parse_pinned_deps(
    items: &str,
    key: &str,
    form: &str,
    is_pinned: fn(&str) -> bool,
) -> Result<Vec<String>, SkillParseError> {
    let malformed = || invalid(format!("'deps' {key} items must be `\"{form}\"` strings"));
    if items.is_empty() {
        return Err(invalid(format!("'deps' {key} list is empty")));
    }
    let mut deps = Vec::new();
    for item in items.split(',') {
        let dep = item
            .trim()
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(malformed)?;
        if !is_pinned(dep) {
            return Err(invalid(format!(
                "{key} dependency is not exactly pinned as {form}: {dep:?}"
            )));
        }
        deps.push(dep.to_owned());
    }
    if deps.len() > MAX_DEPS_PER_LIST {
        return Err(invalid(format!(
            "'deps' {key} list names too many packages"
        )));
    }
    Ok(deps)
}

/// Parse the items of one `host: [...]` list against the closed vocabulary.
fn parse_host_deps(items: &str) -> Result<Vec<HostDep>, SkillParseError> {
    let malformed = || invalid("'deps' host items must be `\"libreoffice\"` strings");
    if items.is_empty() {
        return Err(invalid("'deps' host list is empty"));
    }
    let mut deps = Vec::new();
    for item in items.split(',') {
        let tool = item
            .trim()
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .ok_or_else(malformed)?;
        let Some(tool) = HostDep::parse(tool) else {
            return Err(invalid(format!("unknown host tool in 'deps': {tool:?}")));
        };
        if deps.contains(&tool) {
            return Err(invalid("duplicate host tool in 'deps'"));
        }
        deps.push(tool);
    }
    Ok(deps)
}

/// Load every valid skill package under `source`, one directory per skill,
/// tagging each with `origin`.
///
/// A malformed package — an unreadable or oversized manifest, frontmatter the
/// strict parser rejects, or a directory whose name disagrees with its
/// manifest — is skipped with a warning so one bad file can never break the
/// prompt or fail an exec. The result is sorted by name for deterministic
/// staging and catalog order.
#[must_use]
pub fn load_skills(source: &Path, origin: SkillOrigin) -> Vec<LoadedSkill> {
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
        let package = match parse_skill_manifest(&manifest, origin) {
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
        let Some(scripts) = load_skill_scripts(&entry.path().join(SKILL_SCRIPTS_DIR)) else {
            tracing::warn!("skipping skill '{directory_name}': its scripts/ exceeds the limits");
            continue;
        };
        skills.push(LoadedSkill {
            package,
            manifest,
            scripts,
        });
    }
    skills.sort_by(|a, b| a.package.name.cmp(&b.package.name));
    skills
}

/// Read a skill's optional `scripts/` directory: regular files one level deep,
/// symlink-safe at every step, sorted by name.
///
/// Returns `None` when the directory breaks a bound — more than
/// [`MAX_SKILL_SCRIPTS`] files, or one larger than the workspace file limit —
/// so the caller drops the whole package. A half-staged skill whose
/// instructions reference a script that never arrived fails in the sandbox,
/// where the model cannot tell a missing helper from a broken one; not
/// advertising the skill at all is the honest outcome. A missing directory is
/// simply an empty script set.
fn load_skill_scripts(source: &Path) -> Option<Vec<SkillScript>> {
    let is_directory = source
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    if !is_directory {
        return Some(Vec::new());
    }
    let entries = match std::fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                "skill scripts at {} are unreadable: {error}",
                source.display()
            );
            return None;
        }
    };
    let mut scripts = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let regular_file = entry
            .path()
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        if !regular_file {
            // A nested directory or a symlink is not staged; the package's own
            // files still are.
            tracing::warn!("skipping skill script '{name}': not a regular file");
            continue;
        }
        let Ok(content) = std::fs::read(entry.path()) else {
            tracing::warn!("skill script '{name}' is unreadable");
            return None;
        };
        if content.len() > crate::MAX_WORKSPACE_FILE_BYTES {
            tracing::warn!("skill script '{name}' exceeds the workspace file limit");
            return None;
        }
        scripts.push(SkillScript { name, content });
        if scripts.len() > MAX_SKILL_SCRIPTS {
            tracing::warn!("a skill declares more than {MAX_SKILL_SCRIPTS} scripts");
            return None;
        }
    }
    scripts.sort_by(|a, b| a.name.cmp(&b.name));
    Some(scripts)
}

/// The built-in skills plus the user-authored packages under `user_dir`,
/// merged into one deterministic catalog.
///
/// User skills go through the same strict loader as built-ins. A user package
/// whose name collides with a built-in is dropped with a warning — curated
/// instructions can never be shadowed. A missing user directory is simply an
/// empty user set, not an error, so embeddings without one stage exactly the
/// built-ins. The result is sorted by name.
#[must_use]
pub fn merged_skills(builtins: &[LoadedSkill], user_dir: Option<&Path>) -> Vec<LoadedSkill> {
    let mut skills = builtins.to_vec();
    if let Some(dir) = user_dir.filter(|dir| dir.is_dir()) {
        for user_skill in load_skills(dir, SkillOrigin::User) {
            let name = &user_skill.package.name;
            if builtins.iter().any(|skill| skill.package.name == *name) {
                tracing::warn!("skipping user skill '{name}': a built-in skill owns that name");
                continue;
            }
            skills.push(user_skill);
        }
        skills.sort_by(|a, b| a.package.name.cmp(&b.package.name));
    }
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
        let package = parse_skill_manifest(VALID, SkillOrigin::Builtin).unwrap();
        assert_eq!(package.name, "pdf-documents");
        assert_eq!(
            package.description,
            "Create, merge, split, and fill PDF documents."
        );
        assert_eq!(package.python_deps, ["fpdf2==2.8.3", "pypdf==5.1.0"]);
        assert_eq!(package.host_deps, []);

        let no_deps = "---\nname: charts\ndescription: Plots.\n---\nBody.\n";
        assert_eq!(
            parse_skill_manifest(no_deps, SkillOrigin::Builtin)
                .unwrap()
                .python_deps,
            [""; 0]
        );

        let crlf = VALID.replace('\n', "\r\n");
        let package = parse_skill_manifest(&crlf, SkillOrigin::Builtin).unwrap();
        assert_eq!(package.name, "pdf-documents");
    }

    /// The `host:` list is a closed vocabulary: `libreoffice` parses into its
    /// enum value alongside python pins or alone, and anything else rejects
    /// the manifest rather than becoming a string nothing can act on. That
    /// includes host deps the rest of the system derives for itself, which no
    /// manifest gets to assert.
    #[test]
    fn host_deps_parse_from_the_closed_vocabulary_only() {
        let combined = "---\nname: word-documents\ndescription: Docs.\n\
                        deps: { python: [\"python-docx==1.1.2\"], host: [\"libreoffice\"] }\n\
                        ---\nBody.\n";
        let package = parse_skill_manifest(combined, SkillOrigin::Builtin).unwrap();
        assert_eq!(package.python_deps, ["python-docx==1.1.2"]);
        assert_eq!(package.host_deps, [HostDep::LibreOffice]);

        let host_only =
            "---\nname: a\ndescription: b\ndeps: { host: [\"libreoffice\"] }\n---\nBody.\n";
        let package = parse_skill_manifest(host_only, SkillOrigin::Builtin).unwrap();
        assert_eq!(package.python_deps, [""; 0]);
        assert_eq!(package.host_deps, [HostDep::LibreOffice]);

        for (case, source) in [
            (
                "unknown host tool",
                "---\nname: a\ndescription: b\ndeps: { host: [\"imagemagick\"] }\n---\nBody.\n",
            ),
            (
                "a host tool no manifest may declare",
                "---\nname: a\ndescription: b\ndeps: { host: [\"node\"] }\n---\nBody.\n",
            ),
            (
                "duplicate host tool",
                "---\nname: a\ndescription: b\ndeps: { host: [\"libreoffice\", \"libreoffice\"] }\n---\nBody.\n",
            ),
            (
                "duplicate host list",
                "---\nname: a\ndescription: b\ndeps: { host: [\"libreoffice\"], host: [\"libreoffice\"] }\n---\nBody.\n",
            ),
            (
                "empty host list",
                "---\nname: a\ndescription: b\ndeps: { host: [] }\n---\nBody.\n",
            ),
            (
                "unknown deps key",
                "---\nname: a\ndescription: b\ndeps: { cargo: [\"serde@1.0.0\"] }\n---\nBody.\n",
            ),
            (
                "trailing comma",
                "---\nname: a\ndescription: b\ndeps: { host: [\"libreoffice\"], }\n---\nBody.\n",
            ),
        ] {
            assert!(
                parse_skill_manifest(source, SkillOrigin::Builtin).is_err(),
                "{case} should be rejected"
            );
        }
    }

    /// The `npm:` list holds the same discipline as `python:`: an exact
    /// `package@version` pin, scoped names included, and nothing a range or a
    /// floating tag could hide behind.
    #[test]
    fn npm_deps_parse_only_when_exactly_pinned() {
        let source = "---\nname: diagrams\ndescription: Diagrams.\n\
                      deps: { python: [\"pypdf==6.14.2\"], \
                      npm: [\"mermaid@11.4.1\", \"@mermaid-js/mermaid-cli@11.4.2\"], \
                      host: [\"libreoffice\"] }\n---\nBody.\n";
        let package = parse_skill_manifest(source, SkillOrigin::Builtin).unwrap();
        assert_eq!(package.python_deps, ["pypdf==6.14.2"]);
        assert_eq!(
            package.npm_deps,
            ["mermaid@11.4.1", "@mermaid-js/mermaid-cli@11.4.2"]
        );
        assert_eq!(package.host_deps, [HostDep::LibreOffice]);

        // npm alone satisfies "at least one list", and a prerelease pin is
        // still an exact version.
        let npm_only = "---\nname: a\ndescription: b\n\
                        deps: { npm: [\"puppeteer@24.0.0-next.1\"] }\n---\nBody.\n";
        let package = parse_skill_manifest(npm_only, SkillOrigin::Builtin).unwrap();
        assert_eq!(package.npm_deps, ["puppeteer@24.0.0-next.1"]);
        assert_eq!(package.python_deps, [""; 0]);

        for (case, list) in [
            ("caret range", "\"mermaid@^11.4.1\""),
            ("tilde range", "\"mermaid@~11.4.1\""),
            ("partial version", "\"mermaid@11.4\""),
            ("wildcard version", "\"mermaid@11.x\""),
            ("floating tag", "\"mermaid@latest\""),
            ("no version", "\"mermaid\""),
            ("scoped without version", "\"@mermaid-js/mermaid-cli\""),
            ("scope without a name", "\"@mermaid-js@11.4.1\""),
            ("uppercase name", "\"Mermaid@11.4.1\""),
            ("python-style pin", "\"mermaid==11.4.1\""),
            ("empty list", ""),
            ("unquoted item", "mermaid@11.4.1"),
            (
                "too many packages",
                "\"a@1.0.0\", \"b@1.0.0\", \"c@1.0.0\", \"d@1.0.0\", \"e@1.0.0\", \
                 \"f@1.0.0\", \"g@1.0.0\", \"h@1.0.0\", \"i@1.0.0\"",
            ),
        ] {
            let source =
                format!("---\nname: a\ndescription: b\ndeps: {{ npm: [{list}] }}\n---\nBody.\n");
            assert!(
                parse_skill_manifest(&source, SkillOrigin::Builtin).is_err(),
                "{case} should be rejected"
            );
        }

        let duplicate = "---\nname: a\ndescription: b\n\
                         deps: { npm: [\"mermaid@11.4.1\"], npm: [\"mermaid@11.4.1\"] }\n\
                         ---\nBody.\n";
        assert!(parse_skill_manifest(duplicate, SkillOrigin::Builtin).is_err());
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
                parse_skill_manifest(source, SkillOrigin::Builtin).is_err(),
                "{case} should be rejected"
            );
        }
    }

    /// Contract: a user package written in the open Agent Skills format —
    /// unknown scalar keys, a nested block, a list — loads with those fields
    /// ignored, while the same manifest from a built-in still rejects, because
    /// an unrecognized key in a package we ship is a defect.
    #[test]
    fn unknown_frontmatter_keys_are_ignored_for_user_skills_only() {
        let published = "---\n\
name: web-research\n\
description: Research a topic and write up the findings.\n\
license: Apache-2.0\n\
allowed-tools:\n\
  - Read\n\
  - Bash\n\
metadata:\n\
  version: 1.2.0\n\
  authors:\n\
    - someone\n\
---\n\
# Web research\n\
Instructions.\n";
        let package = parse_skill_manifest(published, SkillOrigin::User).unwrap();
        assert_eq!(package.name, "web-research");
        assert_eq!(
            package.description,
            "Research a topic and write up the findings."
        );
        assert!(parse_skill_manifest(published, SkillOrigin::Builtin).is_err());

        // Tolerance is limited to unknown keys: everything that reaches the
        // prompt or drives host behavior still rejects a user manifest.
        for (case, source) in [
            (
                "duplicate known key",
                "---\nname: a\nname: b\ndescription: c\n---\nBody.\n",
            ),
            (
                "control character in description",
                "---\nname: a\ndescription: b\u{7}c\n---\nBody.\n",
            ),
            (
                "unpinned dependency",
                "---\nname: a\ndescription: b\ndeps: { python: [\"fpdf2>=2\"] }\n---\nBody.\n",
            ),
            (
                "unknown host tool",
                "---\nname: a\ndescription: b\ndeps: { host: [\"imagemagick\"] }\n---\nBody.\n",
            ),
            (
                "keyless line before any ignored key",
                "---\nname: a\njust a line\ndescription: b\n---\nBody.\n",
            ),
            ("empty body", "---\nname: a\ndescription: b\n---\n  \n"),
        ] {
            assert!(
                parse_skill_manifest(source, SkillOrigin::User).is_err(),
                "{case} should be rejected for user skills too"
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

        let skills = load_skills(source.path(), SkillOrigin::Builtin);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].package.name, "pdf-documents");
        assert_eq!(skills[0].package.origin, SkillOrigin::Builtin);
        assert_eq!(skills[0].manifest, VALID);
        assert!(skills[0].scripts.is_empty());
    }

    /// Contract: a package's `scripts/` directory is collected verbatim beside
    /// the manifest, and a package that breaks the staging bounds drops out
    /// whole rather than reaching the catalog with helpers the sandbox will
    /// not find.
    #[test]
    fn scripts_are_collected_within_bounds_or_the_skill_drops_out() {
        let source = tempfile::tempdir().unwrap();
        let skill = source.path().join("pdf-documents");
        let scripts = skill.join(SKILL_SCRIPTS_DIR);
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(skill.join(SKILL_MANIFEST_FILE), VALID).unwrap();
        std::fs::write(scripts.join("fill.py"), "print('fill')").unwrap();
        std::fs::write(scripts.join("build.py"), "print('build')").unwrap();
        // One level only: a nested directory is not staged, and does not cost
        // the package its catalog entry.
        std::fs::create_dir(scripts.join("nested")).unwrap();

        let loaded = load_skills(source.path(), SkillOrigin::Builtin);
        assert_eq!(
            loaded[0]
                .scripts
                .iter()
                .map(|script| (script.name.as_str(), script.content.as_slice()))
                .collect::<Vec<_>>(),
            [
                ("build.py", b"print('build')".as_slice()),
                ("fill.py", b"print('fill')".as_slice()),
            ]
        );

        std::fs::write(
            scripts.join("huge.py"),
            vec![b'x'; crate::MAX_WORKSPACE_FILE_BYTES + 1],
        )
        .unwrap();
        assert!(load_skills(source.path(), SkillOrigin::Builtin).is_empty());
        std::fs::remove_file(scripts.join("huge.py")).unwrap();

        for index in 0..=MAX_SKILL_SCRIPTS {
            std::fs::write(scripts.join(format!("extra{index}.py")), "pass").unwrap();
        }
        assert!(load_skills(source.path(), SkillOrigin::Builtin).is_empty());
    }

    /// Contract: a user package goes through the same strict loader, carries
    /// its origin, and can never shadow a built-in name.
    #[test]
    fn user_skills_merge_after_builtins_without_shadowing() {
        let builtin_dir = tempfile::tempdir().unwrap();
        let pdf = builtin_dir.path().join("pdf-documents");
        std::fs::create_dir(&pdf).unwrap();
        std::fs::write(pdf.join(SKILL_MANIFEST_FILE), VALID).unwrap();
        let builtins = load_skills(builtin_dir.path(), SkillOrigin::Builtin);

        let user_dir = tempfile::tempdir().unwrap();
        // Collides with the built-in: dropped, the built-in manifest survives.
        let shadow = user_dir.path().join("pdf-documents");
        std::fs::create_dir(&shadow).unwrap();
        std::fs::write(
            shadow.join(SKILL_MANIFEST_FILE),
            "---\nname: pdf-documents\ndescription: Impostor.\n---\nBody.\n",
        )
        .unwrap();
        let own = user_dir.path().join("meeting-notes");
        std::fs::create_dir(&own).unwrap();
        std::fs::write(
            own.join(SKILL_MANIFEST_FILE),
            "---\nname: meeting-notes\ndescription: Summarize meetings my way.\n---\nBody.\n",
        )
        .unwrap();

        let merged = merged_skills(&builtins, Some(user_dir.path()));
        assert_eq!(
            merged
                .iter()
                .map(|skill| (skill.package.name.as_str(), skill.package.origin))
                .collect::<Vec<_>>(),
            [
                ("meeting-notes", SkillOrigin::User),
                ("pdf-documents", SkillOrigin::Builtin),
            ]
        );
        assert_eq!(merged[1].manifest, VALID);

        // A missing user directory is an empty user set, not an error.
        let missing = user_dir.path().join("does-not-exist");
        assert_eq!(merged_skills(&builtins, Some(&missing)).len(), 1);
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
        let skills = load_skills(&source, SkillOrigin::Builtin);
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
        // The office skills teach a LibreOffice-backed loop — visual QA for
        // the render paths, in-place editing and recalculation for
        // spreadsheets. The declaration is what drives the host-side install
        // and the honest capability line, so losing it silently regresses
        // both.
        for name in ["presentations", "spreadsheets", "word-documents"] {
            let skill = skills
                .iter()
                .find(|skill| skill.package.name == name)
                .unwrap();
            assert_eq!(
                skill.package.host_deps,
                [HostDep::LibreOffice],
                "{name} must declare its LibreOffice host dependency"
            );
        }
    }
}
