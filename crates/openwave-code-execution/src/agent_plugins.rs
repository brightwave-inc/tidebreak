//! The Agent Plugins packaging format (<https://agent-plugins.org>), v1.0.0.
//!
//! A plugin published in that format is a directory whose root carries a
//! `plugin.json` manifest, with components discovered at fixed locations —
//! skills under `skills/`, one directory each. This module is the reader for
//! that manifest: it validates the closed schema the specification defines and
//! hands back the few facts OpenWave's own [`crate::plugins`] representation
//! needs, so the importer can convert a standard package into the internal
//! `PLUGIN.md` shape the loaders already understand.
//!
//! Two properties of the specification drive the shape of this code:
//!
//! * **`$schema` selects the validation rules.** It is required, and a client
//!   that does not implement the identifier it names must reject the plugin
//!   rather than guess. Schemas are never fetched while loading.
//! * **Failures are graded.** Only two manifest violations are non-fatal —
//!   unknown top-level fields, and an `extensions` value that is not an
//!   object — and both are reported and ignored. Everything else rejects the
//!   plugin whole, so a package never loads in a shape its author did not
//!   describe.
//!
//! Client-specific data rides in `extensions` under reverse-domain
//! namespaces. OpenWave reads [`OPENWAVE_EXTENSION_NAMESPACE`] and ignores
//! every other namespace without inspecting it, which is what the
//! specification requires of a client that does not implement one. Because
//! that namespace is ours, a malformed value inside it is reported and ignored
//! rather than fatal: the plugin still describes itself correctly to every
//! other client.

use crate::plugins::{is_valid_plugin_router_preamble, PluginCategory};

/// The manifest file at the root of a plugin published in the standard format.
pub const AGENT_PLUGIN_MANIFEST_FILE: &str = "plugin.json";

/// The fixed location standard-format skills are discovered at.
pub const AGENT_PLUGIN_SKILLS_DIR: &str = "skills";

/// The specification version this client implements.
pub const AGENT_PLUGIN_SPEC_VERSION: &str = "1.0.0";

/// The only `$schema` identifier [`parse_agent_plugin_manifest`] accepts.
pub const AGENT_PLUGIN_SCHEMA_ID: &str =
    "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// The reverse-domain namespace OpenWave's own manifest data lives under.
pub const OPENWAVE_EXTENSION_NAMESPACE: &str = "io.brightwave.openwave";

const MAX_NAME_CHARS: usize = 64;

/// The manifest fields OpenWave acts on, after validation.
///
/// Metadata the specification type-checks but this client has no consumer for
/// — `version`, `author`, `homepage`, `repository`, `license`, `keywords` — is
/// validated and dropped rather than carried into a representation nothing
/// renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPluginManifest {
    /// The package name, in the specification's grammar (which admits `.`,
    /// unlike OpenWave's own slug).
    pub name: String,
    /// Free-form description, unbounded and unsanitized as the specification
    /// leaves it. Callers rendering it must bound it themselves.
    pub description: Option<String>,
    /// `category` from the OpenWave extension namespace.
    pub category: Option<PluginCategory>,
    /// `router-preamble` from the OpenWave extension namespace, already
    /// checked against the same one-line rule the internal parser applies.
    pub router_preamble: Option<String>,
}

/// One manifest field that was reported and ignored instead of rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredManifestField {
    /// Pointer-ish location of the field, e.g. `extensions` or `keywords`.
    pub field: String,
    pub reason: String,
}

/// A manifest that validated, with the non-fatal violations found along the
/// way. The specification recommends surfacing these, so they are returned
/// rather than logged and dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAgentPluginManifest {
    pub manifest: AgentPluginManifest,
    pub ignored: Vec<IgnoredManifestField>,
}

/// Why a `plugin.json` was rejected whole.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid plugin.json: {0}")]
pub struct AgentPluginParseError(String);

fn invalid(reason: impl Into<String>) -> AgentPluginParseError {
    AgentPluginParseError(reason.into())
}

/// Whether `name` matches the specification's package-name grammar: 1–64
/// characters of `a-z`, `0-9`, `-`, and `.`, alphanumeric at both ends, with
/// no `--` and no `..`.
///
/// Written by hand rather than as a pattern because the published schema
/// expresses the repeat prohibitions with lookahead, which the `regex` crate
/// deliberately does not support.
#[must_use]
pub fn is_valid_agent_plugin_name(name: &str) -> bool {
    let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_NAME_CHARS
        && bytes
            .iter()
            .all(|byte| alphanumeric(*byte) || matches!(byte, b'-' | b'.'))
        && bytes.first().is_some_and(|byte| alphanumeric(*byte))
        && bytes.last().is_some_and(|byte| alphanumeric(*byte))
        && !name.contains("--")
        && !name.contains("..")
}

/// Parse and validate one `plugin.json` against the v1.0.0 schema.
///
/// The manifest is a closed object: `$schema`, `name`, `version`,
/// `description`, `author`, `homepage`, `repository`, `license`, `keywords`,
/// and `extensions` are the whole vocabulary. `$schema` and `name` are
/// required. Optional metadata is checked by JSON type only — a version that
/// is not SemVer, a license that is not an SPDX identifier, or a URL this
/// client cannot parse are all valid manifests, and rejecting them would make
/// this client stricter than the format it claims to read.
pub fn parse_agent_plugin_manifest(
    source: &str,
) -> Result<ParsedAgentPluginManifest, AgentPluginParseError> {
    let value: serde_json::Value = serde_json::from_str(source)
        .map_err(|error| invalid(format!("not valid JSON: {error}")))?;
    let serde_json::Value::Object(object) = value else {
        return Err(invalid("manifest is not a JSON object"));
    };

    let schema = object
        .get("$schema")
        .ok_or_else(|| invalid("missing '$schema'"))?
        .as_str()
        .ok_or_else(|| invalid("'$schema' is not a string"))?;
    if schema != AGENT_PLUGIN_SCHEMA_ID {
        return Err(invalid(format!(
            "'$schema' names {schema:?}, which is not the supported \
             Agent Plugins {AGENT_PLUGIN_SPEC_VERSION} manifest schema"
        )));
    }

    let name = object
        .get("name")
        .ok_or_else(|| invalid("missing 'name'"))?
        .as_str()
        .ok_or_else(|| invalid("'name' is not a string"))?;
    if !is_valid_agent_plugin_name(name) {
        return Err(invalid(format!(
            "'name' does not match the package-name grammar: {name:?}"
        )));
    }

    let mut ignored = Vec::new();
    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "$schema"
                | "name"
                | "version"
                | "description"
                | "author"
                | "homepage"
                | "repository"
                | "license"
                | "keywords"
                | "extensions"
        ) {
            ignored.push(IgnoredManifestField {
                field: key.clone(),
                reason: format!(
                    "field is not part of the Agent Plugins {AGENT_PLUGIN_SPEC_VERSION} \
                     manifest schema"
                ),
            });
        }
    }

    for key in [
        "version",
        "description",
        "homepage",
        "repository",
        "license",
    ] {
        if let Some(value) = object.get(key) {
            if !value.is_string() {
                return Err(invalid(format!("'{key}' is not a string")));
            }
        }
    }
    if let Some(keywords) = object.get("keywords") {
        let keywords = keywords
            .as_array()
            .ok_or_else(|| invalid("'keywords' is not an array"))?;
        if !keywords.iter().all(serde_json::Value::is_string) {
            return Err(invalid("'keywords' contains a non-string entry"));
        }
    }
    if let Some(author) = object.get("author") {
        let author = author
            .as_object()
            .ok_or_else(|| invalid("'author' is not an object"))?;
        for (key, value) in author {
            if !matches!(key.as_str(), "name" | "email" | "url") {
                return Err(invalid(format!("'author' has unknown field {key:?}")));
            }
            if !value.is_string() {
                return Err(invalid(format!("'author.{key}' is not a string")));
            }
        }
    }

    let mut category = None;
    let mut router_preamble = None;
    match object.get("extensions") {
        None => {}
        // The one shape the specification downgrades to a warning: a client
        // that cannot read the extension block still loads the components.
        Some(value) if !value.is_object() => ignored.push(IgnoredManifestField {
            field: "extensions".to_owned(),
            reason: "value is not an object".to_owned(),
        }),
        Some(value) => {
            let extensions = value.as_object().expect("checked above");
            for (namespace, value) in extensions {
                if !value.is_object() {
                    return Err(invalid(format!(
                        "'extensions.{namespace}' is not an object"
                    )));
                }
            }
            // Every other namespace is ignored without inspecting it, which
            // is exactly what the specification asks of a client that does
            // not implement one.
            if let Some(ours) = extensions
                .get(OPENWAVE_EXTENSION_NAMESPACE)
                .and_then(serde_json::Value::as_object)
            {
                for (key, value) in ours {
                    let field = format!("extensions.{OPENWAVE_EXTENSION_NAMESPACE}.{key}");
                    match key.as_str() {
                        "category" => match value.as_str().and_then(PluginCategory::parse) {
                            Some(parsed) => category = Some(parsed),
                            None => ignored.push(IgnoredManifestField {
                                field,
                                reason: "value is not one of the supported categories".to_owned(),
                            }),
                        },
                        "router-preamble" => match value.as_str() {
                            Some(preamble) if is_valid_plugin_router_preamble(preamble) => {
                                router_preamble = Some(preamble.to_owned());
                            }
                            _ => ignored.push(IgnoredManifestField {
                                field,
                                reason: "value is not one bounded printable line".to_owned(),
                            }),
                        },
                        _ => ignored.push(IgnoredManifestField {
                            field,
                            reason: "field is not read by this client".to_owned(),
                        }),
                    }
                }
            }
        }
    }

    Ok(ParsedAgentPluginManifest {
        manifest: AgentPluginManifest {
            name: name.to_owned(),
            description: object
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            category,
            router_preamble,
        },
        ignored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(body: &str) -> String {
        format!("{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA_ID}\", \"name\": \"reporting\"{body}}}")
    }

    /// Contract: the two documented non-fatal violations keep the plugin
    /// loadable, every other schema violation rejects it, and metadata the
    /// specification only type-checks is never second-guessed.
    #[test]
    fn manifest_violations_are_graded_the_way_the_specification_grades_them() {
        let parsed = parse_agent_plugin_manifest(&manifest(
            ", \"version\": \"not.a.semver-at-all\", \"license\": \"whatever\", \
             \"homepage\": \"not a url\", \"keywords\": [\"a\"], \
             \"author\": {\"name\": \"A\"}, \"surprise\": 1, \"extensions\": 7",
        ))
        .expect("non-fatal violations keep the plugin loadable");
        assert_eq!(
            parsed
                .ignored
                .iter()
                .map(|entry| entry.field.as_str())
                .collect::<Vec<_>>(),
            ["surprise", "extensions"]
        );

        for (case, source) in [
            ("missing $schema", "{\"name\": \"reporting\"}".to_owned()),
            (
                "unsupported $schema",
                "{\"$schema\": \"https://agent-plugins.org/schemas/9.9.9/plugin.schema.json\", \
                 \"name\": \"reporting\"}"
                    .to_owned(),
            ),
            (
                "missing name",
                format!("{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA_ID}\"}}"),
            ),
            ("mistyped description", manifest(", \"description\": 5")),
            ("mistyped keywords", manifest(", \"keywords\": \"a\"")),
            (
                "unknown author field",
                manifest(", \"author\": {\"handle\": \"a\"}"),
            ),
            (
                "non-object namespace",
                manifest(", \"extensions\": {\"com.example\": 3}"),
            ),
            ("not an object", "[]".to_owned()),
        ] {
            assert!(
                parse_agent_plugin_manifest(&source).is_err(),
                "{case} should reject the plugin"
            );
        }

        for name in ["a", "read.me", "a-b.c-9"] {
            assert!(is_valid_agent_plugin_name(name), "{name} should be valid");
        }
        for name in ["", "-a", "a-", "a--b", "a..b", "A", "a_b", &"a".repeat(65)] {
            assert!(
                !is_valid_agent_plugin_name(name),
                "{name} should be invalid"
            );
        }
    }

    /// Contract: our own namespace is read, other namespaces are passed over
    /// untouched, and a value we cannot use inside our namespace is reported
    /// and ignored rather than sinking a package that is valid for everyone
    /// else.
    #[test]
    fn the_openwave_extension_namespace_is_read_and_others_are_left_alone() {
        let parsed = parse_agent_plugin_manifest(&manifest(&format!(
            ", \"extensions\": {{\
               \"com.example.client\": {{\"anything\": [1, 2]}}, \
               \"{OPENWAVE_EXTENSION_NAMESPACE}\": {{\
                 \"category\": \"data\", \
                 \"router-preamble\": \"Pick by the report the user asked for.\"}}}}"
        )))
        .unwrap();
        assert!(parsed.ignored.is_empty());
        assert_eq!(parsed.manifest.category, Some(PluginCategory::Data));
        assert_eq!(
            parsed.manifest.router_preamble.as_deref(),
            Some("Pick by the report the user asked for.")
        );

        let parsed = parse_agent_plugin_manifest(&manifest(&format!(
            ", \"extensions\": {{\"{OPENWAVE_EXTENSION_NAMESPACE}\": {{\
               \"category\": \"wizardry\", \"router-preamble\": \"\", \"future\": 1}}}}"
        )))
        .expect("a value we cannot use is not fatal");
        assert_eq!(parsed.manifest.category, None);
        assert_eq!(parsed.manifest.router_preamble, None);
        assert_eq!(parsed.ignored.len(), 3);
    }
}
