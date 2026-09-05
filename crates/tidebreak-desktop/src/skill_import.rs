//! Native import for user-authored skill folders.
//!
//! The renderer asks for one folder picker and receives only skill names and
//! outcomes. Source paths never cross into the webview. A selected folder can
//! be one skill package or a parent whose direct children are skill packages.
//! Parent discovery stays one level deep. Skill contents use a bounded
//! recursive traversal that rejects symbolic links and holds validated bytes
//! before publishing anything.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tidebreak_code_execution::{
    is_valid_skill_name, load_skills, parse_skill_manifest, SkillOrigin, WorkspaceFilePath,
    MAX_WORKSPACE_FILE_BYTES, SKILL_MANIFEST_FILE,
};
use tokio::sync::oneshot;

use crate::host_access::HostAccess;

const MAX_PARENT_ENTRIES: usize = 256;
const MAX_SKILLS_PER_IMPORT: usize = 64;
const MAX_SKILL_ENTRIES: usize = 512;
const MAX_SKILL_FILES: usize = 256;
const MAX_SKILL_DIRECTORIES: usize = 128;
const MAX_SKILL_DEPTH: usize = 8;
const MAX_SKILL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_IMPORT_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillImportReport {
    imported: Vec<String>,
    skipped: Vec<SkillImportIssue>,
    conflicts: Vec<SkillImportIssue>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillImportIssue {
    name: String,
    reason: String,
}

impl SkillImportReport {
    fn skip(&mut self, name: impl Into<String>, reason: impl Into<String>) {
        self.skipped.push(SkillImportIssue {
            name: name.into(),
            reason: reason.into(),
        });
    }

    fn conflict(&mut self, name: impl Into<String>, reason: impl Into<String>) {
        self.conflicts.push(SkillImportIssue {
            name: name.into(),
            reason: reason.into(),
        });
    }

    fn sort(&mut self) {
        self.imported.sort();
        self.skipped
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.conflicts
            .sort_by(|left, right| left.name.cmp(&right.name));
    }
}

/// Pick one skill folder or a parent of skill folders and copy valid packages
/// into this desktop profile's user-skills tree.
#[tauri::command]
pub(crate) async fn import_skills(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
) -> Result<Option<SkillImportReport>, String> {
    host_access
        .require_local(crate::host_authority::Authority::NativeExport)
        .await
        .map_err(|_| "Import skills on the Tidebreak desktop that hosts this library".to_owned())?;

    let selected = {
        let _picker = host_access
            .picker
            .try_lock()
            .map_err(|_| "A file or folder picker is already open".to_owned())?;
        pick_skill_folder(&app).await?
    };
    let Some(selected) = selected else {
        return Ok(None);
    };

    let user_skills_dir = crate::data_dir(&app)?.join("skills");
    let builtin_skills_dir = crate::exec_skills_dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let builtin_names = load_skills(&builtin_skills_dir, SkillOrigin::Builtin)
            .into_iter()
            .map(|skill| skill.package.name)
            .collect();
        import_selected_folder(&selected, &user_skills_dir, &builtin_names)
    })
    .await
    .map_err(|error| format!("Skill import stopped unexpectedly: {error}"))?
    .map(Some)
}

async fn pick_skill_folder(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    let (sender, receiver) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title("Choose a skill folder or a folder of skills");
    if let Some(window) = app.get_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.pick_folder(move |path| {
        let _ = sender.send(path);
    });
    let selected = receiver
        .await
        .map_err(|_| "The folder picker closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The folder picker returned an invalid path".to_owned())?;
    if selected.as_ref().is_some_and(|path| !path.is_absolute()) {
        return Err("The folder picker returned an invalid path".to_owned());
    }
    Ok(selected)
}

fn import_selected_folder(
    selected: &Path,
    user_skills_dir: &Path,
    builtin_names: &HashSet<String>,
) -> Result<SkillImportReport, String> {
    let source = open_directory_nofollow(selected)
        .map_err(|_| "Could not read the selected folder".to_owned())?;
    let destination = open_or_create_absolute_directory(user_skills_dir)
        .map_err(|_| "Could not open Tidebreak's skills folder".to_owned())?;
    let selected_name = display_name(selected);
    let mut report = SkillImportReport::default();
    let mut imported_bytes = 0_u64;

    match manifest_state(&source) {
        Ok(ManifestState::Regular) => import_candidate(
            &selected_name,
            source,
            &destination,
            builtin_names,
            &mut imported_bytes,
            &mut report,
        ),
        Ok(ManifestState::Invalid) => report.skip(
            selected_name,
            "SKILL.md must be a regular file, not a link or folder",
        ),
        Ok(ManifestState::Missing) => import_child_skills(
            selected,
            &source,
            &destination,
            builtin_names,
            &mut imported_bytes,
            &mut report,
        )?,
        Err(_) => report.skip(selected_name, "Could not inspect SKILL.md"),
    }

    report.sort();
    Ok(report)
}

fn import_child_skills(
    selected: &Path,
    source: &Dir,
    destination: &Dir,
    builtin_names: &HashSet<String>,
    imported_bytes: &mut u64,
    report: &mut SkillImportReport,
) -> Result<(), String> {
    let mut entries = source
        .entries()
        .map_err(|_| "Could not read the selected folder".to_owned())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not read the selected folder".to_owned())?;
    if entries.len() > MAX_PARENT_ENTRIES {
        return Err(format!(
            "This folder contains more than {MAX_PARENT_ENTRIES} entries. Choose a smaller folder"
        ));
    }
    entries.sort_by_key(|entry| entry.file_name());

    let mut candidate_skills = 0_usize;
    let mut child_directories = 0_usize;
    for entry in entries {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            if entry.file_name().to_str().is_some_and(is_valid_skill_name) {
                report.skip(
                    entry.file_name().to_string_lossy(),
                    "Symbolic-link skill folders are not imported",
                );
            }
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        child_directories += 1;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = match source.open_dir_nofollow(entry.file_name()) {
            Ok(child) => child,
            Err(_) => {
                report.skip(name, "Could not read this skill folder");
                continue;
            }
        };
        match manifest_state(&child) {
            Ok(ManifestState::Regular) => {
                if !is_valid_skill_name(&name) {
                    report.skip(
                        name,
                        "The folder name must be a lowercase kebab-case skill name",
                    );
                    continue;
                }
                candidate_skills += 1;
                if candidate_skills > MAX_SKILLS_PER_IMPORT {
                    report.skip(
                        name,
                        format!("One import is limited to {MAX_SKILLS_PER_IMPORT} skill folders"),
                    );
                    continue;
                }
                import_candidate(
                    &name,
                    child,
                    destination,
                    builtin_names,
                    imported_bytes,
                    report,
                );
            }
            Ok(ManifestState::Missing) => report.skip(name, "No regular SKILL.md was found"),
            Ok(ManifestState::Invalid) => report.skip(
                name,
                "SKILL.md must be a regular file, not a link or folder",
            ),
            Err(_) => report.skip(name, "Could not inspect SKILL.md"),
        }
    }

    if child_directories == 0 && report.skipped.is_empty() {
        report.skip(
            display_name(selected),
            "No skill folders were found one level inside this folder",
        );
    }
    Ok(())
}

fn import_candidate(
    directory_name: &str,
    source: Dir,
    destination_root: &Dir,
    builtin_names: &HashSet<String>,
    imported_bytes: &mut u64,
    report: &mut SkillImportReport,
) {
    if !is_valid_skill_name(directory_name) {
        report.skip(
            directory_name,
            "The folder name must be a lowercase kebab-case skill name",
        );
        return;
    }
    let prepared = match prepare_skill(&source) {
        Ok(prepared) => prepared,
        Err(reason) => {
            report.skip(directory_name, reason);
            return;
        }
    };
    if prepared.name != directory_name {
        report.skip(
            directory_name,
            format!("SKILL.md names this skill '{}'", prepared.name),
        );
        return;
    }
    if builtin_names.contains(directory_name) {
        report.conflict(
            directory_name,
            "A skill included with Tidebreak already uses this name",
        );
        return;
    }
    if prepared.estimated_bytes > MAX_IMPORT_BYTES.saturating_sub(*imported_bytes) {
        report.skip(
            directory_name,
            format!("One import is limited to {MAX_IMPORT_BYTES} bytes"),
        );
        return;
    }

    match destination_root.create_dir(directory_name) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            report.conflict(
                directory_name,
                "A user skill or plugin skill already uses this name",
            );
            return;
        }
        Err(_) => {
            report.skip(directory_name, "Could not create this skill folder");
            return;
        }
    }

    let copied = publish_skill(destination_root, directory_name, &prepared);
    match copied {
        Ok(bytes) => {
            *imported_bytes += bytes;
            report.imported.push(directory_name.to_owned());
        }
        Err(reason) => {
            let _ = destination_root.remove_dir_all(directory_name);
            report.skip(directory_name, reason);
        }
    }
}

struct PreparedSkill {
    name: String,
    manifest: Vec<u8>,
    contents: PreparedDirectory,
    estimated_bytes: u64,
}

struct PreparedDirectory {
    files: Vec<SourceFile>,
    directories: Vec<PreparedSubdirectory>,
}

struct PreparedSubdirectory {
    name: OsString,
    contents: PreparedDirectory,
}

struct SourceFile {
    name: OsString,
    content: Vec<u8>,
    executable: bool,
}

#[derive(Default)]
struct InspectionLimits {
    entries: usize,
    files: usize,
    directories: usize,
}

fn prepare_skill(source: &Dir) -> Result<PreparedSkill, String> {
    let manifest = read_bounded_file(source, OsStr::new(SKILL_MANIFEST_FILE))
        .map_err(|reason| format!("Could not read SKILL.md: {reason}"))?;
    let manifest_text =
        std::str::from_utf8(&manifest).map_err(|_| "SKILL.md must use UTF-8 text".to_owned())?;
    let package = parse_skill_manifest(manifest_text, SkillOrigin::User)
        .map_err(|error| error.to_string())?;
    let mut estimated_bytes = manifest.len() as u64;
    let mut limits = InspectionLimits::default();
    let contents = inspect_directory(source, "", 0, false, &mut limits, &mut estimated_bytes)?;
    Ok(PreparedSkill {
        name: package.name,
        manifest,
        contents,
        estimated_bytes,
    })
}

fn inspect_directory(
    directory: &Dir,
    relative_directory: &str,
    depth: usize,
    executable_files: bool,
    limits: &mut InspectionLimits,
    estimated_bytes: &mut u64,
) -> Result<PreparedDirectory, String> {
    let mut entries = directory
        .entries()
        .map_err(|_| "Could not read a folder in this skill".to_owned())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "Could not read a folder in this skill".to_owned())?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut files = Vec::new();
    let mut directories = Vec::new();
    for entry in entries {
        let name = entry.file_name();
        if depth == 0 && name == OsStr::new(SKILL_MANIFEST_FILE) {
            continue;
        }
        let component = name
            .to_str()
            .ok_or_else(|| "Skill paths must use UTF-8 names".to_owned())?;
        let relative = if relative_directory.is_empty() {
            component.to_owned()
        } else {
            format!("{relative_directory}/{component}")
        };
        WorkspaceFilePath::parse(relative.clone())
            .map_err(|_| "A skill contains an invalid or overly long path".to_owned())?;
        limits.entries += 1;
        if limits.entries > MAX_SKILL_ENTRIES {
            return Err(format!(
                "A skill is limited to {MAX_SKILL_ENTRIES} files and folders"
            ));
        }
        let file_type = entry
            .file_type()
            .map_err(|_| "Could not inspect a file in this skill".to_owned())?;
        if file_type.is_symlink() {
            return Err("Symbolic links are not imported".to_owned());
        }
        if file_type.is_dir() {
            if depth >= MAX_SKILL_DEPTH {
                return Err(format!(
                    "Skill folders are limited to {MAX_SKILL_DEPTH} levels"
                ));
            }
            limits.directories += 1;
            if limits.directories > MAX_SKILL_DIRECTORIES {
                return Err(format!(
                    "A skill is limited to {MAX_SKILL_DIRECTORIES} folders"
                ));
            }
            let child = directory
                .open_dir_nofollow(&name)
                .map_err(|_| "Could not read a folder in this skill".to_owned())?;
            let child_executable = executable_files || name == OsStr::new("scripts");
            directories.push(PreparedSubdirectory {
                name,
                contents: inspect_directory(
                    &child,
                    &relative,
                    depth + 1,
                    child_executable,
                    limits,
                    estimated_bytes,
                )?,
            });
            continue;
        }
        if !file_type.is_file() {
            return Err("Skill contents must be regular files and folders".to_owned());
        }
        limits.files += 1;
        if limits.files > MAX_SKILL_FILES {
            return Err(format!("A skill is limited to {MAX_SKILL_FILES} files"));
        }
        let content = read_bounded_file(directory, &name)?;
        *estimated_bytes = checked_skill_bytes(*estimated_bytes, content.len() as u64)?;
        files.push(SourceFile {
            name,
            content,
            executable: executable_files,
        });
    }
    Ok(PreparedDirectory { files, directories })
}

fn regular_file_len(directory: &Dir, name: &OsStr) -> Result<u64, String> {
    let metadata = directory
        .symlink_metadata(name)
        .map_err(|_| "Could not inspect a skill file".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Skill files must be regular files".to_owned());
    }
    let byte_len = metadata.len();
    if byte_len > MAX_WORKSPACE_FILE_BYTES as u64 {
        return Err(format!(
            "Each skill file is limited to {MAX_WORKSPACE_FILE_BYTES} bytes"
        ));
    }
    Ok(byte_len)
}

fn checked_skill_bytes(current: u64, added: u64) -> Result<u64, String> {
    let total = current
        .checked_add(added)
        .ok_or_else(|| "This skill is too large to import".to_owned())?;
    if total > MAX_SKILL_BYTES {
        return Err(format!("One skill is limited to {MAX_SKILL_BYTES} bytes"));
    }
    Ok(total)
}

fn publish_skill(
    destination_root: &Dir,
    name: &str,
    prepared: &PreparedSkill,
) -> Result<u64, String> {
    let destination = destination_root
        .open_dir_nofollow(name)
        .map_err(|_| "Could not open the new skill folder".to_owned())?;
    let mut copied = 0_u64;
    publish_directory(&destination, &prepared.contents, &mut copied)?;
    copied = checked_copied_bytes(
        copied,
        write_new_file(
            &destination,
            OsStr::new(SKILL_MANIFEST_FILE),
            &prepared.manifest,
        )?,
    )?;
    Ok(copied)
}

fn publish_directory(
    destination: &Dir,
    prepared: &PreparedDirectory,
    copied: &mut u64,
) -> Result<(), String> {
    for file in &prepared.files {
        *copied = checked_copied_bytes(
            *copied,
            write_prepared_file(destination, &file.name, &file.content, file.executable)?,
        )?;
    }
    for child in &prepared.directories {
        destination
            .create_dir(&child.name)
            .map_err(|_| "Could not create a skill subfolder".to_owned())?;
        let destination_child = destination
            .open_dir_nofollow(&child.name)
            .map_err(|_| "Could not open a skill subfolder".to_owned())?;
        publish_directory(&destination_child, &child.contents, copied)?;
    }
    Ok(())
}

fn write_prepared_file(
    destination: &Dir,
    name: &OsStr,
    content: &[u8],
    executable: bool,
) -> Result<u64, String> {
    let mut write_options = OpenOptions::new();
    write_options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        write_options.mode(if executable { 0o700 } else { 0o600 });
    }
    let mut destination_file = destination
        .open_with(name, &write_options)
        .map_err(|_| "Could not copy a skill file".to_owned())?;
    destination_file
        .write_all(content)
        .and_then(|()| destination_file.flush())
        .and_then(|()| destination_file.sync_all())
        .map_err(|_| "Could not finish copying a skill file".to_owned())?;
    Ok(content.len() as u64)
}

fn write_new_file(destination: &Dir, name: &OsStr, content: &[u8]) -> Result<u64, String> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = destination
        .open_with(name, &options)
        .map_err(|_| "Could not write SKILL.md".to_owned())?;
    file.write_all(content)
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|_| "Could not write SKILL.md".to_owned())?;
    Ok(content.len() as u64)
}

fn checked_copied_bytes(current: u64, added: u64) -> Result<u64, String> {
    let total = current
        .checked_add(added)
        .ok_or_else(|| "This skill is too large to import".to_owned())?;
    if total > MAX_SKILL_BYTES {
        return Err("A skill grew beyond the import limit while it was being copied".to_owned());
    }
    Ok(total)
}

fn read_bounded_file(directory: &Dir, name: &OsStr) -> Result<Vec<u8>, String> {
    let expected_len = regular_file_len(directory, name)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory
        .open_with(name, &options)
        .map_err(|_| "The file could not be opened".to_owned())?;
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Err("The file is not a regular file".to_owned());
    }
    let mut content = Vec::with_capacity(expected_len as usize);
    file.take(expected_len.saturating_add(1))
        .read_to_end(&mut content)
        .map_err(|_| "The file could not be read".to_owned())?;
    if content.len() as u64 != expected_len {
        return Err("The file changed while it was being read".to_owned());
    }
    Ok(content)
}

enum ManifestState {
    Missing,
    Regular,
    Invalid,
}

fn manifest_state(directory: &Dir) -> std::io::Result<ManifestState> {
    match directory.symlink_metadata(SKILL_MANIFEST_FILE) {
        Ok(metadata) => {
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                Ok(ManifestState::Regular)
            } else {
                Ok(ManifestState::Invalid)
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ManifestState::Missing),
        Err(error) => Err(error),
    }
}

fn open_or_create_absolute_directory(path: &Path) -> std::io::Result<Dir> {
    if let Ok(directory) = open_directory_nofollow(path) {
        return Ok(directory);
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let parent = open_directory_nofollow(parent)?;
    match parent.create_dir(name) {
        Ok(()) => parent.open_dir_nofollow(name),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            parent.open_dir_nofollow(name)
        }
        Err(error) => Err(error),
    }
}

fn open_directory_nofollow(path: &Path) -> std::io::Result<Dir> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    Dir::open_ambient_dir(parent, ambient_authority())?.open_dir_nofollow(name)
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Selected folder".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const MANIFEST: &str = "---\nname: meeting-notes\ndescription: Write concise meeting notes.\n---\n\n# Meeting notes\n";

    #[test]
    fn imports_one_skill_with_nested_scripts_references_and_assets() {
        let source = tempfile::tempdir().unwrap();
        let skill = source.path().join("meeting-notes");
        fs::create_dir_all(skill.join("scripts/nested")).unwrap();
        fs::create_dir_all(skill.join("references/guides")).unwrap();
        fs::create_dir_all(skill.join("assets")).unwrap();
        fs::write(skill.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();
        fs::write(skill.join("LICENSE"), "license").unwrap();
        fs::write(skill.join("scripts/render.sh"), "echo notes").unwrap();
        fs::write(skill.join("scripts/nested/helper.sh"), "echo helper").unwrap();
        fs::write(skill.join("references/guides/style.md"), "Be concise").unwrap();
        fs::write(skill.join("assets/icon.png"), [0_u8, 1, 2, 3]).unwrap();

        let destination = tempfile::tempdir().unwrap();
        let report =
            import_selected_folder(&skill, &destination.path().join("skills"), &HashSet::new())
                .unwrap();

        assert_eq!(report.imported, vec!["meeting-notes"]);
        assert!(report.skipped.is_empty());
        assert!(report.conflicts.is_empty());
        let imported = destination.path().join("skills/meeting-notes");
        assert_eq!(
            fs::read_to_string(imported.join("LICENSE")).unwrap(),
            "license"
        );
        assert_eq!(
            fs::read_to_string(imported.join("scripts/render.sh")).unwrap(),
            "echo notes"
        );
        assert_eq!(
            fs::read_to_string(imported.join("scripts/nested/helper.sh")).unwrap(),
            "echo helper"
        );
        assert_eq!(
            fs::read_to_string(imported.join("references/guides/style.md")).unwrap(),
            "Be concise"
        );
        assert_eq!(
            fs::read(imported.join("assets/icon.png")).unwrap(),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn publishes_the_bytes_that_were_validated() {
        let source = tempfile::tempdir().unwrap();
        let skill = source.path().join("meeting-notes");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();
        fs::write(skill.join("helper.txt"), "safe bytes").unwrap();

        let source_dir = open_directory_nofollow(&skill).unwrap();
        let prepared = prepare_skill(&source_dir).unwrap();
        fs::write(skill.join("helper.txt"), "evil bytes").unwrap();

        let destination = tempfile::tempdir().unwrap();
        let destination_root =
            open_or_create_absolute_directory(&destination.path().join("skills")).unwrap();
        destination_root.create_dir("meeting-notes").unwrap();
        publish_skill(&destination_root, "meeting-notes", &prepared).unwrap();

        assert_eq!(
            fs::read_to_string(destination.path().join("skills/meeting-notes/helper.txt")).unwrap(),
            "safe bytes"
        );
    }

    #[test]
    fn unrelated_folders_do_not_consume_the_skill_limit() {
        let source = tempfile::tempdir().unwrap();
        for index in 0..MAX_SKILLS_PER_IMPORT {
            fs::create_dir(source.path().join(format!("a-{index:02}"))).unwrap();
        }
        let skill = source.path().join("meeting-notes");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();

        let destination = tempfile::tempdir().unwrap();
        let report = import_selected_folder(
            source.path(),
            &destination.path().join("skills"),
            &HashSet::new(),
        )
        .unwrap();

        assert_eq!(report.imported, vec!["meeting-notes"]);
        assert!(destination
            .path()
            .join("skills/meeting-notes/SKILL.md")
            .is_file());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_skill_that_contains_a_symbolic_link() {
        let source = tempfile::tempdir().unwrap();
        let skill = source.path().join("meeting-notes");
        fs::create_dir_all(skill.join("references")).unwrap();
        fs::write(skill.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();
        fs::write(skill.join("references/style.md"), "Be concise").unwrap();
        std::os::unix::fs::symlink(
            skill.join("references/style.md"),
            skill.join("references/linked.md"),
        )
        .unwrap();

        let destination = tempfile::tempdir().unwrap();
        let report =
            import_selected_folder(&skill, &destination.path().join("skills"), &HashSet::new())
                .unwrap();

        assert!(report.imported.is_empty());
        assert_eq!(report.skipped[0].reason, "Symbolic links are not imported");
        assert!(!destination.path().join("skills/meeting-notes").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_path_execution_cannot_stage() {
        let source = tempfile::tempdir().unwrap();
        let skill = source.path().join("meeting-notes");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();
        fs::write(skill.join("bad\\name.txt"), "not portable").unwrap();

        let destination = tempfile::tempdir().unwrap();
        let report =
            import_selected_folder(&skill, &destination.path().join("skills"), &HashSet::new())
                .unwrap();

        assert!(report.imported.is_empty());
        assert_eq!(
            report.skipped[0].reason,
            "A skill contains an invalid or overly long path"
        );
    }

    #[test]
    fn a_parent_import_reports_invalid_and_conflicting_skills_without_overwriting() {
        let source = tempfile::tempdir().unwrap();
        let valid = source.path().join("meeting-notes");
        fs::create_dir_all(&valid).unwrap();
        fs::write(valid.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();
        fs::create_dir_all(source.path().join("missing-manifest")).unwrap();
        let mismatched = source.path().join("different-name");
        fs::create_dir_all(&mismatched).unwrap();
        fs::write(mismatched.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();

        let destination = tempfile::tempdir().unwrap();
        let existing = destination.path().join("skills/meeting-notes");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join(SKILL_MANIFEST_FILE), "keep me").unwrap();
        let report = import_selected_folder(
            source.path(),
            &destination.path().join("skills"),
            &HashSet::new(),
        )
        .unwrap();

        assert!(report.imported.is_empty());
        assert_eq!(report.conflicts[0].name, "meeting-notes");
        assert_eq!(report.skipped.len(), 2);
        assert_eq!(
            fs::read_to_string(existing.join(SKILL_MANIFEST_FILE)).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn a_builtin_name_is_a_conflict_even_when_the_user_tree_is_empty() {
        let source = tempfile::tempdir().unwrap();
        let skill = source.path().join("meeting-notes");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join(SKILL_MANIFEST_FILE), MANIFEST).unwrap();
        let destination = tempfile::tempdir().unwrap();

        let report = import_selected_folder(
            &skill,
            &destination.path().join("skills"),
            &HashSet::from(["meeting-notes".to_owned()]),
        )
        .unwrap();

        assert!(report.imported.is_empty());
        assert_eq!(report.conflicts[0].name, "meeting-notes");
        assert!(!destination.path().join("skills/meeting-notes").exists());
    }
}
