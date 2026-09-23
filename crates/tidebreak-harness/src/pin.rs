//! Pinned harness installs under the Tidebreak data directory.
//!
//! Each engine is an exact npm package version. Probe and launch use that
//! copy. The user's PATH is not the engine (decision 0041).
//!
//! The pin is the floor, not the only version. A reader on the `latest`
//! update channel installs whatever the registry publishes, into a sibling
//! directory keyed by that exact version; the marker inside says which one it
//! is, so nothing on disk is trusted by being newest. See
//! [`latest_published_version`] and [`ensure_installed_version`].

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tidebreak_core::HarnessKind;
use tidebreak_managed_node::{
    managed_node_executable, managed_node_path_dir, managed_npm_executable,
};
use tokio::process::Command;
use tokio::time::timeout;

use crate::{is_absolute_executable, spawn_process_tree};

const INSTALL_TIMEOUT: Duration = Duration::from_secs(180);

/// A registry lookup is one small HTTP request, but npm still resolves the
/// registry, reads its config, and may wait on a slow network.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(60);

/// One exact harness pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HarnessPin {
    /// Engine this pin belongs to.
    pub kind: HarnessKind,
    /// Exact package version.
    pub version: &'static str,
    /// npm package name.
    pub package: &'static str,
    /// Binary name inside `node_modules/.bin`.
    pub bin: &'static str,
    /// Arguments after [`Self::bin`] that start this version's own sign-in
    /// flow. Each one is checked against the pinned binary's captured
    /// `--help` (`fixtures/<engine>/<version>/sign-in-help.txt`), so a pin
    /// bump that moves the command fails a test instead of a person.
    pub sign_in: &'static [&'static str],
}

/// Current install pins. They do not identify complete fixture captures.
/// Replay baselines and later protocol-specific captures live in
/// `fixtures/README.md`; keep that coverage table accurate when changing a pin.
pub const PINS: &[HarnessPin] = &[
    // Replay baseline: 2.1.233; task-line captures: 2.1.259; 2.1.238/239 process
    // observations are manifest notes.
    // `claude login` is not a command: it starts a session with "login" as
    // the prompt. Sign-in lives under `auth`.
    HarnessPin {
        kind: HarnessKind::ClaudeCode,
        version: "2.1.259",
        package: "@anthropic-ai/claude-code",
        bin: "claude",
        sign_in: &["auth", "login"],
    },
    // Replay baseline: 0.147.0; MCP elicitation captures: 0.153.0; item and
    // resume captures: 0.153.4.
    HarnessPin {
        kind: HarnessKind::Codex,
        version: "0.153.4",
        package: "@openai/codex",
        bin: "codex",
        sign_in: &["login"],
    },
    // Replay baseline: 1.18.18; no complete 1.18.27 capture.
    HarnessPin {
        kind: HarnessKind::Opencode,
        version: "1.18.27",
        package: "opencode-ai",
        bin: "opencode",
        sign_in: &["auth", "login"],
    },
    // Replay baselines: 1.0.4/5; 1.0.13 covers ACP, tool images, and plan updates.
    HarnessPin {
        kind: HarnessKind::Grok,
        version: "1.0.13",
        package: "@xai-official/grok",
        bin: "grok",
        sign_in: &["login"],
    },
];

/// The pin for a kind, if we ship one.
#[must_use]
pub fn pin_for(kind: HarnessKind) -> Option<&'static HarnessPin> {
    PINS.iter().find(|pin| pin.kind == kind)
}

/// The release each engine's hand-kept capability tables were last checked
/// against.
///
/// Tables drift: the Grok effort picker broke twice when a later release
/// kept an older release's table. So an adapter reads a capability from the
/// engine at probe time wherever the engine states it, and keeps a table only
/// where it does not. A pin bump fails
/// `capability_tables_were_reviewed_for_every_pin` until someone checks the
/// tables named here against the new release and moves its entry. It sits
/// beside [`PINS`] so a pin bump sees it, and only the test reads it.
#[cfg(test)]
pub(crate) const TABLES_REVIEWED_AT: &[(HarnessKind, &str)] = &[
    // `claude::EFFORT_LADDER`, the fallback when `claude --help` lists no
    // `--effort` choices, and the fast-mode ids in
    // `claude::model_serves_fast_mode`, which the engine states nowhere.
    (HarnessKind::ClaudeCode, "2.1.259"),
    // None: `model/list` states each model's effort ladder and fast tier.
    (HarnessKind::Codex, "0.153.4"),
    // None: this pin takes no effort control.
    (HarnessKind::Opencode, "1.18.27"),
    // `grok::EFFORT_LADDER_*`, the `--reasoning-effort` vocabulary per
    // release, and `grok::GROK_MODEL_EFFORTS`, the fallback for a model the
    // ACP `initialize` answer does not list.
    (HarnessKind::Grok, "1.0.13"),
];

/// The arguments that start `kind`'s own sign-in, for the pinned binary.
///
/// `None` for an engine Tidebreak ships no pin for, which has nothing to
/// sign in to. A release installed on the `latest` update channel is
/// assumed to keep the pin's command, the same way it keeps the pin's
/// capability flags until someone captures otherwise.
#[must_use]
pub fn sign_in_args(kind: HarnessKind) -> Option<&'static [&'static str]> {
    pin_for(kind)
        .map(|pin| pin.sign_in)
        .filter(|args| !args.is_empty())
}

/// The sign-in command the way a person types it: `claude auth login`.
#[must_use]
pub fn sign_in_command(kind: HarnessKind) -> Option<String> {
    let pin = pin_for(kind)?;
    let args = sign_in_args(kind)?;
    Some(
        std::iter::once(pin.bin)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The directory holding the managed engine binary, so an embedded terminal
/// can put it on `PATH`.
///
/// `version` names the install the update channel drives; `None` is the pin.
/// An install whose marker does not match is not a directory to trust.
#[must_use]
pub fn managed_bin_dir(
    data_dir: &Path,
    kind: HarnessKind,
    version: Option<&str>,
) -> Option<PathBuf> {
    let binary = match version {
        Some(version) => managed_binary_version(data_dir, kind, version),
        None => managed_binary(data_dir, kind),
    }?;
    binary.parent().map(Path::to_path_buf)
}

/// `{data_dir}/tools/harnesses/{kind}/{version}` for the pin itself.
#[must_use]
pub fn install_dir(data_dir: &Path, pin: &HarnessPin) -> PathBuf {
    install_dir_for(data_dir, pin, pin.version)
}

/// `{data_dir}/tools/harnesses/{kind}/{version}` for any exact version of
/// the pin's package.
#[must_use]
pub fn install_dir_for(data_dir: &Path, pin: &HarnessPin, version: &str) -> PathBuf {
    versions_dir(data_dir, pin.kind).join(version)
}

/// `{data_dir}/tools/harnesses/{kind}`: one subdirectory per installed
/// version.
fn versions_dir(data_dir: &Path, kind: HarnessKind) -> PathBuf {
    data_dir.join("tools").join("harnesses").join(kind.as_str())
}

fn marker_path(dir: &Path) -> PathBuf {
    dir.join("installed.json")
}

fn binary_path(dir: &Path, pin: &HarnessPin) -> PathBuf {
    let name = if cfg!(windows) {
        format!("{}.cmd", pin.bin)
    } else {
        pin.bin.to_owned()
    };
    dir.join("node_modules").join(".bin").join(name)
}

/// The Grok home that belongs to one Grok install, beside its `node_modules`.
///
/// Grok's npm entrypoint does not run the binary it ships with. It runs
/// `$GROK_HOME/bin/grok` when that file exists, and Grok's own updater keeps
/// that file current, so the person's `~/.grok/bin/grok` can be any release:
/// on one machine the 1.0.13 pin ran 1.0.40. Only when the file is missing
/// does the entrypoint unpack its own binary, as `bin/grok-<version>`. Run
/// with this directory as `GROK_HOME`, it unpacks the pinned binary here,
/// and Tidebreak then runs that binary directly.
///
/// This home holds nothing but the binary. Sessions, the probe, and sign-in
/// keep the person's own `GROK_HOME`, because Grok keeps sign-in
/// (`auth.json`), settings (`config.toml`), and session history there.
fn grok_home(dir: &Path) -> PathBuf {
    dir.join("grok-home")
}

/// The pinned Grok binary the npm entrypoint unpacks into [`grok_home`].
fn grok_native_binary(dir: &Path, version: &str) -> PathBuf {
    let name = if cfg!(windows) {
        format!("grok-{version}.exe")
    } else {
        format!("grok-{version}")
    };
    grok_home(dir).join("bin").join(name)
}

/// The file Tidebreak runs for one installed version: the npm entrypoint,
/// except for Grok, whose entrypoint may run another release (see
/// [`grok_home`]).
fn engine_binary(dir: &Path, pin: &HarnessPin, version: &str) -> PathBuf {
    match pin.kind {
        HarnessKind::Grok => grok_native_binary(dir, version),
        _ => binary_path(dir, pin),
    }
}

/// Whether npm installed this exact version of the pinned package here: the
/// marker names it and the entrypoint is executable.
fn installed_tree(dir: &Path, pin: &HarnessPin, version: &str) -> bool {
    let Some(marker) = std::fs::read_to_string(marker_path(dir))
        .ok()
        .and_then(|text| serde_json::from_str::<InstallMarker>(&text).ok())
    else {
        return false;
    };
    marker.package == pin.package
        && marker.version == version
        && is_absolute_executable(&binary_path(dir, pin))
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct InstallMarker {
    package: String,
    version: String,
}

/// The managed binary for this pin, if the marker matches and the file exists.
#[must_use]
pub fn managed_binary(data_dir: &Path, kind: HarnessKind) -> Option<PathBuf> {
    let pin = pin_for(kind)?;
    managed_binary_version(data_dir, kind, pin.version)
}

/// The managed binary for one exact version of `kind`, if its marker names
/// that version of the pinned package and the file exists.
///
/// For Grok the file is the binary unpacked into the install's own Grok home
/// ([`grok_home`]). An install from before that home existed has none until
/// [`ensure_installed_version`] or [`unpack_installed_binary`] unpacks it.
#[must_use]
pub fn managed_binary_version(
    data_dir: &Path,
    kind: HarnessKind,
    version: &str,
) -> Option<PathBuf> {
    let pin = pin_for(kind)?;
    let dir = install_dir_for(data_dir, pin, version);
    if !installed_tree(&dir, pin, version) {
        return None;
    }
    let binary = engine_binary(&dir, pin, version);
    is_absolute_executable(&binary).then_some(binary)
}

/// Where the managed binary for one exact version of `kind` lives, whether or
/// not anything is installed there yet. [`managed_binary_version`] answers
/// whether it is ready to run.
#[must_use]
pub fn managed_binary_path(data_dir: &Path, kind: HarnessKind, version: &str) -> Option<PathBuf> {
    let pin = pin_for(kind)?;
    Some(engine_binary(
        &install_dir_for(data_dir, pin, version),
        pin,
        version,
    ))
}

/// Every version of `kind` installed under the data directory, newest first.
///
/// A directory counts only when its marker names the pinned package at the
/// version the directory is named for and the binary inside is executable —
/// the same bar [`managed_binary_version`] holds one directory to. A torn or
/// foreign tree is not a version.
#[must_use]
pub fn installed_versions(data_dir: &Path, kind: HarnessKind) -> Vec<String> {
    installed(data_dir, kind, |version| {
        managed_binary_version(data_dir, kind, version).is_some()
    })
}

/// Every version of `kind` npm installed under the data directory, newest
/// first, whether or not its binary is ready to run.
///
/// [`installed_versions`] leaves out a Grok install whose binary is not
/// unpacked yet: one from before Tidebreak unpacked it. Removing superseded
/// installs reads this list instead, so such an install still goes.
#[must_use]
pub fn installed_trees(data_dir: &Path, kind: HarnessKind) -> Vec<String> {
    let Some(pin) = pin_for(kind) else {
        return Vec::new();
    };
    installed(data_dir, kind, |version| {
        installed_tree(&install_dir_for(data_dir, pin, version), pin, version)
    })
}

fn installed(data_dir: &Path, kind: HarnessKind, counts: impl Fn(&str) -> bool) -> Vec<String> {
    let Some(pin) = pin_for(kind) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(versions_dir(data_dir, pin.kind)) else {
        return Vec::new();
    };
    let mut versions: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|version| counts(version))
        .collect();
    versions.sort_by(|a, b| compare_versions(b, a));
    versions
}

/// Order two package versions.
///
/// Dotted numeric components compare as numbers, so `2.1.251` is newer than
/// `2.1.99`. A release without a pre-release suffix is newer than the same
/// release with one; two suffixes compare as text. Good enough for the
/// registries the pins come from, without a semver dependency.
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (a_core, a_rest) = split_version(a);
    let (b_core, b_rest) = split_version(b);
    let len = a_core.len().max(b_core.len());
    for index in 0..len {
        let left = a_core.get(index).copied().unwrap_or(0);
        let right = b_core.get(index).copied().unwrap_or(0);
        match left.cmp(&right) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    match (a_rest, b_rest) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => left.cmp(right),
    }
}

/// The numeric core of a version and its pre-release suffix, if any.
fn split_version(version: &str) -> (Vec<u64>, Option<&str>) {
    let version = version.trim().trim_start_matches('v');
    let end = version
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(version.len());
    let core = version[..end]
        .split('.')
        .filter_map(|part| part.parse::<u64>().ok())
        .collect();
    let rest = version[end..].trim_start_matches(['-', '+']);
    (core, (!rest.is_empty()).then_some(rest))
}

/// Whether `candidate` is a package version the registry could have
/// published: something npm would accept in `pkg@version`, and nothing a
/// shell or a path would read differently.
fn is_plausible_version(candidate: &str) -> bool {
    let mut chars = candidate.chars();
    chars.next().is_some_and(|first| first.is_ascii_digit())
        && candidate
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'))
        && candidate.len() <= 64
}

/// Ask the registry which version `kind`'s package publishes as `latest`.
///
/// Runs `npm view <package> dist-tags.latest` under the host-verified managed
/// Node runtime, the same npm the install uses, so the answer names a version
/// that install can fetch. Never runs on a listing path: a lookup is a
/// network round trip, and the doctor's Check for updates and a deliberate
/// install are the only callers.
pub async fn latest_published_version(
    data_dir: &Path,
    kind: HarnessKind,
    managed_node_root: Option<&Path>,
) -> Result<String, String> {
    let node_root = verified_managed_node_root(managed_node_root)
        .ok_or_else(|| "install the managed Node runtime before checking for updates".to_owned())?;
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    let mut command = npm_command(data_dir, node_root);
    command.args([
        "view",
        "--no-fund",
        "--no-audit",
        "--no-update-notifier",
        pin.package,
        "dist-tags.latest",
    ]);
    let child = spawn_process_tree(&mut command)
        .map_err(|err| format!("npm view {}: {err}", pin.package))?;
    let output = timeout(LOOKUP_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("npm view {} timed out", pin.package))?
        .map_err(|err| format!("npm view {}: {err}", pin.package))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "npm view {} failed: {}",
            pin.package,
            stderr.lines().last().unwrap_or("unknown error")
        ));
    }
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !is_plausible_version(&version) {
        return Err(format!(
            "npm view {} answered with something other than a version",
            pin.package
        ));
    }
    Ok(version)
}

/// One lock per install directory, so two callers cannot npm-install into it
/// at once.
///
/// A pin now has two installers — the warm install the New Workspace dialog
/// starts, and the create path's own fallback — and npm has no guard of its
/// own for two processes writing one `node_modules`. The marker is written
/// only after a successful install, so a torn tree left by an interleaved run
/// would be blessed by the next marker write rather than reinstalled. Keying
/// on the directory rather than the kind keeps two data directories (tests,
/// profiles) from serializing against each other.
fn install_lock(dir: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("harness install locks")
        .entry(dir.to_path_buf())
        .or_default()
        .clone()
}

/// Install the pin with the host-verified managed Node runtime's npm if it is
/// not present.
///
/// The caller owns verification of `managed_node_root` (for the desktop this
/// is the digest-gated host-tool broker). This crate deliberately does not
/// scan `{data_dir}/tools/node`: a sibling directory that merely looks like a
/// Node install must never become executable code by being newest on disk.
///
/// Concurrent calls for one pin are serialized: the first installs and the
/// rest return the binary it produced.
pub async fn ensure_installed(
    data_dir: &Path,
    kind: HarnessKind,
    managed_node_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    ensure_installed_version(data_dir, kind, pin.version, managed_node_root).await
}

/// Install one exact version of `kind`'s package, if it is not present.
///
/// The `latest` update channel lands here with the version the registry
/// named; the pinned channel lands here through [`ensure_installed`]. Same
/// lock, same marker, same verified npm: a newer version is held to exactly
/// the bar the pin is.
pub async fn ensure_installed_version(
    data_dir: &Path,
    kind: HarnessKind,
    version: &str,
    managed_node_root: Option<&Path>,
) -> Result<PathBuf, String> {
    let node_root = verified_managed_node_root(managed_node_root)
        .ok_or_else(|| "install the managed Node runtime before pinning harnesses".to_owned())?;
    if !is_plausible_version(version) {
        return Err(format!("{version:?} is not a package version"));
    }
    if let Some(existing) = managed_binary_version(data_dir, kind, version) {
        return Ok(existing);
    }
    let pin = pin_for(kind).ok_or_else(|| format!("{kind} has no pin"))?;
    let dir = install_dir_for(data_dir, pin, version);
    let lock = install_lock(&dir);
    let _guard = lock.lock().await;
    // Another caller may have installed this version while this one waited.
    if let Some(existing) = managed_binary_version(data_dir, kind, version) {
        return Ok(existing);
    }
    let spec = format!("{}@{}", pin.package, version);
    // A tree npm already installed only lacks the unpacked Grok binary: an
    // install from before Tidebreak unpacked it. It needs no second download
    // unless the unpack fails on it, when the tree itself may be what is
    // missing a piece.
    let fresh = !installed_tree(&dir, pin, version);
    if fresh {
        npm_install(data_dir, node_root, pin, &dir, version).await?;
    }
    if pin.kind == HarnessKind::Grok {
        if let Err(error) = unpack_grok(&dir, pin, version, node_root).await {
            if fresh {
                return Err(error);
            }
            tracing::warn!(%error, "could not unpack Grok from its installed tree; installing it again");
            npm_install(data_dir, node_root, pin, &dir, version).await?;
            unpack_grok(&dir, pin, version, node_root).await?;
        }
    }
    managed_binary_version(data_dir, kind, version).ok_or_else(|| {
        format!(
            "npm install {spec} finished but {} was not executable",
            pin.bin
        )
    })
}

async fn npm_install(
    data_dir: &Path,
    node_root: &Path,
    pin: &HarnessPin,
    dir: &Path,
    version: &str,
) -> Result<(), String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|err| format!("could not create harness install dir: {err}"))?;
    let spec = format!("{}@{}", pin.package, version);
    let mut command = npm_command(data_dir, node_root);
    command
        .args([
            "install",
            "--omit=dev",
            "--no-fund",
            "--no-audit",
            "--no-progress",
            &spec,
        ])
        .current_dir(dir);
    if pin.kind == HarnessKind::Grok {
        // Grok's install script unpacks its binary into `$GROK_HOME/bin` and
        // writes `$GROK_HOME/config.toml`. Aimed at the install's own home,
        // it leaves the person's `~/.grok` alone.
        command.env("GROK_HOME", grok_home(dir));
    }
    let child =
        spawn_process_tree(&mut command).map_err(|err| format!("npm install {spec}: {err}"))?;
    let output = timeout(INSTALL_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("npm install {spec} timed out"))?
        .map_err(|err| format!("npm install {spec}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "npm install {spec} failed: {}",
            stderr.lines().last().unwrap_or("unknown error")
        ));
    }
    let marker = InstallMarker {
        package: pin.package.to_owned(),
        version: version.to_owned(),
    };
    tokio::fs::write(
        marker_path(dir),
        serde_json::to_vec_pretty(&marker).map_err(|err| err.to_string())?,
    )
    .await
    .map_err(|err| format!("could not write harness install marker: {err}"))
}

/// Unpack the installed Grok binary into the install's own Grok home, when
/// it is not there yet.
///
/// Grok's npm entrypoint unpacks its bundled binary when `$GROK_HOME/bin/grok`
/// is missing, so this runs it once with [`grok_home`] as `GROK_HOME`. Its
/// answer to `--version` has to name `version`: anything else means the
/// entrypoint ran a binary this install did not ship.
async fn unpack_grok(
    dir: &Path,
    pin: &HarnessPin,
    version: &str,
    node_root: &Path,
) -> Result<(), String> {
    if is_absolute_executable(&grok_native_binary(dir, version)) {
        return Ok(());
    }
    let home = grok_home(dir);
    tokio::fs::create_dir_all(&home)
        .await
        .map_err(|err| format!("could not create the Grok home: {err}"))?;
    let mut command = Command::new(binary_path(dir, pin));
    command
        .arg("--version")
        .current_dir(dir)
        .env_clear()
        .env("PATH", prepend_path(&managed_node_path_dir(node_root)))
        // The entrypoint and the binary it starts both read these; nothing
        // here may reach the person's own home.
        .env("GROK_HOME", &home)
        .env("HOME", &home)
        .env(crate::grok::DISABLE_AUTOUPDATER_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    keep_windows_start_env(&mut command);
    let child = spawn_process_tree(&mut command)
        .map_err(|err| format!("could not unpack Grok {version}: {err}"))?;
    let output = timeout(LOOKUP_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("unpacking Grok {version} timed out"))?
        .map_err(|err| format!("could not unpack Grok {version}: {err}"))?;
    let reported = String::from_utf8_lossy(&output.stdout);
    let reported = reported
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty());
    let named = reported.is_some_and(|line| {
        line.split_whitespace()
            .any(|word| word.trim_start_matches('v') == version)
    });
    if !output.status.success() || !named {
        return Err(format!(
            "Grok's installer did not unpack {version}: it reported {}",
            reported.unwrap_or("nothing")
        ));
    }
    if !is_absolute_executable(&grok_native_binary(dir, version)) {
        return Err(format!(
            "Grok's installer ran {version} but left no binary in {}",
            home.display()
        ));
    }
    Ok(())
}

/// Give back what a Windows child needs to start after `env_clear`.
///
/// Grok's Windows entrypoint is `grok.cmd`, which runs under `cmd.exe`: it
/// needs `ComSpec` to find the shell, `SystemRoot` to start at all, and
/// `PATHEXT` to resolve the commands it runs. Unix has none of these.
#[cfg(windows)]
fn keep_windows_start_env(command: &mut Command) {
    for name in ["SystemRoot", "ComSpec", "PATHEXT"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

#[cfg(not(windows))]
fn keep_windows_start_env(_command: &mut Command) {}

/// The managed binary for `kind`, unpacking an installed Grok binary that is
/// not unpacked yet.
///
/// Only the Grok install from before Tidebreak unpacked its binary needs
/// this; it downloads nothing. `version` names the install the update
/// channel drives, and `None` is the pin. Any other engine, a missing
/// install, or a Node runtime Tidebreak has not verified answers
/// [`managed_binary_version`]'s answer.
pub async fn unpack_installed_binary(
    data_dir: &Path,
    kind: HarnessKind,
    version: Option<&str>,
    managed_node_root: Option<&Path>,
) -> Option<PathBuf> {
    let pin = pin_for(kind)?;
    let version = version.unwrap_or(pin.version);
    if let Some(binary) = managed_binary_version(data_dir, kind, version) {
        return Some(binary);
    }
    let node_root = verified_managed_node_root(managed_node_root)?;
    let dir = install_dir_for(data_dir, pin, version);
    if kind != HarnessKind::Grok || !installed_tree(&dir, pin, version) {
        return None;
    }
    let lock = install_lock(&dir);
    let _guard = lock.lock().await;
    if let Err(err) = unpack_grok(&dir, pin, version, node_root).await {
        tracing::warn!(%err, "could not unpack the installed Grok binary");
    }
    managed_binary_version(data_dir, kind, version)
}

/// The verified root when both platform-native Node and npm entrypoints exist.
///
/// Unix npm is a script that resolves `node` from PATH. Windows npm is a
/// `.cmd` shim that first resolves the sibling `node.exe`. Both are validated
/// here and the managed runtime directory is still placed first on PATH for
/// the harness shims installed under `node_modules/.bin`.
fn verified_managed_node_root(managed_node_root: Option<&Path>) -> Option<&Path> {
    let root = managed_node_root?;
    (is_absolute_executable(&managed_node_executable(root))
        && is_absolute_executable(&managed_npm_executable(root)))
    .then_some(root)
}

/// Where npm keeps its cache and logs for every command this crate runs.
///
/// npm defaults both to `$HOME/.npm`, and a self-host container may run as a
/// uid with no home at all: Kubernetes hands an unmapped uid `HOME=/`, and
/// the first thing npm does is fail to create `/.npm/_logs`, so every install
/// died before a byte was fetched. The cache lives beside the installs so it
/// depends on nothing but the data directory.
fn npm_cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("tools").join("npm-cache")
}

/// The managed runtime's npm with the environment every command needs: the
/// runtime first on PATH, and its cache under the data directory rather than
/// wherever `$HOME` points.
fn npm_command(data_dir: &Path, node_root: &Path) -> Command {
    let mut command = Command::new(managed_npm_executable(node_root));
    command
        .env("PATH", prepend_path(&managed_node_path_dir(node_root)))
        .env("npm_config_cache", npm_cache_dir(data_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn prepend_path(bin: &Path) -> std::ffi::OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&current));
    std::env::join_paths(paths).unwrap_or_else(|_| bin.as_os_str().to_os_string())
}

/// A Grok install the way npm leaves one from before Tidebreak unpacked its
/// binary, beside a person whose own Grok is a newer release.
#[cfg(all(test, unix))]
pub(crate) struct FakeGrok {
    pub(crate) data_dir: PathBuf,
    pub(crate) node_root: PathBuf,
    /// The person's home directory, whose `.grok/bin/grok` reports 1.0.40.
    pub(crate) person_home: PathBuf,
    /// One line per entrypoint run: the Grok home it resolved.
    pub(crate) entrypoint_log: PathBuf,
}

/// Lay out a [`FakeGrok`] under `root`.
///
/// The entrypoint does what Grok's npm entrypoint does: it runs
/// `$GROK_HOME/bin/grok`, where `GROK_HOME` defaults to `$HOME/.grok`, and
/// unpacks the binary it shipped there as `grok-<version>` when that file is
/// missing. The shipped binary reports the pin; the person's reports 1.0.40.
#[cfg(all(test, unix))]
pub(crate) fn fake_grok_install(root: &Path) -> FakeGrok {
    use std::os::unix::fs::PermissionsExt;
    let write_exec = |path: &Path, body: &str| {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let pin = pin_for(HarnessKind::Grok).unwrap();
    let data_dir = root.join("data");
    let dir = install_dir(&data_dir, pin);
    let shipped = dir.join("node_modules/@xai-official/grok-platform/bin/grok");
    write_exec(
        &shipped,
        &format!("#!/bin/sh\necho 'grok {} (shipped)'\n", pin.version),
    );
    let entrypoint_log = root.join("entrypoint.log");
    write_exec(
        &binary_path(&dir, pin),
        &format!(
            r#"#!/bin/sh
home="${{GROK_HOME:-$HOME/.grok}}"
printf '%s\n' "$home" >> '{log}'
if [ ! -e "$home/bin/grok" ]; then
  mkdir -p "$home/bin"
  cp '{shipped}' "$home/bin/grok-{version}"
  ln -s 'grok-{version}' "$home/bin/grok"
fi
exec "$home/bin/grok" "$@"
"#,
            log = entrypoint_log.display(),
            shipped = shipped.display(),
            version = pin.version,
        ),
    );
    std::fs::write(
        marker_path(&dir),
        serde_json::to_vec(&InstallMarker {
            package: pin.package.to_owned(),
            version: pin.version.to_owned(),
        })
        .unwrap(),
    )
    .unwrap();
    let person_home = root.join("person");
    write_exec(
        &person_home.join(".grok/bin/grok"),
        "#!/bin/sh\necho 'grok 1.0.40 (updated by Grok)'\n",
    );
    let node_root = root.join("node");
    write_exec(&managed_node_executable(&node_root), "#!/bin/sh\nexit 0\n");
    write_exec(&managed_npm_executable(&node_root), "#!/bin/sh\nexit 1\n");
    FakeGrok {
        data_dir,
        node_root,
        person_home,
        entrypoint_log,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_pin() {
        for kind in [
            HarnessKind::ClaudeCode,
            HarnessKind::Codex,
            HarnessKind::Opencode,
            HarnessKind::Grok,
        ] {
            assert!(pin_for(kind).is_some(), "{kind}");
        }
    }

    /// Every pin's sign-in command, checked against the `--help` of that
    /// exact release.
    ///
    /// The doctor names this command and the Sign in action runs it, so a
    /// wrong one sends a person to a prompt rather than a login: Claude Code
    /// has no top-level `login`, and `claude login` starts a session with
    /// "login" as its first message. The capture lives beside the version's
    /// other fixtures. A pin bump fails here until someone captures
    /// `<bin> <sign-in args> --help` from the new release and, if the
    /// command moved, updates [`HarnessPin::sign_in`].
    #[test]
    fn sign_in_commands_match_the_pinned_help() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        for pin in PINS {
            let engine = match pin.kind {
                HarnessKind::ClaudeCode => "claude-code",
                HarnessKind::Codex => "codex",
                HarnessKind::Opencode => "opencode",
                HarnessKind::Grok => "grok",
                HarnessKind::Internal => continue,
            };
            let command = sign_in_command(pin.kind)
                .unwrap_or_else(|| panic!("{} ships a pin with no sign-in command", pin.kind));
            let capture = fixtures
                .join(engine)
                .join(pin.version)
                .join("sign-in-help.txt");
            let help = std::fs::read_to_string(&capture).unwrap_or_else(|_| {
                panic!(
                    "capture `{command} --help` from the {} {} pin into {}",
                    pin.kind,
                    pin.version,
                    capture.display()
                )
            });
            // clap and commander print `Usage: <command> …`; yargs prints the
            // command on its own first line.
            let usage = help.lines().map(str::trim).any(|line| {
                let line = line.strip_prefix("Usage:").map_or(line, str::trim);
                line == command || line.starts_with(&format!("{command} "))
            });
            assert!(
                usage,
                "{} {} does not document `{command}`; see {}",
                pin.kind,
                pin.version,
                capture.display()
            );
        }
    }

    /// Every pin's hand-kept capability tables were checked against that
    /// exact release. Moving a pin without looking at its tables is how a
    /// later release inherited an earlier one's effort ladder.
    #[test]
    fn capability_tables_were_reviewed_for_every_pin() {
        for pin in PINS {
            let reviewed = TABLES_REVIEWED_AT
                .iter()
                .find(|(kind, _)| *kind == pin.kind)
                .map(|(_, version)| *version);
            assert_eq!(
                reviewed,
                Some(pin.version),
                "{} is pinned to {} but its capability tables were last checked against {}. \
                 Check each table TABLES_REVIEWED_AT names for it against the new release, \
                 then move its entry.",
                pin.kind,
                pin.version,
                reviewed.unwrap_or("no release"),
            );
        }
    }

    #[test]
    fn sign_in_commands_read_the_way_a_person_types_them() {
        assert_eq!(
            sign_in_command(HarnessKind::ClaudeCode).as_deref(),
            Some("claude auth login")
        );
        assert_eq!(
            sign_in_command(HarnessKind::Codex).as_deref(),
            Some("codex login")
        );
        assert_eq!(
            sign_in_command(HarnessKind::Opencode).as_deref(),
            Some("opencode auth login")
        );
        assert_eq!(
            sign_in_command(HarnessKind::Grok).as_deref(),
            Some("grok login")
        );
        assert_eq!(sign_in_command(HarnessKind::Internal), None);
        assert_eq!(sign_in_args(HarnessKind::Internal), None);
    }

    /// Codex 0.153.0 lacks gpt-6-astra model metadata and falls back through
    /// Model Gateway. 0.153.4 embeds Astra while keeping the same route id.
    #[test]
    fn codex_pin_supplies_astra_metadata_line() {
        let pin = pin_for(HarnessKind::Codex).expect("codex pin");
        assert_eq!(pin.package, "@openai/codex");
        assert_eq!(pin.version, "0.153.4");
    }

    #[test]
    fn marker_mismatch_hides_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let pin = pin_for(HarnessKind::ClaudeCode).unwrap();
        let dir = install_dir(tmp.path(), pin);
        let binary = binary_path(&dir, pin);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            marker_path(&dir),
            r#"{"package":"@anthropic-ai/claude-code","version":"0.0.1"}"#,
        )
        .unwrap();
        assert_eq!(managed_binary(tmp.path(), HarnessKind::ClaudeCode), None);
        std::fs::write(
            marker_path(&dir),
            serde_json::to_vec(&InstallMarker {
                package: pin.package.to_owned(),
                version: pin.version.to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            managed_binary(tmp.path(), HarnessKind::ClaudeCode),
            Some(binary)
        );
    }

    fn write_install(data_dir: &Path, kind: HarnessKind, version: &str) -> PathBuf {
        let pin = pin_for(kind).unwrap();
        let dir = install_dir_for(data_dir, pin, version);
        let binary = binary_path(&dir, pin);
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            marker_path(&dir),
            serde_json::to_vec(&InstallMarker {
                package: pin.package.to_owned(),
                version: version.to_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        binary
    }

    /// A newer version sits beside the pin in its own directory. Neither
    /// hides the other, and the listing puts the newer one first — by
    /// number, not by text, so `2.1.300` beats `2.1.99`.
    #[test]
    fn installed_versions_list_newest_first_beside_the_pin() {
        let tmp = tempfile::tempdir().unwrap();
        let kind = HarnessKind::ClaudeCode;
        let pin = pin_for(kind).unwrap();
        let pinned = write_install(tmp.path(), kind, pin.version);
        let newer = write_install(tmp.path(), kind, "2.1.300");
        write_install(tmp.path(), kind, "2.1.99");
        // A directory whose marker disagrees with its name is not a version.
        let torn = install_dir_for(tmp.path(), pin, "9.9.9");
        std::fs::create_dir_all(&torn).unwrap();
        std::fs::write(
            marker_path(&torn),
            r#"{"package":"@anthropic-ai/claude-code","version":"1.0.0"}"#,
        )
        .unwrap();

        assert_eq!(
            installed_versions(tmp.path(), kind),
            vec![
                "2.1.300".to_owned(),
                pin.version.to_owned(),
                "2.1.99".to_owned()
            ]
        );
        assert_eq!(managed_binary(tmp.path(), kind), Some(pinned));
        assert_eq!(
            managed_binary_version(tmp.path(), kind, "2.1.300"),
            Some(newer)
        );
        assert_eq!(managed_binary_version(tmp.path(), kind, "9.9.9"), None);
    }

    #[test]
    fn versions_compare_by_number_then_prerelease() {
        assert_eq!(compare_versions("2.1.251", "2.1.99"), Ordering::Greater);
        assert_eq!(compare_versions("2.1.234", "2.1.234"), Ordering::Equal);
        assert_eq!(compare_versions("2.2.0", "2.1.300"), Ordering::Greater);
        assert_eq!(compare_versions("2.1.0-beta.1", "2.1.0"), Ordering::Less);
        assert_eq!(compare_versions("v1.0.4", "1.0.5"), Ordering::Less);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn a_registry_answer_must_look_like_a_version() {
        assert!(is_plausible_version("2.1.251"));
        assert!(is_plausible_version("1.0.0-rc.2"));
        assert!(!is_plausible_version(""));
        assert!(!is_plausible_version("latest"));
        assert!(!is_plausible_version("../2.1.251"));
        assert!(!is_plausible_version("2.1.251; rm -rf /"));
    }

    #[test]
    fn harness_binary_uses_the_platform_npm_shim_name() {
        let pin = pin_for(HarnessKind::ClaudeCode).unwrap();
        let path = binary_path(Path::new("install"), pin);
        #[cfg(windows)]
        assert_eq!(path.file_name().unwrap(), "claude.cmd");
        #[cfg(not(windows))]
        assert_eq!(path.file_name().unwrap(), "claude");
    }

    /// A container may run as a uid that owns no home directory. npm must
    /// never learn where `$HOME` points: its cache and logs go under the data
    /// directory for every command, install and registry lookup alike.
    #[test]
    fn npm_keeps_its_cache_under_the_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        let node_root = tmp.path().join("node");
        let command = npm_command(&data_dir, &node_root);
        let std_command = command.as_std();
        let cache = std_command
            .get_envs()
            .find(|(key, _)| *key == "npm_config_cache")
            .and_then(|(_, value)| value)
            .expect("npm is told where its cache lives");
        assert_eq!(
            std::path::Path::new(cache),
            data_dir.join("tools").join("npm-cache")
        );
        assert!(
            std_command.get_envs().all(|(key, _)| key != "HOME"),
            "npm's cache placement does not depend on HOME"
        );
    }

    #[test]
    fn missing_managed_node_is_an_install_error() {
        let tmp = tempfile::tempdir().unwrap();
        // A directory that resembles an install is not authority. Only the
        // verified root supplied by the host may be used.
        let decoy_bin = tmp.path().join("tools/node/decoy/bin");
        std::fs::create_dir_all(&decoy_bin).unwrap();
        for name in ["node", "npm"] {
            let path = decoy_bin.join(name);
            std::fs::write(&path, b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let err = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(ensure_installed(tmp.path(), HarnessKind::ClaudeCode, None))
            .unwrap_err();
        assert!(err.contains("managed Node"), "{err}");
    }

    /// Grok's npm entrypoint runs the person's `~/.grok/bin/grok`, which
    /// Grok's own updater keeps current, so the pinned install can run
    /// another release. The install instead unpacks the binary it shipped
    /// into its own Grok home, once, and Tidebreak runs that binary. The
    /// unpack run names that home, and nothing lands in the person's.
    #[cfg(unix)]
    #[tokio::test]
    async fn grok_unpacks_the_binary_it_shipped_into_its_own_home() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = super::fake_grok_install(tmp.path());
        let pin = pin_for(HarnessKind::Grok).unwrap();
        let home = grok_home(&install_dir(&fake.data_dir, pin));
        assert_eq!(
            managed_binary(&fake.data_dir, HarnessKind::Grok),
            None,
            "an install from before the unpack has no binary to run yet"
        );
        assert!(installed_versions(&fake.data_dir, HarnessKind::Grok).is_empty());
        assert_eq!(
            installed_trees(&fake.data_dir, HarnessKind::Grok),
            [pin.version],
            "removal still sees it"
        );

        let binary = ensure_installed(&fake.data_dir, HarnessKind::Grok, Some(&fake.node_root))
            .await
            .unwrap();
        assert_eq!(
            binary,
            home.join("bin").join(format!("grok-{}", pin.version))
        );
        assert_eq!(
            managed_bin_dir(&fake.data_dir, HarnessKind::Grok, None),
            Some(home.join("bin"))
        );
        assert_eq!(
            std::fs::read_to_string(&fake.entrypoint_log).unwrap(),
            format!("{}\n", home.display()),
            "the entrypoint ran once, in the install's own Grok home"
        );
        assert!(!fake
            .person_home
            .join(format!(".grok/bin/grok-{}", pin.version))
            .exists());

        // Run the way sessions run it, in the person's own home: it still
        // reports the pin, not the person's newer Grok.
        let env = vec![
            ("HOME".into(), fake.person_home.clone().into_os_string()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ];
        let version = crate::observe_version(&binary, &env).await.unwrap();
        assert_eq!(version, format!("grok {} (shipped)", pin.version));

        // Already unpacked: nothing runs again.
        ensure_installed(&fake.data_dir, HarnessKind::Grok, Some(&fake.node_root))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&fake.entrypoint_log)
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    /// A tree npm installed that cannot unpack Grok's binary, such as one
    /// missing the binary it ships, is installed again once, and the unpack
    /// runs on the fresh tree. Skipping the install because a tree exists
    /// left such an install "not installed" for good.
    #[cfg(unix)]
    #[tokio::test]
    async fn grok_installs_a_tree_again_when_its_binary_will_not_unpack() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = super::fake_grok_install(tmp.path());
        let pin = pin_for(HarnessKind::Grok).unwrap();
        let dir = install_dir(&fake.data_dir, pin);
        let shipped = dir.join("node_modules/@xai-official/grok-platform/bin/grok");
        let kept = tmp.path().join("shipped-grok");
        std::fs::rename(&shipped, &kept).unwrap();
        let npm = managed_npm_executable(&fake.node_root);
        std::fs::write(
            &npm,
            format!(
                "#!/bin/sh\nprintf 'npm\\n' >> '{log}'\ncp '{kept}' '{shipped}'\n",
                log = fake.entrypoint_log.display(),
                kept = kept.display(),
                shipped = shipped.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();

        let binary = ensure_installed(&fake.data_dir, HarnessKind::Grok, Some(&fake.node_root))
            .await
            .unwrap();
        assert_eq!(
            binary,
            grok_home(&dir)
                .join("bin")
                .join(format!("grok-{}", pin.version))
        );
        let home = grok_home(&dir).display().to_string();
        assert_eq!(
            std::fs::read_to_string(&fake.entrypoint_log)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [home.as_str(), "npm", home.as_str()],
            "the unpack failed, npm installed the tree again, and the unpack ran once more"
        );
    }

    /// The warm install and the create path's fallback can ask for one pin at
    /// the same time. Only one of them may be inside the install directory.
    #[tokio::test]
    async fn one_pin_directory_admits_one_installer_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let pin = pin_for(HarnessKind::ClaudeCode).unwrap();
        let dir = install_dir(tmp.path(), pin);
        let inside = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let installers = (0..4).map(|_| {
            let (dir, inside, peak) = (dir.clone(), Arc::clone(&inside), Arc::clone(&peak));
            tokio::spawn(async move {
                let lock = install_lock(&dir);
                let _guard = lock.lock().await;
                let now = inside.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                tokio::task::yield_now().await;
                inside.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            })
        });
        for installer in installers.collect::<Vec<_>>() {
            installer.await.unwrap();
        }

        assert_eq!(peak.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A second pin is not held up by the first.
        let other = install_dir(tmp.path(), pin_for(HarnessKind::Codex).unwrap());
        assert!(!Arc::ptr_eq(&install_lock(&dir), &install_lock(&other)));
    }
}
