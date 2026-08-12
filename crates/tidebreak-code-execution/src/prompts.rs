//! Reusable prompts: saved starting messages the user can insert.
//!
//! A prompt is a directory whose `PROMPT.md` carries a name, a one-line tip,
//! and a markdown body. The body is the text a surface drops into the
//! composer when the user picks it — nothing else. That is the whole feature,
//! and the narrowness is deliberate: a prompt is **user-side content**. It
//! never enters the operating prompt, is never staged into an exec workspace,
//! and has no catalog line, so nothing here is reachable by the model except
//! by the user choosing to send it.
//!
//! That is what separates a prompt from a skill. A skill is instructions the
//! model routes on, so `skills.rs` bounds every string that reaches the prompt
//! and stages files into the sandbox. A prompt has neither reach, so it needs
//! only the checks that keep a management surface honest: a well-formed slug
//! for addressing, a bounded printable description for the card, and a bounded
//! body so a stray file cannot become a multi-megabyte fetch.
//!
//! Prompts come from the same two sources skills do — packages shipped with
//! the application, and user-authored directories under a per-install path —
//! and follow the same rules: one strict parser for both, a name collision
//! resolving in the built-in's favor, and skip-with-warning per package so one
//! bad file cannot take out the library.

use std::path::Path;

/// The manifest file every prompt package is defined by.
pub const PROMPT_MANIFEST_FILE: &str = "PROMPT.md";

const MAX_NAME_BYTES: usize = 64;
const MAX_DESCRIPTION_BYTES: usize = 200;

/// Bound on the insertable body.
///
/// A prompt is a starting message a person wrote, so this is generous by two
/// orders of magnitude for the real cases; it exists so a file that wandered
/// into the prompts directory cannot become an unbounded response body.
pub const MAX_PROMPT_BODY_BYTES: usize = 16 * 1024;

/// Which source a validated prompt package was loaded from.
///
/// Host-derived from the load path, never from manifest content, so a user
/// package cannot claim to be built-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum PromptOrigin {
    /// Shipped with the application from a trusted resource directory.
    Builtin,
    /// Authored by the user in the per-install prompts directory.
    User,
}

/// Host-derived library entry parsed from a prompt manifest's frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptPackage {
    /// Kebab-case slug; also the directory name and the route's address.
    pub name: String,
    /// One printable line: the tip a card or popover shows.
    pub description: String,
    /// Where the package was loaded from.
    pub origin: PromptOrigin,
}

/// One validated prompt: its library entry plus the text to insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedPrompt {
    /// The parsed frontmatter a library listing is built from.
    pub package: PromptPackage,
    /// The markdown below the frontmatter, inserted into the composer verbatim.
    pub body: String,
}

/// Why a prompt manifest was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid prompt manifest: {0}")]
pub struct PromptParseError(String);

fn invalid(reason: impl Into<String>) -> PromptParseError {
    PromptParseError(reason.into())
}

/// Whether `name` is a well-formed prompt slug: bounded kebab-case with no
/// empty segments — the same grammar skill and plugin names use.
#[must_use]
pub fn is_valid_prompt_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .split('-')
            .all(|segment| !segment.is_empty() && segment.bytes().all(is_slug_byte))
}

fn is_slug_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// Parse one `PROMPT.md` source: strict frontmatter between `---` fences, then
/// a non-empty markdown body. `origin` records which source the caller is
/// loading from; it never comes from the manifest itself.
///
/// Recognized keys are exactly `name` and `description`, both required. A
/// duplicate, a name that is not a slug, a description that is not one bounded
/// printable line, an empty body, or a body past
/// [`MAX_PROMPT_BODY_BYTES`] rejects the manifest regardless of origin.
///
/// Unknown keys split by origin exactly as skill manifests do: a built-in is a
/// package we ship, so an unrecognized key there is a defect worth failing on,
/// while a user manifest may carry fields from whatever editor wrote it and
/// those are ignored with a warning, along with the indented lines belonging
/// to them.
pub fn parse_prompt_manifest(
    source: &str,
    origin: PromptOrigin,
) -> Result<LoadedPrompt, PromptParseError> {
    let rest = source
        .strip_prefix("---\n")
        .ok_or_else(|| invalid("missing opening frontmatter fence"))?;
    let (frontmatter, body) = rest
        .split_once("\n---\n")
        .ok_or_else(|| invalid("missing closing frontmatter fence"))?;

    let mut name = None;
    let mut description = None;
    let tolerant = matches!(origin, PromptOrigin::User);
    // An ignored key's value may continue over following lines. Those lines
    // carry no key of their own, so tolerating them is what makes ignoring the
    // key mean ignoring the whole entry.
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
            other => {
                if !tolerant {
                    return Err(invalid(format!("unknown frontmatter key {other:?}")));
                }
                tracing::warn!("ignoring unknown frontmatter key {other:?} in a user prompt");
                inside_ignored_value = true;
            }
        }
    }

    let name = name.ok_or_else(|| invalid("missing 'name'"))?;
    if !is_valid_prompt_name(name) {
        return Err(invalid(format!(
            "'name' is not a kebab-case slug: {name:?}"
        )));
    }
    let description = description.ok_or_else(|| invalid("missing 'description'"))?;
    if description.is_empty()
        || description.len() > MAX_DESCRIPTION_BYTES
        || description.trim() != description
        || description.chars().any(char::is_control)
    {
        return Err(invalid("'description' is not one bounded printable line"));
    }
    let body = body.trim();
    if body.is_empty() {
        return Err(invalid("empty prompt body"));
    }
    if body.len() > MAX_PROMPT_BODY_BYTES {
        return Err(invalid("prompt body exceeds the insertable limit"));
    }
    Ok(LoadedPrompt {
        package: PromptPackage {
            name: name.to_owned(),
            description: description.to_owned(),
            origin,
        },
        body: body.to_owned(),
    })
}

/// Load every valid prompt package under `source`, one directory per prompt,
/// tagging each with `origin`.
///
/// A malformed package — an unreadable manifest, frontmatter the parser
/// rejects, or a directory whose name disagrees with its manifest — is skipped
/// with a warning so one bad file can never break the library. The result is
/// sorted by name for a deterministic listing.
#[must_use]
pub fn load_prompts(source: &Path, origin: PromptOrigin) -> Vec<LoadedPrompt> {
    let mut prompts = Vec::new();
    let entries = match std::fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                "prompt source directory {} is unreadable: {error}",
                source.display()
            );
            return prompts;
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
        let manifest_path = entry.path().join(PROMPT_MANIFEST_FILE);
        let regular_file = manifest_path
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink());
        if !regular_file {
            tracing::warn!("skipping prompt '{directory_name}': no regular {PROMPT_MANIFEST_FILE}");
            continue;
        }
        let manifest = match std::fs::read_to_string(&manifest_path) {
            Ok(manifest) => manifest,
            Err(error) => {
                tracing::warn!("skipping prompt '{directory_name}': manifest unreadable: {error}");
                continue;
            }
        };
        let prompt = match parse_prompt_manifest(&manifest, origin) {
            Ok(prompt) => prompt,
            Err(error) => {
                tracing::warn!("skipping prompt '{directory_name}': {error}");
                continue;
            }
        };
        if prompt.package.name != directory_name {
            tracing::warn!(
                "skipping prompt '{directory_name}': manifest names itself {:?}",
                prompt.package.name
            );
            continue;
        }
        prompts.push(prompt);
    }
    prompts.sort_by(|a, b| a.package.name.cmp(&b.package.name));
    prompts
}

/// The built-in prompts plus the user-authored packages under `user_dir`,
/// merged into one deterministic library.
///
/// User prompts go through the same parser as built-ins. A user package whose
/// name collides with a built-in is dropped with a warning, so a curated
/// prompt can never be shadowed by a local file of the same name. A missing
/// user directory is simply an empty user set.
#[must_use]
pub fn merged_prompts(builtins: &[LoadedPrompt], user_dir: Option<&Path>) -> Vec<LoadedPrompt> {
    let mut prompts = builtins.to_vec();
    if let Some(dir) = user_dir.filter(|dir| dir.is_dir()) {
        for user_prompt in load_prompts(dir, PromptOrigin::User) {
            let name = &user_prompt.package.name;
            if builtins.iter().any(|prompt| prompt.package.name == *name) {
                tracing::warn!("skipping user prompt '{name}': a built-in prompt owns that name");
                continue;
            }
            prompts.push(user_prompt);
        }
        prompts.sort_by(|a, b| a.package.name.cmp(&b.package.name));
    }
    prompts
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "---\n\
name: weekly-update\n\
description: Draft this week's status update.\n\
---\n\
\n\
Write a status update covering what shipped, what slipped, and what is next.\n";

    /// Contract: the manifest splits into the library entry and the exact text
    /// a composer inserts, and everything that would make either dishonest —
    /// a missing key, a name that cannot address a route, an unbounded body —
    /// rejects the package instead of half-loading it.
    #[test]
    fn manifests_parse_into_an_entry_and_a_body_or_are_rejected() {
        let prompt = parse_prompt_manifest(VALID, PromptOrigin::Builtin).unwrap();
        assert_eq!(prompt.package.name, "weekly-update");
        assert_eq!(
            prompt.package.description,
            "Draft this week's status update."
        );
        assert_eq!(
            prompt.body,
            "Write a status update covering what shipped, what slipped, and what is next."
        );

        let oversized = format!(
            "---\nname: a\ndescription: b\n---\n{}\n",
            "x".repeat(MAX_PROMPT_BODY_BYTES + 1)
        );
        for (case, source) in [
            ("no frontmatter", "# Just markdown\n".to_owned()),
            ("unclosed frontmatter", "---\nname: a\n".to_owned()),
            (
                "missing description",
                "---\nname: a\n---\nBody.\n".to_owned(),
            ),
            (
                "non-kebab name",
                "---\nname: Weekly Update\ndescription: b\n---\nBody.\n".to_owned(),
            ),
            (
                "control character in description",
                "---\nname: a\ndescription: b\u{7}c\n---\nBody.\n".to_owned(),
            ),
            (
                "unknown key in a built-in",
                "---\nname: a\ndescription: b\nauthor: c\n---\nBody.\n".to_owned(),
            ),
            (
                "empty body",
                "---\nname: a\ndescription: b\n---\n  \n".to_owned(),
            ),
            ("oversized body", oversized),
        ] {
            assert!(
                parse_prompt_manifest(&source, PromptOrigin::Builtin).is_err(),
                "{case} should be rejected"
            );
        }

        // A user-authored file may carry keys from whatever wrote it; the same
        // manifest is still a defect in a package we ship.
        let extra = "---\nname: a\ndescription: b\nauthor: someone\ntags:\n  - one\n---\nBody.\n";
        assert!(parse_prompt_manifest(extra, PromptOrigin::User).is_ok());
        assert!(parse_prompt_manifest(extra, PromptOrigin::Builtin).is_err());
    }

    /// Contract: a user package goes through the same loader, carries its
    /// origin, and can never shadow a built-in name.
    #[test]
    fn user_prompts_merge_after_builtins_without_shadowing() {
        let write = |dir: &Path, name: &str, source: &str| {
            let prompt = dir.join(name);
            std::fs::create_dir(&prompt).unwrap();
            std::fs::write(prompt.join(PROMPT_MANIFEST_FILE), source).unwrap();
        };

        let builtin_dir = tempfile::tempdir().unwrap();
        write(builtin_dir.path(), "weekly-update", VALID);
        // Parses, but the directory disagrees with the manifest.
        write(builtin_dir.path(), "mislabeled", VALID);
        let builtins = load_prompts(builtin_dir.path(), PromptOrigin::Builtin);
        assert_eq!(builtins.len(), 1);

        let user_dir = tempfile::tempdir().unwrap();
        write(
            user_dir.path(),
            "weekly-update",
            "---\nname: weekly-update\ndescription: Impostor.\n---\nImpostor body.\n",
        );
        write(
            user_dir.path(),
            "standup",
            "---\nname: standup\ndescription: My standup format.\n---\nYesterday, today, blockers.\n",
        );
        write(user_dir.path(), "broken", "no frontmatter\n");

        let merged = merged_prompts(&builtins, Some(user_dir.path()));
        assert_eq!(
            merged
                .iter()
                .map(|prompt| (prompt.package.name.as_str(), prompt.package.origin))
                .collect::<Vec<_>>(),
            [
                ("standup", PromptOrigin::User),
                ("weekly-update", PromptOrigin::Builtin),
            ]
        );
        assert!(merged[1].body.starts_with("Write a status update"));

        // A missing user directory is an empty user set, not an error.
        let missing = user_dir.path().join("does-not-exist");
        assert_eq!(merged_prompts(&builtins, Some(&missing)).len(), 1);
    }
}
