//! Shared pieces for building macOS Seatbelt (`sandbox-exec`) profiles.
//!
//! Two very different processes are confined with the same machinery: the
//! model's own commands ([`crate::local`]) and the host-side LibreOffice that
//! renders office outputs to PDF (in the desktop crate). The profiles differ —
//! one is a workspace the model writes in, the other a converter that reads a
//! bundle and writes one temp directory — but the path-escaping rules and the
//! sandbox binary are the same, and getting the escaping wrong is a sandbox
//! escape in either. Hence one module, not two conventions.

use std::path::Path;

/// The system sandbox launcher. Present on every supported macOS host; its
/// absence is reported rather than silently skipped, so an unconfined process
/// never runs by accident.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// A path that cannot be represented safely inside a profile. Callers turn
/// this into their own sandbox-failure state; it is never a reason to run
/// without the sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsafeSandboxPath {
    NotUtf8,
    ControlCharacters,
}

impl std::fmt::Display for UnsafeSandboxPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotUtf8 => write!(f, "sandbox paths must be valid UTF-8"),
            Self::ControlCharacters => write!(f, "sandbox paths cannot contain control characters"),
        }
    }
}

impl std::error::Error for UnsafeSandboxPath {}

/// `(literal "…")` for a known-good static path.
pub fn literal_str(path: &str) -> String {
    format!("(literal \"{}\")", escape(path))
}

/// `(subpath "…")` for a known-good static path.
pub fn subpath_str(path: &str) -> String {
    format!("(subpath \"{}\")", escape(path))
}

/// `(literal "…")` for a host-resolved path, rejecting anything that cannot be
/// escaped into a profile faithfully.
pub fn literal(path: &Path) -> Result<String, UnsafeSandboxPath> {
    Ok(literal_str(checked(path)?))
}

/// `(subpath "…")` for a host-resolved path, with the same rejection.
pub fn subpath(path: &Path) -> Result<String, UnsafeSandboxPath> {
    Ok(subpath_str(checked(path)?))
}

fn checked(path: &Path) -> Result<&str, UnsafeSandboxPath> {
    let path = path.to_str().ok_or(UnsafeSandboxPath::NotUtf8)?;
    if path.chars().any(char::is_control) {
        return Err(UnsafeSandboxPath::ControlCharacters);
    }
    Ok(path)
}

/// Escaping for an SBPL string literal: backslash first, then the quote that
/// would otherwise end the literal and let a crafted path append clauses.
pub fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_quotes_and_backslashes_without_changing_other_characters() {
        for (input, expected) in [
            ("", ""),
            ("/tmp/a folder/資料", "/tmp/a folder/資料"),
            (r#"/tmp/a"b\c"#, r#"/tmp/a\"b\\c"#),
            (r#"\""#, r#"\\\""#),
            (r#"\\"#, r#"\\\\"#),
        ] {
            assert_eq!(escape(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn path_clauses_keep_injected_profile_rules_inside_the_string() {
        let path = r#"/tmp/") (allow default) ;\"#;
        let expected_literal = r#"(literal "/tmp/\") (allow default) ;\\")"#;
        let expected_subpath = r#"(subpath "/tmp/\") (allow default) ;\\")"#;
        assert_eq!(literal_str(path), expected_literal);
        assert_eq!(subpath_str(path), expected_subpath);
        assert_eq!(literal(Path::new(path)).unwrap(), expected_literal);
        assert_eq!(subpath(Path::new(path)).unwrap(), expected_subpath);
    }

    #[test]
    fn resolved_paths_reject_control_characters() {
        // C0, DEL, and C1 controls include newlines and NUL. Escaping quotes
        // must never admit a path that the profile parser interprets differently.
        for control in (0..=0x1f).chain(0x7f..=0x9f) {
            let path = format!("/tmp/before{}after", char::from_u32(control).unwrap());
            for result in [literal(Path::new(&path)), subpath(Path::new(&path))] {
                assert_eq!(result, Err(UnsafeSandboxPath::ControlCharacters));
            }
        }
    }

    #[test]
    fn resolved_paths_preserve_unicode_and_spaces() {
        let path = Path::new("/tmp/a folder/資料");
        assert_eq!(literal(path).unwrap(), r#"(literal "/tmp/a folder/資料")"#);
        assert_eq!(subpath(path).unwrap(), r#"(subpath "/tmp/a folder/資料")"#);
    }

    #[cfg(unix)]
    #[test]
    fn resolved_paths_reject_non_utf8_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/tmp/\xff"));
        assert_eq!(literal(path), Err(UnsafeSandboxPath::NotUtf8));
        assert_eq!(subpath(path), Err(UnsafeSandboxPath::NotUtf8));
    }

    #[cfg(windows)]
    #[test]
    fn resolved_paths_reject_unpaired_utf16_surrogates() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let path = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]);
        assert_eq!(literal(Path::new(&path)), Err(UnsafeSandboxPath::NotUtf8));
        assert_eq!(subpath(Path::new(&path)), Err(UnsafeSandboxPath::NotUtf8));
    }
}
