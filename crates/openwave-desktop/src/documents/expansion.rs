//! Bounded, native-only expansion for folders and ZIP archives.
//!
//! The renderer never receives source paths. Folder traversal uses directory
//! capabilities and never follows symlinks; archive entries are copied into
//! isolated temporary directories rather than extracted by their supplied
//! paths.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use tempfile::{Builder as TempDirBuilder, TempDir};
use zip::ZipArchive;

use super::{import_display_name, is_safe_title_char};

const MAX_DIRECTORY_DEPTH: usize = 8;
const MAX_IMPORT_FILES: usize = 500;
const MAX_IMPORT_RESULTS: usize = 1_000;
const MAX_DIRECTORY_ENTRIES: usize = 10_000;
const MAX_ZIP_ENTRIES: usize = 1_000;
const MAX_ZIP_ENTRY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ZIP_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

const BLOCKED_IMPORT_EXTENSIONS: &[&str] = &[
    "apk",
    "app",
    "appimage",
    "appx",
    "appxbundle",
    "bat",
    "bin",
    "cmd",
    "com",
    "cpl",
    "deb",
    "dmg",
    "exe",
    "gadget",
    "hta",
    "img",
    "ins",
    "ipa",
    "iso",
    "jar",
    "lnk",
    "msi",
    "msp",
    "mst",
    "pkg",
    "qcow",
    "qcow2",
    "rpm",
    "scr",
    "sparsebundle",
    "sparseimage",
    "sys",
    "vb",
    "vbe",
    "vbs",
    "vhd",
    "vhdx",
    "wim",
    "ws",
    "wsc",
    "wsf",
    "wsh",
];

#[derive(Clone, Copy)]
struct ExpansionLimits {
    max_directory_depth: usize,
    max_import_files: usize,
    max_directory_entries: usize,
    max_zip_entries: usize,
    max_zip_entry_bytes: u64,
    max_zip_total_bytes: u64,
}

impl Default for ExpansionLimits {
    fn default() -> Self {
        Self {
            max_directory_depth: MAX_DIRECTORY_DEPTH,
            max_import_files: MAX_IMPORT_FILES,
            max_directory_entries: MAX_DIRECTORY_ENTRIES,
            max_zip_entries: MAX_ZIP_ENTRIES,
            max_zip_entry_bytes: MAX_ZIP_ENTRY_BYTES,
            max_zip_total_bytes: MAX_ZIP_TOTAL_BYTES,
        }
    }
}

pub(super) enum ExpandedImportItem {
    File {
        source: ExpandedFile,
        display_name: String,
    },
    Failure {
        display_name: String,
        message: String,
    },
}

pub(super) enum ExpandedFile {
    Path(PathBuf),
    Open(File),
}

pub(super) struct ExpandedImports {
    items: Vec<ExpandedImportItem>,
    temp_dir: Option<TempDir>,
}

impl ExpandedImports {
    pub(super) fn into_parts(self) -> (Vec<ExpandedImportItem>, Option<TempDir>) {
        (self.items, self.temp_dir)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExpansionCancelled;

pub(super) fn expand_import_paths(
    paths: Vec<PathBuf>,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ExpandedImports, ExpansionCancelled> {
    expand_import_paths_with_limits(paths, ExpansionLimits::default(), is_cancelled)
}

fn expand_import_paths_with_limits(
    paths: Vec<PathBuf>,
    limits: ExpansionLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<ExpandedImports, ExpansionCancelled> {
    let mut expander = Expander {
        limits,
        is_cancelled,
        items: Vec::new(),
        temp_dir: None,
        imported_files: 0,
        visited_directory_entries: 0,
        temp_file_sequence: 0,
        stopped: false,
        limit_reported: false,
    };
    for path in paths {
        expander.check_cancelled()?;
        if expander.stopped {
            break;
        }
        expander.expand_root(path)?;
    }
    Ok(ExpandedImports {
        items: expander.items,
        temp_dir: expander.temp_dir,
    })
}

struct Expander<'a> {
    limits: ExpansionLimits,
    is_cancelled: &'a dyn Fn() -> bool,
    items: Vec<ExpandedImportItem>,
    temp_dir: Option<TempDir>,
    imported_files: usize,
    visited_directory_entries: usize,
    temp_file_sequence: usize,
    stopped: bool,
    limit_reported: bool,
}

impl Expander<'_> {
    fn check_cancelled(&self) -> Result<(), ExpansionCancelled> {
        if (self.is_cancelled)() {
            Err(ExpansionCancelled)
        } else {
            Ok(())
        }
    }

    fn expand_root(&mut self, path: PathBuf) -> Result<(), ExpansionCancelled> {
        let display_name =
            import_display_name(&path).unwrap_or_else(|_| "Selected source".to_owned());
        if path_component_is_skipped(&display_name) {
            return Ok(());
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                self.push_failure(display_name, "Could not read the selected source");
                return Ok(());
            }
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            self.push_failure(
                display_name,
                "Aliases and symbolic links cannot be imported",
            );
        } else if file_type.is_dir() {
            match open_directory_nofollow(&path) {
                Ok(directory) => self.walk_directory(&path, &directory, Path::new(""), 0)?,
                Err(_) => self.push_failure(display_name, "Could not read the selected folder"),
            }
        } else if file_type.is_file() {
            self.expand_path_file(path, display_name)?;
        } else {
            self.push_failure(display_name, "Choose a file or folder to import");
        }
        Ok(())
    }

    fn walk_directory(
        &mut self,
        root_path: &Path,
        directory: &Dir,
        relative_path: &Path,
        depth: usize,
    ) -> Result<(), ExpansionCancelled> {
        self.check_cancelled()?;
        let entries = match directory.entries() {
            Ok(entries) => entries,
            Err(_) => {
                self.push_failure(
                    directory_display_name(root_path, relative_path),
                    "Could not read this folder",
                );
                return Ok(());
            }
        };
        let mut entries = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let file_name = entry.file_name();
                let sort_name = file_name.to_str()?.to_owned();
                Some((sort_name, file_name, entry))
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));

        for (name, os_name, entry) in entries {
            self.check_cancelled()?;
            if self.stopped {
                break;
            }
            self.visited_directory_entries += 1;
            if self.visited_directory_entries > self.limits.max_directory_entries {
                self.stop_at_limit(
                    directory_display_name(root_path, relative_path),
                    format!(
                        "Folder expansion is limited to {} entries",
                        self.limits.max_directory_entries
                    ),
                );
                break;
            }
            if path_component_is_skipped(&name) {
                continue;
            }
            let child_relative = relative_path.join(&os_name);
            let child_path = root_path.join(&child_relative);
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    self.push_failure(name, "Could not inspect this folder entry");
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth >= self.limits.max_directory_depth {
                    self.push_failure(
                        name,
                        format!(
                            "Folder nesting is limited to {} levels",
                            self.limits.max_directory_depth
                        ),
                    );
                    continue;
                }
                match directory.open_dir_nofollow(&os_name) {
                    Ok(child) => {
                        self.walk_directory(root_path, &child, &child_relative, depth + 1)?
                    }
                    Err(_) => self.push_failure(name, "Could not read this folder"),
                }
            } else if file_type.is_file() {
                self.expand_directory_file(directory, Path::new(&os_name), child_path, name)?;
            }
        }
        Ok(())
    }

    fn expand_path_file(
        &mut self,
        path: PathBuf,
        display_name: String,
    ) -> Result<(), ExpansionCancelled> {
        self.check_cancelled()?;
        if path_component_is_skipped(&display_name)
            || has_blocked_import_extension(Path::new(&display_name))
        {
            return Ok(());
        }
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            match open_regular_file_nofollow(&path) {
                Ok(file) => self.expand_zip(file, display_name),
                Err(_) => {
                    self.push_failure(display_name, "Could not read this archive");
                    Ok(())
                }
            }
        } else {
            self.push_file(ExpandedFile::Path(path), display_name);
            Ok(())
        }
    }

    fn expand_directory_file(
        &mut self,
        directory: &Dir,
        file_name: &Path,
        path: PathBuf,
        display_name: String,
    ) -> Result<(), ExpansionCancelled> {
        self.check_cancelled()?;
        if path_component_is_skipped(&display_name)
            || has_blocked_import_extension(Path::new(&display_name))
        {
            return Ok(());
        }
        let file = match open_directory_file_nofollow(directory, file_name) {
            Ok(file) => file,
            Err(_) => {
                self.push_failure(display_name, "Could not read this document");
                return Ok(());
            }
        };
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            self.expand_zip(file, display_name)
        } else {
            self.push_file(ExpandedFile::Open(file), display_name);
            Ok(())
        }
    }

    fn expand_zip(
        &mut self,
        archive_file: File,
        archive_name: String,
    ) -> Result<(), ExpansionCancelled> {
        let fallback_file = match archive_file.try_clone() {
            Ok(file) => file,
            Err(_) => {
                self.push_failure(archive_name, "Could not read this archive");
                return Ok(());
            }
        };
        let mut archive = match ZipArchive::new(archive_file) {
            Ok(archive) => archive,
            Err(_) => {
                self.push_file(ExpandedFile::Open(fallback_file), archive_name);
                return Ok(());
            }
        };
        if archive.len() > self.limits.max_zip_entries {
            self.push_file(ExpandedFile::Open(fallback_file), archive_name);
            return Ok(());
        }

        let mut seen_names = HashSet::new();
        let mut plans = Vec::new();
        let mut declared_total = 0_u64;
        for index in 0..archive.len() {
            self.check_cancelled()?;
            let file = match archive.by_index(index) {
                Ok(file) => file,
                Err(_) => continue,
            };
            if file.is_dir() || file.is_symlink() || file.encrypted() || file.size() == 0 {
                continue;
            }
            let Some((normalized_path, display_name)) = safe_zip_entry_name(file.name()) else {
                continue;
            };
            if !seen_names.insert(normalized_path.to_lowercase()) {
                continue;
            }
            if has_blocked_import_extension(Path::new(&display_name)) {
                continue;
            }
            if file.size() > self.limits.max_zip_entry_bytes {
                plans.push(ArchivePlan::Failure {
                    normalized_path,
                    display_name,
                    message: format!(
                        "Archive entries are limited to {} MiB",
                        self.limits.max_zip_entry_bytes / (1024 * 1024)
                    ),
                });
                continue;
            }
            let Some(next_total) = declared_total.checked_add(file.size()) else {
                self.push_file(ExpandedFile::Open(fallback_file), archive_name);
                return Ok(());
            };
            if next_total > self.limits.max_zip_total_bytes {
                self.push_file(ExpandedFile::Open(fallback_file), archive_name);
                return Ok(());
            }
            declared_total = next_total;
            plans.push(ArchivePlan::File {
                normalized_path,
                display_name,
                index,
                declared_size: file.size(),
            });
        }
        if !plans
            .iter()
            .any(|plan| matches!(plan, ArchivePlan::File { .. }))
        {
            self.push_file(ExpandedFile::Open(fallback_file), archive_name);
            return Ok(());
        }
        plans.sort_by(|left, right| left.normalized_path().cmp(right.normalized_path()));

        let mut extracted_files = 0;
        let mut extracted_bytes = 0_u64;
        for plan in plans {
            self.check_cancelled()?;
            if self.stopped {
                break;
            }
            match plan {
                ArchivePlan::Failure {
                    display_name,
                    message,
                    ..
                } => self.push_failure(display_name, message),
                ArchivePlan::File {
                    display_name,
                    index,
                    declared_size,
                    ..
                } => {
                    let output_path = match self.next_temp_path(&display_name) {
                        Ok(path) => path,
                        Err(message) => {
                            self.push_failure(display_name, message);
                            continue;
                        }
                    };
                    let mut entry = match archive.by_index(index) {
                        Ok(entry) => entry,
                        Err(_) => {
                            self.push_failure(display_name, "Could not read this archive entry");
                            continue;
                        }
                    };
                    match copy_archive_entry(
                        &mut entry,
                        &output_path,
                        declared_size,
                        self.limits.max_zip_entry_bytes,
                        self.limits
                            .max_zip_total_bytes
                            .saturating_sub(extracted_bytes),
                        self.is_cancelled,
                    ) {
                        Ok(copied) => {
                            extracted_bytes += copied;
                            self.push_file(ExpandedFile::Path(output_path), display_name);
                            extracted_files += 1;
                        }
                        Err(CopyArchiveError::Cancelled) => return Err(ExpansionCancelled),
                        Err(CopyArchiveError::Failed) => {
                            self.push_failure(display_name, "Could not read this archive entry")
                        }
                    }
                }
            }
        }
        if extracted_files == 0 && !self.stopped {
            self.push_file(ExpandedFile::Open(fallback_file), archive_name);
        }
        Ok(())
    }

    fn next_temp_path(&mut self, display_name: &str) -> Result<PathBuf, &'static str> {
        if self.temp_dir.is_none() {
            self.temp_dir = Some(
                TempDirBuilder::new()
                    .prefix("openwave-source-import-")
                    .tempdir()
                    .map_err(|_| "Could not prepare this archive")?,
            );
        }
        let sequence = self.temp_file_sequence;
        self.temp_file_sequence += 1;
        let directory = self
            .temp_dir
            .as_ref()
            .expect("temporary directory was initialized")
            .path()
            .join(format!("{sequence:08}"));
        std::fs::create_dir(&directory).map_err(|_| "Could not prepare this archive")?;
        let file_name = Path::new(display_name)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Could not prepare this archive")?;
        Ok(directory.join(file_name))
    }

    fn push_file(&mut self, source: ExpandedFile, display_name: String) {
        if self.imported_files >= self.limits.max_import_files
            || self.items.len() >= MAX_IMPORT_RESULTS
        {
            self.stop_at_limit(
                display_name,
                format!(
                    "One import is limited to {} files",
                    self.limits.max_import_files
                ),
            );
            return;
        }
        self.imported_files += 1;
        self.items.push(ExpandedImportItem::File {
            source,
            display_name,
        });
    }

    fn push_failure(&mut self, display_name: impl Into<String>, message: impl Into<String>) {
        if self.items.len() < MAX_IMPORT_RESULTS {
            self.items.push(ExpandedImportItem::Failure {
                display_name: display_name.into(),
                message: message.into(),
            });
        }
    }

    fn stop_at_limit(&mut self, display_name: String, message: String) {
        if !self.limit_reported {
            self.push_failure(display_name, message);
            self.limit_reported = true;
        }
        self.stopped = true;
    }
}

enum ArchivePlan {
    File {
        normalized_path: String,
        display_name: String,
        index: usize,
        declared_size: u64,
    },
    Failure {
        normalized_path: String,
        display_name: String,
        message: String,
    },
}

impl ArchivePlan {
    fn normalized_path(&self) -> &str {
        match self {
            Self::File {
                normalized_path, ..
            }
            | Self::Failure {
                normalized_path, ..
            } => normalized_path,
        }
    }
}

enum CopyArchiveError {
    Cancelled,
    Failed,
}

fn copy_archive_entry(
    source: &mut impl Read,
    destination: &Path,
    declared_size: u64,
    max_entry_bytes: u64,
    max_total_bytes: u64,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<u64, CopyArchiveError> {
    let mut destination = File::create_new(destination).map_err(|_| CopyArchiveError::Failed)?;
    let mut copied = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        if is_cancelled() {
            return Err(CopyArchiveError::Cancelled);
        }
        let read = source
            .read(&mut buffer)
            .map_err(|_| CopyArchiveError::Failed)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).expect("copy buffer length fits u64"))
            .ok_or(CopyArchiveError::Failed)?;
        if copied > max_entry_bytes || copied > max_total_bytes {
            return Err(CopyArchiveError::Failed);
        }
        destination
            .write_all(&buffer[..read])
            .map_err(|_| CopyArchiveError::Failed)?;
    }
    if copied != declared_size {
        return Err(CopyArchiveError::Failed);
    }
    destination.flush().map_err(|_| CopyArchiveError::Failed)?;
    Ok(copied)
}

fn safe_zip_entry_name(raw_name: &str) -> Option<(String, String)> {
    if raw_name.is_empty()
        || raw_name.starts_with('/')
        || raw_name.contains('\\')
        || raw_name.contains('\0')
    {
        return None;
    }
    let components = raw_name.split('/').collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|component| zip_component_is_ambiguous(component))
        || components
            .iter()
            .any(|component| path_component_is_skipped(component))
    {
        return None;
    }
    let normalized_path = components.join("/");
    let display_name = if safe_display_name(&normalized_path) {
        normalized_path.clone()
    } else {
        let file_name = components.last().copied()?;
        safe_display_name(file_name).then(|| file_name.to_owned())?
    };
    Some((normalized_path, display_name))
}

fn zip_component_is_ambiguous(component: &str) -> bool {
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.contains(':')
        || component.ends_with(' ')
        || component.ends_with('.')
    {
        return true;
    }
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem)
        .to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || stem
            .strip_prefix("com")
            .or_else(|| stem.strip_prefix("lpt"))
            .is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'))
}

fn path_component_is_skipped(component: &str) -> bool {
    component.starts_with('.')
        || component.starts_with("~$")
        || component.eq_ignore_ascii_case("__MACOSX")
}

fn safe_display_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= 255 && name.chars().all(is_safe_title_char)
}

fn directory_display_name(root_path: &Path, relative_path: &Path) -> String {
    relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| safe_display_name(name))
        .map(str::to_owned)
        .or_else(|| import_display_name(root_path).ok())
        .unwrap_or_else(|| "Selected folder".to_owned())
}

fn open_directory_nofollow(path: &Path) -> std::io::Result<Dir> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    Dir::open_ambient_dir(parent, ambient_authority())?.open_dir_nofollow(file_name)
}

fn open_regular_file_nofollow(path: &Path) -> std::io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory.open_with(file_name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    Ok(file.into_std())
}

fn open_directory_file_nofollow(directory: &Dir, path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory.open_with(path, &options)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    Ok(file.into_std())
}

pub(super) fn has_blocked_import_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            BLOCKED_IMPORT_EXTENSIONS
                .iter()
                .any(|blocked| extension.eq_ignore_ascii_case(blocked))
        })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    use super::*;

    #[test]
    fn folders_are_sorted_bounded_and_skip_non_documents_without_following_links() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sources");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("b.md"), "b").unwrap();
        std::fs::write(root.join("a.md"), "a").unwrap();
        std::fs::write(root.join(".hidden.md"), "hidden").unwrap();
        std::fs::write(root.join("~$notes.docx"), "lock").unwrap();
        std::fs::write(root.join("installer.EXE"), b"%PDF executable").unwrap();
        std::fs::create_dir(root.join("__MACOSX")).unwrap();
        std::fs::write(root.join("__MACOSX").join("metadata"), "metadata").unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested").join("c.txt"), "c").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("nested"), root.join("linked")).unwrap();

        let expanded = expand_import_paths(vec![root.clone()], &|| false).unwrap();
        assert_eq!(file_names(&expanded.items), ["a.md", "b.md", "c.txt"]);

        std::fs::rename(root.join("a.md"), root.join("a-original.md")).unwrap();
        std::fs::write(root.join("a.md"), "replacement").unwrap();
        let original = expanded
            .items
            .iter()
            .find_map(|item| match item {
                ExpandedImportItem::File {
                    source: ExpandedFile::Open(file),
                    display_name,
                } if display_name == "a.md" => Some(file),
                _ => None,
            })
            .unwrap();
        let mut original = original.try_clone().unwrap();
        let mut contents = String::new();
        original.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "a");
        std::fs::remove_file(root.join("a.md")).unwrap();
        std::fs::rename(root.join("a-original.md"), root.join("a.md")).unwrap();

        let limited = expand_import_paths_with_limits(
            vec![root.clone()],
            ExpansionLimits {
                max_import_files: 2,
                ..ExpansionLimits::default()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(file_names(&limited.items), ["a.md", "b.md"]);
        assert!(failures(&limited.items)
            .iter()
            .any(|(_, message)| message.contains("limited to 2 files")));

        let depth_limited = expand_import_paths_with_limits(
            vec![root],
            ExpansionLimits {
                max_directory_depth: 0,
                ..ExpansionLimits::default()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(file_names(&depth_limited.items), ["a.md", "b.md"]);
        assert!(failures(&depth_limited.items)
            .iter()
            .any(|(_, message)| message.contains("limited to 0 levels")));
    }

    #[test]
    fn zip_entries_are_safely_sorted_and_an_all_skipped_archive_is_retained() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("sources.zip");
        write_zip(
            &archive_path,
            &[
                ("b.md", b"b"),
                ("../escape.md", b"escape"),
                ("folder\\ambiguous.md", b"ambiguous"),
                ("__MACOSX/metadata", b"metadata"),
                (".hidden.md", b"hidden"),
                ("run.exe", b"%PDF executable"),
                ("folder/a.md", b"a"),
            ],
            true,
        );

        let expanded = expand_import_paths(vec![archive_path], &|| false).unwrap();
        assert_eq!(file_names(&expanded.items), ["b.md", "folder/a.md"]);
        assert!(!directory.path().join("escape.md").exists());

        let skipped_path = directory.path().join("skipped.zip");
        write_zip(
            &skipped_path,
            &[(".hidden.md", b"hidden"), ("run.exe", b"binary")],
            false,
        );
        let skipped = expand_import_paths(vec![skipped_path], &|| false).unwrap();
        assert_eq!(file_names(&skipped.items), ["skipped.zip"]);
    }

    #[test]
    fn zip_count_and_size_limits_retain_the_archive_or_report_partial_failure() {
        let directory = tempfile::tempdir().unwrap();
        let archive_path = directory.path().join("many.zip");
        write_zip(
            &archive_path,
            &[("a.md", b"a"), ("b.md", b"bb"), ("c.md", b"ccc")],
            false,
        );
        let count_limited = expand_import_paths_with_limits(
            vec![archive_path.clone()],
            ExpansionLimits {
                max_zip_entries: 2,
                ..ExpansionLimits::default()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(file_names(&count_limited.items), ["many.zip"]);

        let size_limited = expand_import_paths_with_limits(
            vec![archive_path],
            ExpansionLimits {
                max_zip_entry_bytes: 1,
                ..ExpansionLimits::default()
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(file_names(&size_limited.items), ["a.md"]);
        assert_eq!(
            failures(&size_limited.items)
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["b.md", "c.md"]
        );
    }

    #[test]
    fn expansion_observes_cancellation_between_sorted_entries() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("sources");
        std::fs::create_dir(&root).unwrap();
        for name in ["a.md", "b.md", "c.md"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        let checks = std::cell::Cell::new(0);
        let result = expand_import_paths(vec![root], &|| {
            let next = checks.get() + 1;
            checks.set(next);
            next >= 4
        });
        assert_eq!(result.err(), Some(ExpansionCancelled));
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])], include_symlink: bool) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        for (name, bytes) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        if include_symlink {
            writer
                .add_symlink("linked.md", "folder/a.md", SimpleFileOptions::default())
                .unwrap();
        }
        writer.finish().unwrap();
    }

    fn file_names(items: &[ExpandedImportItem]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|item| match item {
                ExpandedImportItem::File { display_name, .. } => Some(display_name.as_str()),
                ExpandedImportItem::Failure { .. } => None,
            })
            .collect()
    }

    fn failures(items: &[ExpandedImportItem]) -> Vec<(&String, &String)> {
        items
            .iter()
            .filter_map(|item| match item {
                ExpandedImportItem::Failure {
                    display_name,
                    message,
                } => Some((display_name, message)),
                ExpandedImportItem::File { .. } => None,
            })
            .collect()
    }
}
