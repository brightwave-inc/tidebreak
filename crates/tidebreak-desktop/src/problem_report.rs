//! The native half of Report a problem: the facts an issue is prefilled
//! with, and the logs folder Help → Show Logs opens.
//!
//! The report itself is the diagnostics bundle `unclean_exit` saves. The
//! issue carries only what identifies the build: its version, the operating
//! system, and the architecture. Nothing from the logs or a conversation is
//! ever put in the issue's address, because the address travels to GitHub
//! before the person has read it.

use serde::Serialize;
use tauri::AppHandle;

/// What a new issue is prefilled with, and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProblemReportFacts {
    /// This build's version, e.g. `0.117.0`.
    version: String,
    /// The operating system and its version, e.g. `macOS 15.6`.
    os: String,
    /// The processor architecture, e.g. `arm64`.
    arch: String,
}

/// The version, operating system, and architecture a bug report names.
#[tauri::command]
pub(crate) fn problem_report_facts(app: AppHandle) -> ProblemReportFacts {
    ProblemReportFacts {
        version: app.package_info().version.to_string(),
        os: os_label(std::env::consts::OS, os_version().as_deref()),
        arch: arch_label(std::env::consts::ARCH).to_owned(),
    }
}

/// Open this computer's Tidebreak logs folder in its file manager. The logs
/// are this computer's even while the window works on another machine: the
/// shell and its embedded server write them here either way.
#[tauri::command]
pub(crate) async fn reveal_logs_directory(app: AppHandle) -> Result<(), String> {
    const OPEN_FAILED: &str = "Tidebreak could not open the logs folder.";
    let logs = crate::data_dir(&app)?.join("logs");
    std::fs::create_dir_all(&logs).map_err(|_| OPEN_FAILED.to_owned())?;
    crate::code_worktree::open_directory(logs)
        .map(|_| ())
        .map_err(|_| OPEN_FAILED.to_owned())
}

/// The system's name as people write it, with its version when known.
fn os_label(os: &str, version: Option<&str>) -> String {
    let name = match os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    match version.map(str::trim).filter(|version| !version.is_empty()) {
        Some(version) => format!("{name} {version}"),
        None => name.to_owned(),
    }
}

/// The architecture as the release assets and Apple name it.
fn arch_label(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        other => other,
    }
}

/// The macOS product version, e.g. `15.6`.
#[cfg(target_os = "macos")]
fn os_version() -> Option<String> {
    let name = c"kern.osproductversion";
    let mut size: libc::size_t = 0;
    // SAFETY: a null buffer asks sysctl for the value's length only.
    let asked = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if asked != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0_u8; size];
    // SAFETY: `buffer` holds `size` writable bytes, and sysctl writes at most
    // that many, updating `size` to what it wrote.
    let read = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buffer.truncate(size);
    std::ffi::CStr::from_bytes_until_nul(&buffer)
        .ok()?
        .to_str()
        .ok()
        .map(str::to_owned)
}

#[cfg(not(target_os = "macos"))]
fn os_version() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_system_the_way_people_write_it() {
        assert_eq!(os_label("macos", Some("15.6")), "macOS 15.6");
        assert_eq!(os_label("macos", Some("  ")), "macOS");
        assert_eq!(os_label("windows", None), "Windows");
        assert_eq!(os_label("freebsd", None), "freebsd");
        assert_eq!(arch_label("aarch64"), "arm64");
        assert_eq!(arch_label("x86_64"), "x86_64");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_the_running_macos_version() {
        let version = os_version().expect("macOS reports its product version");
        assert!(
            version.split('.').all(|part| part.parse::<u32>().is_ok()),
            "{version}"
        );
    }

    /// The facts serialize as exactly three fields, so nothing else can ride
    /// into the issue's prefill by way of this struct.
    #[test]
    fn the_facts_are_the_version_the_system_and_the_architecture() {
        let facts = ProblemReportFacts {
            version: "0.117.0".to_owned(),
            os: "macOS 15.6".to_owned(),
            arch: "arm64".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(facts).unwrap(),
            serde_json::json!({"version": "0.117.0", "os": "macOS 15.6", "arch": "arm64"})
        );
    }
}
