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
