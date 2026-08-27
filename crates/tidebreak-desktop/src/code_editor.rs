//! Native open-in-editor for one file inside a Code workspace's worktree.
//!
//! The sibling `code_worktree` module hands a whole worktree to the operating
//! system's folder handler. This one goes a step further: it opens a single
//! file, at a line, in the editor the reader already uses. The renderer never
//! supplies a path to run — it names a workspace and a path relative to that
//! workspace, and this module re-reads the worktree root from the embedded
//! server, resolves the file inside it, and refuses anything that lands
//! outside. The editor comes from a closed set of launcher plans, so no part
//! of the renderer's input reaches a shell.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;
use tidebreak_core::{CodeWorkspaceStatus, WorkspaceId};

use crate::remote::RemoteAttachment;
use crate::{wait_server_info, AppState};

pub(crate) const REASON_AUTHORITY_UNAVAILABLE: &str = "code_editor_authority_unavailable";
pub(crate) const REASON_WORKSPACE_UNAVAILABLE: &str = "code_editor_workspace_unavailable";
pub(crate) const REASON_WORKSPACE_INACTIVE: &str = "code_editor_workspace_inactive";
pub(crate) const REASON_PATH_INVALID: &str = "code_editor_path_invalid";
pub(crate) const REASON_PATH_OUTSIDE_WORKTREE: &str = "code_editor_path_outside_worktree";
pub(crate) const REASON_PATH_NOT_FOUND: &str = "code_editor_path_not_found";
pub(crate) const REASON_EDITOR_UNKNOWN: &str = "code_editor_editor_unknown";
pub(crate) const REASON_EDITOR_UNAVAILABLE: &str = "code_editor_editor_unavailable";
pub(crate) const REASON_OPEN_FAILED: &str = "code_editor_open_failed";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeEditorOpenError {
    /// Stable reason code that the renderer turns into user-facing copy.
    reason: &'static str,
    /// Native diagnostic for logs and support. The renderer does not display it.
    detail: Option<String>,
}

impl CodeEditorOpenError {
    fn new(reason: &'static str) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    fn detailed(reason: &'static str, detail: impl std::fmt::Display) -> Self {
        Self {
            reason,
            detail: Some(detail.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CodeEditorOpenStatus {
    Opened,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeEditorOpenResult {
    status: CodeEditorOpenStatus,
}

/// The editors this desktop knows how to start.
///
/// A closed set, not a command string: each variant carries its own program
/// candidates and its own way of spelling "this file, at this line", so the
/// renderer chooses an editor rather than composing a command line. `Custom`
/// is the one escape hatch, and it still takes an absolute program path that
/// is spawned directly — never a shell string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ExternalEditorKind {
    Vscode,
    Cursor,
    Zed,
    Jetbrains,
    Custom,
}

impl ExternalEditorKind {
    /// The kinds the settings panel probes, in the order it shows them.
    const DETECTED: [Self; 4] = [Self::Vscode, Self::Cursor, Self::Zed, Self::Jetbrains];

    fn id(self) -> &'static str {
        match self {
            Self::Vscode => "vscode",
            Self::Cursor => "cursor",
            Self::Zed => "zed",
            Self::Jetbrains => "jetbrains",
            Self::Custom => "custom",
        }
    }

    /// Program names looked up on `PATH`, most specific first.
    fn commands(self) -> &'static [&'static str] {
        match self {
            Self::Vscode => &["code"],
            Self::Cursor => &["cursor"],
            Self::Zed => &["zed"],
            Self::Jetbrains => &["idea", "pycharm", "webstorm", "rustrover", "goland"],
            Self::Custom => &[],
        }
    }

    /// Absolute locations to try when the command-line launcher is not on
    /// `PATH`. A desktop app started from Finder or a launcher inherits a
    /// minimal `PATH`, so the installed-editor probe cannot rely on it alone.
    fn fallbacks(self) -> &'static [&'static str] {
        match self {
            Self::Vscode => &[
                "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
                "/usr/share/code/bin/code",
                "/snap/bin/code",
            ],
            Self::Cursor => &[
                "/Applications/Cursor.app/Contents/Resources/app/bin/cursor",
                "/usr/share/cursor/bin/cursor",
            ],
            Self::Zed => &["/Applications/Zed.app/Contents/MacOS/cli", "/snap/bin/zed"],
            Self::Jetbrains => &[
                "/Applications/IntelliJ IDEA.app/Contents/MacOS/idea",
                "/Applications/IntelliJ IDEA CE.app/Contents/MacOS/idea",
                "/snap/bin/intellij-idea-community",
            ],
            Self::Custom => &[],
        }
    }
}

/// One editor's availability on this computer, as the settings panel shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExternalEditorProbe {
    /// The stable id the renderer stores as the reader's preference.
    id: &'static str,
    /// The launcher this probe found, for the panel's caption. Absent when the
    /// editor is not installed.
    program: Option<String>,
}

/// What this computer has installed, so the settings panel can say so.
#[tauri::command]
pub(crate) async fn detect_external_editors(
) -> Result<Vec<ExternalEditorProbe>, CodeEditorOpenError> {
    let search_path = std::env::var_os("PATH");
    Ok(ExternalEditorKind::DETECTED
        .iter()
        .map(|kind| ExternalEditorProbe {
            id: kind.id(),
            program: resolve_program(*kind, search_path.as_deref())
                .map(|program| program.display().to_string()),
        })
        .collect())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenInEditorRequest {
    workspace_id: String,
    /// A path relative to the worktree root. Absent opens the worktree itself.
    #[serde(default)]
    relative_path: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    editor: ExternalEditorKind,
    /// Absolute program path, required when `editor` is `custom`.
    #[serde(default)]
    custom_program: Option<String>,
}

/// Open one file from a local worktree in the reader's own editor.
#[tauri::command]
pub(crate) async fn open_in_editor(
    attachment: State<'_, Arc<RemoteAttachment>>,
    app_state: State<'_, Arc<AppState>>,
    request: OpenInEditorRequest,
) -> Result<CodeEditorOpenResult, CodeEditorOpenError> {
    if attachment.current().await.is_some() {
        return Err(CodeEditorOpenError::new(REASON_AUTHORITY_UNAVAILABLE));
    }
    let workspace_id: WorkspaceId = request
        .workspace_id
        .parse()
        .map_err(|_| CodeEditorOpenError::new(REASON_WORKSPACE_UNAVAILABLE))?;
    let info = wait_server_info(app_state.inner())
        .await
        .map_err(|error| CodeEditorOpenError::detailed(REASON_WORKSPACE_UNAVAILABLE, error))?;
    let workspace = read_workspace(&info, workspace_id).await?;
    if !matches!(
        workspace.status,
        CodeWorkspaceStatus::Active | CodeWorkspaceStatus::SetupFailed
    ) {
        return Err(CodeEditorOpenError::new(REASON_WORKSPACE_INACTIVE));
    }
    let target = resolve_target(
        Path::new(&workspace.worktree_path),
        request.relative_path.as_deref(),
    )?;
    let program = match request.editor {
        ExternalEditorKind::Custom => custom_program(request.custom_program.as_deref())?,
        kind => resolve_program(kind, std::env::var_os("PATH").as_deref())
            .ok_or_else(|| CodeEditorOpenError::new(REASON_EDITOR_UNAVAILABLE))?,
    };
    let plan = launcher_plan(request.editor, &program, &target, request.line);
    spawn(plan)
}

#[derive(Deserialize)]
struct CodeWorktreeSnapshot {
    worktree_path: String,
    status: CodeWorkspaceStatus,
}

async fn read_workspace(
    info: &crate::NativeServerInfo,
    workspace_id: WorkspaceId,
) -> Result<CodeWorktreeSnapshot, CodeEditorOpenError> {
    let response = crate::documents::local_client()
        .get(format!("{}/code/workspaces/{workspace_id}", info.base_url))
        .bearer_auth(&info.token)
        .send()
        .await
        .map_err(|error| CodeEditorOpenError::detailed(REASON_WORKSPACE_UNAVAILABLE, error))?;
    if !response.status().is_success() {
        return Err(CodeEditorOpenError::detailed(
            REASON_WORKSPACE_UNAVAILABLE,
            response.status(),
        ));
    }
    response
        .json()
        .await
        .map_err(|error| CodeEditorOpenError::detailed(REASON_WORKSPACE_UNAVAILABLE, error))
}

/// The absolute file (or folder) to open, proven to sit inside the worktree.
///
/// Both sides are canonicalized before they are compared, so a symlink inside
/// the worktree that points elsewhere is refused the same way `../` is. That
/// also means the target has to exist: an editor cannot open a file the
/// worktree no longer has, and inventing the path would defeat the check.
fn resolve_target(worktree: &Path, relative: Option<&str>) -> Result<PathBuf, CodeEditorOpenError> {
    if !worktree.is_absolute() {
        return Err(CodeEditorOpenError::new(REASON_PATH_INVALID));
    }
    let root = canonical(worktree)?;
    let Some(relative) = relative else {
        return Ok(root);
    };
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err(CodeEditorOpenError::new(REASON_PATH_INVALID));
    }
    // Refuse traversal in the request itself rather than relying on the
    // canonical comparison alone: a `..` that happens to stay inside is still
    // the renderer reaching past the path it was given.
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CodeEditorOpenError::new(REASON_PATH_INVALID));
    }
    let target = canonical(&root.join(relative))?;
    if !target.starts_with(&root) {
        return Err(CodeEditorOpenError::new(REASON_PATH_OUTSIDE_WORKTREE));
    }
    Ok(target)
}

fn canonical(path: &Path) -> Result<PathBuf, CodeEditorOpenError> {
    std::fs::canonicalize(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CodeEditorOpenError::new(REASON_PATH_NOT_FOUND)
        } else {
            CodeEditorOpenError::detailed(REASON_OPEN_FAILED, error)
        }
    })
}

/// The reader's own `$EDITOR`-style program, held to the same shape as the
/// built-in launchers: one absolute executable, spawned directly.
fn custom_program(program: Option<&str>) -> Result<PathBuf, CodeEditorOpenError> {
    let program = program.map(str::trim).filter(|value| !value.is_empty());
    let Some(program) = program else {
        return Err(CodeEditorOpenError::new(REASON_EDITOR_UNKNOWN));
    };
    let path = PathBuf::from(program);
    if !path.is_absolute() {
        return Err(CodeEditorOpenError::new(REASON_EDITOR_UNKNOWN));
    }
    if !path.is_file() {
        return Err(CodeEditorOpenError::new(REASON_EDITOR_UNAVAILABLE));
    }
    Ok(path)
}

/// The first program for `kind` that exists, searching `PATH` then the known
/// install locations.
fn resolve_program(
    kind: ExternalEditorKind,
    search_path: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    for command in kind.commands() {
        for directory in search_path.iter().flat_map(std::env::split_paths) {
            let candidate = directory.join(command);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    kind.fallbacks()
        .iter()
        .map(PathBuf::from)
        .find(|candidate| candidate.is_file())
}

struct LauncherPlan {
    program: PathBuf,
    args: Vec<OsString>,
}

/// How each editor spells "open this file at this line".
///
/// VS Code and its forks take `--goto path:line`; Zed takes `path:line`;
/// JetBrains takes `--line <n> <path>`. A custom program gets the path alone,
/// because nothing here knows its flags.
fn launcher_plan(
    kind: ExternalEditorKind,
    program: &Path,
    target: &Path,
    line: Option<u32>,
) -> LauncherPlan {
    let line = line.filter(|line| *line >= 1);
    let args = match (kind, line) {
        (ExternalEditorKind::Vscode | ExternalEditorKind::Cursor, Some(line)) => {
            vec![OsString::from("--goto"), suffixed(target, line)]
        }
        (ExternalEditorKind::Zed, Some(line)) => vec![suffixed(target, line)],
        (ExternalEditorKind::Jetbrains, Some(line)) => vec![
            OsString::from("--line"),
            OsString::from(line.to_string()),
            target.as_os_str().to_owned(),
        ],
        _ => vec![target.as_os_str().to_owned()],
    };
    LauncherPlan {
        program: program.to_path_buf(),
        args,
    }
}

/// `path:line`, built on the raw bytes so a path that is not valid UTF-8 keeps
/// its own spelling instead of being lossily rewritten.
fn suffixed(target: &Path, line: u32) -> OsString {
    let mut value = target.as_os_str().to_owned();
    value.push(format!(":{line}"));
    value
}

fn spawn(plan: LauncherPlan) -> Result<CodeEditorOpenResult, CodeEditorOpenError> {
    let mut command = tokio::process::Command::new(&plan.program);
    command
        .args(plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let mut child = command.spawn().map_err(|error| {
        let reason = if error.kind() == std::io::ErrorKind::NotFound {
            REASON_EDITOR_UNAVAILABLE
        } else {
            REASON_OPEN_FAILED
        };
        CodeEditorOpenError::detailed(reason, error)
    })?;
    tauri::async_runtime::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(CodeEditorOpenResult {
        status: CodeEditorOpenStatus::Opened,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree() -> tempfile::TempDir {
        let directory = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(directory.path().join("src")).unwrap();
        std::fs::write(directory.path().join("src/main.rs"), "fn main() {}").unwrap();
        directory
    }

    #[test]
    fn resolves_a_file_inside_the_worktree_and_nothing_outside_it() {
        let directory = worktree();
        let root = std::fs::canonicalize(directory.path()).unwrap();

        assert_eq!(
            resolve_target(directory.path(), Some("src/main.rs")).unwrap(),
            root.join("src/main.rs")
        );
        // No relative path opens the worktree itself.
        assert_eq!(resolve_target(directory.path(), None).unwrap(), root);

        for traversal in ["../escape", "src/../../escape", "./src/main.rs"] {
            assert_eq!(
                resolve_target(directory.path(), Some(traversal))
                    .unwrap_err()
                    .reason,
                REASON_PATH_INVALID,
                "{traversal} should be refused"
            );
        }
        assert_eq!(
            resolve_target(directory.path(), Some("/etc/passwd"))
                .unwrap_err()
                .reason,
            REASON_PATH_INVALID
        );
        assert_eq!(
            resolve_target(directory.path(), Some(""))
                .unwrap_err()
                .reason,
            REASON_PATH_INVALID
        );
        assert_eq!(
            resolve_target(Path::new("relative/worktree"), Some("src/main.rs"))
                .unwrap_err()
                .reason,
            REASON_PATH_INVALID
        );
        assert_eq!(
            resolve_target(directory.path(), Some("src/missing.rs"))
                .unwrap_err()
                .reason,
            REASON_PATH_NOT_FOUND
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_that_leaves_the_worktree_is_refused() {
        let directory = worktree();
        let outside = tempfile::TempDir::new().unwrap();
        let secret = outside.path().join("secret");
        std::fs::write(&secret, "not in the worktree").unwrap();
        std::os::unix::fs::symlink(&secret, directory.path().join("src/link.rs")).unwrap();

        assert_eq!(
            resolve_target(directory.path(), Some("src/link.rs"))
                .unwrap_err()
                .reason,
            REASON_PATH_OUTSIDE_WORKTREE
        );
    }

    #[test]
    fn each_editor_spells_the_line_its_own_way() {
        let program = Path::new("/usr/local/bin/editor");
        let target = Path::new("/work tree/src/main.rs");

        let vscode = launcher_plan(ExternalEditorKind::Vscode, program, target, Some(42));
        assert_eq!(vscode.program, program);
        assert_eq!(
            vscode.args,
            vec![
                OsString::from("--goto"),
                OsString::from("/work tree/src/main.rs:42")
            ]
        );
        assert_eq!(
            launcher_plan(ExternalEditorKind::Cursor, program, target, Some(42)).args,
            vscode.args
        );
        assert_eq!(
            launcher_plan(ExternalEditorKind::Zed, program, target, Some(7)).args,
            vec![OsString::from("/work tree/src/main.rs:7")]
        );
        assert_eq!(
            launcher_plan(ExternalEditorKind::Jetbrains, program, target, Some(7)).args,
            vec![
                OsString::from("--line"),
                OsString::from("7"),
                target.as_os_str().to_owned()
            ]
        );
        // A custom program gets the path alone: nothing here knows its flags.
        assert_eq!(
            launcher_plan(ExternalEditorKind::Custom, program, target, Some(7)).args,
            vec![target.as_os_str().to_owned()]
        );
    }

    #[test]
    fn no_line_and_a_zero_line_both_open_the_file_plainly() {
        let program = Path::new("/usr/local/bin/code");
        let target = Path::new("/work/src/main.rs");
        for line in [None, Some(0)] {
            assert_eq!(
                launcher_plan(ExternalEditorKind::Vscode, program, target, line).args,
                vec![target.as_os_str().to_owned()],
                "line {line:?} should not produce a --goto"
            );
        }
    }

    #[test]
    fn a_custom_editor_must_be_an_absolute_program_that_exists() {
        let directory = tempfile::TempDir::new().unwrap();
        let program = directory.path().join("my-editor");
        std::fs::write(&program, "#!/bin/sh\n").unwrap();

        assert_eq!(
            custom_program(Some(&program.display().to_string())).unwrap(),
            program
        );
        assert_eq!(
            custom_program(None).unwrap_err().reason,
            REASON_EDITOR_UNKNOWN
        );
        assert_eq!(
            custom_program(Some("   ")).unwrap_err().reason,
            REASON_EDITOR_UNKNOWN
        );
        // A bare name would be a `PATH` lookup, which is a shell's job.
        assert_eq!(
            custom_program(Some("vim")).unwrap_err().reason,
            REASON_EDITOR_UNKNOWN
        );
        assert_eq!(
            custom_program(Some(
                &directory.path().join("missing").display().to_string()
            ))
            .unwrap_err()
            .reason,
            REASON_EDITOR_UNAVAILABLE
        );
    }

    #[test]
    fn detection_searches_the_given_path_and_reports_a_missing_editor() {
        let directory = tempfile::TempDir::new().unwrap();
        std::fs::write(directory.path().join("code"), "#!/bin/sh\n").unwrap();
        let search_path = std::env::join_paths([directory.path()]).unwrap();

        assert_eq!(
            resolve_program(ExternalEditorKind::Vscode, Some(&search_path)),
            Some(directory.path().join("code"))
        );
        assert_eq!(
            resolve_program(ExternalEditorKind::Custom, Some(&search_path)),
            None
        );
    }

    #[test]
    fn failures_serialize_as_stable_reason_and_private_detail() {
        let value = serde_json::to_value(CodeEditorOpenError::detailed(
            REASON_OPEN_FAILED,
            "native detail",
        ))
        .unwrap();
        assert_eq!(value["reason"], REASON_OPEN_FAILED);
        assert_eq!(value["detail"], "native detail");
    }

    #[test]
    fn the_renderer_names_an_editor_from_the_closed_set() {
        assert_eq!(
            serde_json::from_str::<ExternalEditorKind>("\"vscode\"").unwrap(),
            ExternalEditorKind::Vscode
        );
        assert_eq!(
            serde_json::from_str::<ExternalEditorKind>("\"jetbrains\"").unwrap(),
            ExternalEditorKind::Jetbrains
        );
        assert!(serde_json::from_str::<ExternalEditorKind>("\"emacs\"").is_err());
    }
}
