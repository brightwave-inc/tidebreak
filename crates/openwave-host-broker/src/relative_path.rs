//! OS-independent, validated paths relative to a registered root.

use std::fmt;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Why a requested root-relative path was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RelativePathError {
    /// Paths are bounded before filesystem or audit processing.
    #[error("path exceeds its size limit")]
    TooLong,
    /// Absolute paths are never part of the agent-facing protocol.
    #[error("path must be relative to a registered root")]
    Absolute,
    /// Parent components are refused rather than normalized across authority.
    #[error("path must not contain parent traversal")]
    ParentTraversal,
    /// Backslashes, colons, and NUL have platform-dependent or unsafe meaning.
    #[error("path contains a character outside the portable path grammar")]
    NonPortableCharacter,
    /// Windows normalizes these names or routes them to device namespaces.
    #[error("path contains a non-portable filename")]
    NonPortableFilename,
}

/// A normalized path with one meaning on every supported host.
///
/// `/` is the only separator in the wire grammar. Empty and `.` components are
/// removed; the root itself is represented as `.`. Backslashes, colons, parent
/// traversal, absolute paths, Windows device names, and filenames Windows would
/// normalize are rejected even on Unix. Invalid wire data therefore cannot
/// instantiate this type and later change meaning on another host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelativePath(String);

impl RelativePath {
    /// Validate and normalize an untrusted protocol path.
    pub fn parse(input: &str) -> Result<Self, RelativePathError> {
        if input.len() > 1024 {
            return Err(RelativePathError::TooLong);
        }
        if input.starts_with('/') {
            return Err(RelativePathError::Absolute);
        }
        if input.contains(['\0', '\\', ':']) {
            return Err(RelativePathError::NonPortableCharacter);
        }

        let mut segments = Vec::new();
        for segment in input.split('/') {
            match segment {
                "" | "." => {}
                ".." => return Err(RelativePathError::ParentTraversal),
                value if !portable_filename(value) => {
                    return Err(RelativePathError::NonPortableFilename);
                }
                value => segments.push(value),
            }
        }

        Ok(Self(if segments.is_empty() {
            ".".to_owned()
        } else {
            segments.join("/")
        }))
    }

    /// A path denoting the registered root itself.
    pub fn root() -> Self {
        Self(".".to_owned())
    }

    /// Stable slash-separated representation used by the broker protocol.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this path denotes the registered root itself.
    pub fn is_root(&self) -> bool {
        self.0 == "."
    }

    pub(crate) fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|segment| *segment != ".")
    }
}

fn portable_filename(value: &str) -> bool {
    if value.ends_with(['.', ' '])
        || value.chars().any(|character| {
            character <= '\u{1f}' || matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
        })
    {
        return false;
    }
    let stem = value.split('.').next().unwrap_or_default();
    let upper = stem.to_ascii_uppercase();
    !matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) && !matches_device_number(&upper, "COM")
        && !matches_device_number(&upper, "LPT")
}

fn matches_device_number(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    })
}

impl fmt::Display for RelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<&str> for RelativePath {
    type Error = RelativePathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl Serialize for RelativePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RelativePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_relative_paths_to_a_stable_wire_form() {
        assert_eq!(RelativePath::parse("").unwrap().as_str(), ".");
        assert_eq!(RelativePath::parse("./a//b").unwrap().as_str(), "a/b");
    }

    #[test]
    fn rejects_absolute_parent_and_nonportable_paths_on_every_host() {
        assert!(matches!(
            RelativePath::parse(&"a".repeat(1025)),
            Err(RelativePathError::TooLong)
        ));
        for absolute in ["/etc/passwd", "\\server\\share", "C:/Windows"] {
            assert!(RelativePath::parse(absolute).is_err(), "{absolute}");
        }
        for traversal in ["../secret", "safe/../secret", "..\\secret"] {
            assert!(RelativePath::parse(traversal).is_err(), "{traversal}");
        }
        for nonportable in [
            "C:relative",
            "mixed\\separator/file",
            "file:stream",
            "bad\0name",
            "CON",
            "aux.txt",
            "LPT9.log",
            "CONIN$",
            "conout$.txt",
            "CLOCK$",
            "COM¹",
            "lpt².log",
            "trailing.",
            "trailing ",
            "question?.txt",
            "control\u{1f}.txt",
        ] {
            assert!(RelativePath::parse(nonportable).is_err(), "{nonportable}");
        }
    }

    #[test]
    fn deserialization_cannot_bypass_validation() {
        for json in [
            r#""../secret""#,
            r#""..\\secret""#,
            r#""C:\\Windows""#,
            r#""\\\\server\\share""#,
            r#""mixed/..\\secret""#,
        ] {
            assert!(
                serde_json::from_str::<RelativePath>(json).is_err(),
                "{json}"
            );
        }
    }
}
