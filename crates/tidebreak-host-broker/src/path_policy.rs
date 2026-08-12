//! Cross-platform root registration and containment policy.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf, Prefix},
};

use cap_fs_ext::DirExt;
use cap_std::{ambient_authority, fs::Dir};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Why a host folder cannot be opened as a registered root.
#[derive(Debug, Error)]
pub enum RootPolicyError {
    /// The candidate is a filesystem root, home directory, or an ancestor that
    /// would grant much more authority than one project folder.
    #[error("root is not a specific project folder")]
    TooBroad,
    /// Trusted consent must name the exact absolute picker result. Resolving a
    /// relative control path against broker process state would change what the
    /// user selected.
    #[error("root must be an absolute host path")]
    NotAbsolute,
    /// The candidate overlaps a protected system location.
    #[error("root overlaps a protected system location")]
    SensitiveLocation,
    /// The candidate is an entire OS user profile.
    #[error("root must be a folder inside a user profile, not the whole profile")]
    WholeUserProfile,
    /// Host filesystem resolution or descriptor pinning failed.
    #[error("could not securely open root: {0}")]
    Io(#[from] io::Error),
    /// This target has no reviewed host-root policy and must fail closed.
    #[error("host roots are unsupported on this target")]
    UnsupportedPlatform,
    /// Windows device and general verbatim namespaces can alias protected
    /// volumes without a stable drive or share identity.
    #[error("root uses an unsupported filesystem namespace")]
    UnsupportedNamespace,
    /// A required Windows known-folder location was unavailable. Root policy
    /// must not silently become less restrictive when the host environment is
    /// incomplete.
    #[error("required host policy location is unavailable: {0}")]
    MissingKnownFolder(&'static str),
    /// A UNC root reached a hidden administrative share (`C$`, `ADMIN$`) or a
    /// loopback server name. Either one reaches a local volume the drive-letter
    /// policy protects, under a name that policy cannot recognize.
    #[error("root uses an administrative or loopback network share")]
    AdministrativeShare,
    /// A canonical Windows path component that can name something other than
    /// the directory it appears to name: an alternate data stream, a reserved
    /// device name, or a name Windows silently rewrites.
    #[error("root path contains a non-portable Windows component")]
    NonPortableComponent,
    /// The canonical path is not already its long-name form, so it may be an
    /// 8.3 short name aliasing a different directory than policy compared.
    #[error("root path could not be confirmed as its long-name form")]
    ShortNameAlias,
}

/// A canonical root pinned to a descriptor after policy validation.
///
/// Its absolute path and directory handle remain crate-private. Future broker
/// operations consume the handle directly; callers cannot turn validation into
/// a reusable path string or substitute a different directory after the check.
pub struct ValidatedRoot {
    _canonical_path: PathBuf,
    _directory: Dir,
    identity: RootIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub(crate) enum RootIdentity {
    Unix { device: u64, inode: u64 },
    Windows { volume: u32, file_index: u64 },
}

impl std::fmt::Debug for ValidatedRoot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatedRoot")
            .finish_non_exhaustive()
    }
}

impl ValidatedRoot {
    pub(crate) fn canonical_path(&self) -> &Path {
        &self._canonical_path
    }

    pub(crate) fn directory(&self) -> &Dir {
        &self._directory
    }

    pub(crate) const fn identity(&self) -> RootIdentity {
        self.identity
    }

    /// Re-open the registered canonical path without following links and
    /// require it to still name the descriptor-pinned directory.
    ///
    /// Most broker operations use the pinned descriptor directly. Native exec
    /// profiles require a path string, so this closes the rename-and-replace
    /// gap before that path leaves the broker.
    pub(crate) fn canonical_path_if_current(&self) -> io::Result<&Path> {
        let current = open_canonical_dir_nofollow(&self._canonical_path)?;
        if root_identity(&current)? != self.identity {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "registered root identity changed",
            ));
        }
        Ok(&self._canonical_path)
    }
}

/// Which canonical host folders the user may register at all.
#[derive(Debug, Clone)]
pub struct RootPolicy {
    sensitive: Vec<PathBuf>,
    user_containers: Vec<PathBuf>,
    broad: Vec<PathBuf>,
    home: PathBuf,
}

impl RootPolicy {
    /// Build the reviewed policy for the current desktop host platform.
    ///
    /// The home directory is resolved through the filesystem here instead of
    /// read from ambient environment. Unsupported targets return an error rather
    /// than receiving an empty allow-most policy.
    pub fn for_host(home: PathBuf) -> Result<Self, RootPolicyError> {
        let home = canonicalize_host_path(&home)?;
        #[cfg(unix)]
        let paths = |items: &[&str]| items.iter().map(PathBuf::from).collect();

        #[cfg(target_os = "macos")]
        let policy = Self {
            sensitive: paths(&[
                "/bin",
                "/sbin",
                "/usr",
                "/etc",
                "/var",
                "/dev",
                "/System",
                "/Library",
                "/Applications",
                "/cores",
                "/private",
                "/opt",
            ]),
            user_containers: paths(&["/Users"]),
            broad: paths(&["/Volumes", "/Network"]),
            home,
        };

        #[cfg(all(unix, not(target_os = "macos")))]
        let policy = Self {
            sensitive: paths(&[
                "/bin", "/sbin", "/usr", "/etc", "/var", "/lib", "/lib64", "/lib32", "/boot",
                "/proc", "/sys", "/dev", "/run", "/srv", "/opt", "/root",
            ]),
            user_containers: paths(&["/home"]),
            broad: paths(&["/mnt", "/media"]),
            home,
        };

        #[cfg(windows)]
        let policy = {
            let sensitive = vec![
                canonical_windows_known_folder(&["SystemRoot", "windir"])?,
                canonical_windows_known_folder(&["ProgramFiles"])?,
                canonical_windows_known_folder(&["ProgramFiles(x86)"])?,
                canonical_windows_known_folder(&["ProgramData"])?,
            ];
            let profile_container = home
                .parent()
                .ok_or(RootPolicyError::WholeUserProfile)?
                .to_path_buf();
            Self {
                sensitive,
                user_containers: vec![profile_container],
                broad: Vec::new(),
                home,
            }
        };

        #[cfg(not(any(unix, windows)))]
        return Err(RootPolicyError::UnsupportedPlatform);

        #[cfg(any(unix, windows))]
        Ok(policy)
    }

    /// Resolve, validate, and descriptor-pin one user-selected directory.
    ///
    /// Canonicalization prevents a selected symlink from disguising a protected
    /// target. The canonical path is then opened one component at a time without
    /// following links, closing the replacement race between validation and the
    /// descriptor used by later operations.
    pub fn open_root(&self, candidate: &Path) -> Result<ValidatedRoot, RootPolicyError> {
        if !candidate.is_absolute() {
            return Err(RootPolicyError::NotAbsolute);
        }
        let canonical_path = canonicalize_host_path(candidate)?;
        self.validate_canonical(&canonical_path)?;
        let directory = open_canonical_dir_nofollow(&canonical_path)?;
        let identity = root_identity(&directory)?;
        Ok(ValidatedRoot {
            _canonical_path: canonical_path,
            _directory: directory,
            identity,
        })
    }

    /// Add an application-private directory that connected roots must never
    /// contain or overlap, such as the broker's own state and audit directory.
    pub fn with_private_directory(mut self, path: &Path) -> Result<Self, RootPolicyError> {
        if !path.is_absolute() {
            return Err(RootPolicyError::NotAbsolute);
        }
        self.sensitive.push(canonicalize_host_path(path)?);
        Ok(self)
    }

    fn validate_canonical(&self, root: &Path) -> Result<(), RootPolicyError> {
        if !root.is_absolute() || is_filesystem_root(root) {
            return Err(RootPolicyError::TooBroad);
        }
        if !supported_canonical_namespace(root) {
            return Err(RootPolicyError::UnsupportedNamespace);
        }
        if cfg!(windows) {
            validate_windows_shape(root)?;
        }
        if is_within(root, &self.home) {
            return Err(RootPolicyError::TooBroad);
        }
        if self
            .sensitive
            .iter()
            .any(|path| is_within(path, root) || is_within(root, path))
        {
            return Err(RootPolicyError::SensitiveLocation);
        }
        for container in &self.user_containers {
            if is_within(root, container) {
                return Err(RootPolicyError::TooBroad);
            }
            if is_direct_child(container, root) {
                return Err(RootPolicyError::WholeUserProfile);
            }
        }
        if self.broad.iter().any(|path| is_within(root, path)) {
            return Err(RootPolicyError::TooBroad);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        home: PathBuf,
        sensitive: Vec<PathBuf>,
        user_containers: Vec<PathBuf>,
        broad: Vec<PathBuf>,
    ) -> Self {
        Self {
            sensitive,
            user_containers,
            broad,
            home,
        }
    }
}

#[cfg(unix)]
fn root_identity(directory: &Dir) -> io::Result<RootIdentity> {
    use cap_fs_ext::MetadataExt;

    let metadata = directory.dir_metadata()?;
    Ok(RootIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn root_identity(directory: &Dir) -> io::Result<RootIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let succeeded =
        unsafe { GetFileInformationByHandle(directory.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(RootIdentity::Windows {
        volume: information.dwVolumeSerialNumber,
        file_index: u64::from(information.nFileIndexHigh) << 32
            | u64::from(information.nFileIndexLow),
    })
}

#[cfg(not(any(unix, windows)))]
fn root_identity(_directory: &Dir) -> io::Result<RootIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "root identity is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn canonical_windows_known_folder(
    variables: &'static [&'static str],
) -> Result<PathBuf, RootPolicyError> {
    let path = windows_known_folder_from(variables, |name| std::env::var_os(name))?;
    canonicalize_host_path(&path)
}

#[cfg(windows)]
fn windows_known_folder_from(
    variables: &'static [&'static str],
    get: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, RootPolicyError> {
    let path = variables
        .iter()
        .find_map(|name| get(name).map(PathBuf::from))
        .filter(|path| path.is_absolute())
        .ok_or(RootPolicyError::MissingKnownFolder(variables[0]))?;
    Ok(path)
}

fn supported_canonical_namespace(path: &Path) -> bool {
    #[cfg(windows)]
    {
        path.components().find_map(|component| match component {
            Component::Prefix(prefix) => Some(matches!(
                prefix.kind(),
                Prefix::Disk(_)
                    | Prefix::VerbatimDisk(_)
                    | Prefix::UNC(_, _)
                    | Prefix::VerbatimUNC(_, _)
            )),
            _ => None,
        }) == Some(true)
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        true
    }
}

/// Resolve a host path to its canonical form, refusing anything Windows could
/// still resolve somewhere else.
///
/// `std::fs::canonicalize` already resolves symlinks, junctions, and 8.3 short
/// names — on Windows it opens the path and asks the kernel for the final name.
/// The extra step here does not repeat that work; it refuses to trust it
/// silently. Every path policy compares — the home directory, the known folders
/// it protects, the broker's private directory, and each candidate root — goes
/// through this one function, so all sides of every comparison are in the same
/// long-name form.
fn canonicalize_host_path(path: &Path) -> Result<PathBuf, RootPolicyError> {
    let canonical = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    require_long_name_form(&canonical)?;
    Ok(canonical)
}

/// Require a canonical Windows path to already equal its long-name expansion.
///
/// `C:\PROGRA~1` and `C:\Program Files` name one directory and compare unequal,
/// so a short name reaching a protected location would pass a policy written in
/// long names. Canonicalization is expected to have expanded it; this confirms
/// it did rather than assuming. The path was just opened successfully by
/// `canonicalize`, so a failure here means the host cannot tell us what this
/// path names, and the registration is refused.
#[cfg(windows)]
fn require_long_name_form(canonical: &Path) -> Result<(), RootPolicyError> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;

    let mut wide: Vec<u16> = canonical.as_os_str().encode_wide().collect();
    wide.push(0);
    let required = unsafe { GetLongPathNameW(wide.as_ptr(), std::ptr::null_mut(), 0) };
    if required == 0 {
        return Err(RootPolicyError::ShortNameAlias);
    }
    let mut buffer = vec![0u16; required as usize];
    let written = unsafe { GetLongPathNameW(wide.as_ptr(), buffer.as_mut_ptr(), required) };
    // A second call that needs more room than the first reported means the path
    // changed underneath us.
    if written == 0 || written >= required {
        return Err(RootPolicyError::ShortNameAlias);
    }
    buffer.truncate(written as usize);
    let long = PathBuf::from(OsString::from_wide(&buffer));
    if !same_canonical_path(&long, canonical) {
        return Err(RootPolicyError::ShortNameAlias);
    }
    Ok(())
}

/// Refuse canonical Windows paths whose components can name something other
/// than the directory they appear to name.
///
/// Nothing rejected here can be an ordinary NTFS directory: `:`, `<>"|?*`, and
/// control characters are not legal in a name, reserved device names cannot be
/// created, and Windows strips trailing dots and spaces rather than storing
/// them. A canonical path containing one is therefore not a folder the user
/// picked — it is a stream reference, a device, or a name that will resolve
/// differently the next time it is opened.
fn validate_windows_shape(root: &Path) -> Result<(), RootPolicyError> {
    for component in root.components() {
        match component {
            Component::Prefix(prefix) if !is_addressable_share(prefix.kind()) => {
                return Err(RootPolicyError::AdministrativeShare);
            }
            Component::Normal(segment)
                if !portable_windows_component(&segment.to_string_lossy()) =>
            {
                return Err(RootPolicyError::NonPortableComponent);
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether a UNC prefix names a share the user could have meant.
///
/// `\\localhost\C$\Windows` and `\\host\ADMIN$` reach the same bytes as the
/// drive-letter paths the policy protects, under a name no drive-letter
/// comparison can match. Non-UNC prefixes are the namespace check's business.
fn is_addressable_share(prefix: Prefix<'_>) -> bool {
    match prefix {
        Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
            !is_hidden_share(&share.to_string_lossy())
                && !is_loopback_server(&server.to_string_lossy())
        }
        _ => true,
    }
}

/// Whether one path component of a canonical Windows path names exactly the
/// directory it spells.
fn portable_windows_component(segment: &str) -> bool {
    // A colon opens an alternate data stream (`Documents:evil`) or a
    // drive-relative path; either way the component no longer names the
    // directory the rest of the policy compared.
    !segment.is_empty()
        && !segment.contains(':')
        && crate::relative_path::portable_filename(segment)
}

/// Administrative and hidden shares end in `$` — `C$`, `ADMIN$`, `IPC$`.
fn is_hidden_share(share: &str) -> bool {
    share.ends_with('$')
}

/// Server names that route a UNC path back to this machine's own volumes.
fn is_loopback_server(server: &str) -> bool {
    let server = server.trim_start_matches('[').trim_end_matches(']');
    server.eq_ignore_ascii_case("localhost")
        || server.eq_ignore_ascii_case("::1")
        || server == "."
        || server
            .split_once('.')
            .is_some_and(|(first, rest)| first == "127" && rest.chars().any(|c| c.is_ascii_digit()))
}

/// Whether two absolute paths name the same location under host path
/// comparison rules.
#[cfg(windows)]
fn same_canonical_path(left: &Path, right: &Path) -> bool {
    let left = normalized_components(left);
    let right = normalized_components(right);
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(&left, &right)| component_eq(left, right))
}

fn open_canonical_dir_nofollow(path: &Path) -> io::Result<Dir> {
    let mut anchor = PathBuf::new();
    let mut segments: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => anchor.push(prefix.as_os_str()),
            Component::RootDir => anchor.push(component.as_os_str()),
            Component::Normal(segment) => segments.push(segment.to_owned()),
            Component::CurDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "canonical root contained a relative component",
                ));
            }
        }
    }
    if anchor.as_os_str().is_empty() || segments.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "canonical root did not have an absolute anchor and directory segment",
        ));
    }

    let mut directory = Dir::open_ambient_dir(anchor, ambient_authority())?;
    for segment in segments {
        directory = directory.open_dir_nofollow(segment)?;
    }
    Ok(directory)
}

fn is_within(root: &Path, candidate: &Path) -> bool {
    let root = normalized_components(root);
    let candidate = normalized_components(candidate);
    if !root
        .iter()
        .any(|component| matches!(component, Component::Normal(_)))
        || candidate.len() < root.len()
    {
        return false;
    }
    root.iter()
        .zip(candidate.iter())
        .all(|(&left, &right)| component_eq(left, right))
}

fn normalized_components(path: &Path) -> Vec<Component<'_>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if matches!(components.last(), Some(Component::Normal(_))) => {
                components.pop();
            }
            Component::ParentDir => {}
            component => components.push(component),
        }
    }
    components
}

fn component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    match (left, right) {
        (Component::Normal(left), Component::Normal(right)) => os_component_eq(left, right),
        (Component::Prefix(left), Component::Prefix(right)) => prefix_eq(left.kind(), right.kind()),
        (left, right) => left == right,
    }
}

fn os_component_eq(left: &OsStr, right: &OsStr) -> bool {
    if cfg!(windows) {
        windows_component_eq(left, right)
    } else {
        left == right
    }
}

/// Compare two Windows path components the way the filesystem does.
///
/// NTFS folds case over the whole Unicode range, not just ASCII, so an
/// ASCII-only comparison would read `C:\Ünister\Documents` and
/// `C:\ünister\Documents` as different folders. Every caller uses equality to
/// *refuse* — a protected location, the whole home directory, another user's
/// profile — so widening what counts as equal only ever fails closed. Lossy
/// conversion is safe for the same reason: it can merge two unpaired surrogates
/// into one replacement character, which again only refuses more.
fn windows_component_eq(left: &OsStr, right: &OsStr) -> bool {
    left == right || left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
}

fn prefix_eq(left: Prefix<'_>, right: Prefix<'_>) -> bool {
    match (left, right) {
        (Prefix::Disk(left), Prefix::Disk(right))
        | (Prefix::Disk(left), Prefix::VerbatimDisk(right))
        | (Prefix::VerbatimDisk(left), Prefix::Disk(right))
        | (Prefix::VerbatimDisk(left), Prefix::VerbatimDisk(right)) => {
            left.eq_ignore_ascii_case(&right)
        }
        (Prefix::UNC(left_server, left_share), Prefix::UNC(right_server, right_share))
        | (Prefix::UNC(left_server, left_share), Prefix::VerbatimUNC(right_server, right_share))
        | (Prefix::VerbatimUNC(left_server, left_share), Prefix::UNC(right_server, right_share))
        | (
            Prefix::VerbatimUNC(left_server, left_share),
            Prefix::VerbatimUNC(right_server, right_share),
        ) => os_component_eq(left_server, right_server) && os_component_eq(left_share, right_share),
        (Prefix::Verbatim(left), Prefix::Verbatim(right))
        | (Prefix::DeviceNS(left), Prefix::DeviceNS(right)) => os_component_eq(left, right),
        _ => false,
    }
}

fn is_filesystem_root(path: &Path) -> bool {
    let components = normalized_components(path);
    !components.is_empty()
        && !components
            .iter()
            .any(|component| matches!(component, Component::Normal(_)))
}

fn is_direct_child(parent: &Path, child: &Path) -> bool {
    let parent = normalized_components(parent);
    let child = normalized_components(child);
    child.len() == parent.len() + 1
        && matches!(child.last(), Some(Component::Normal(_)))
        && parent
            .iter()
            .zip(child.iter())
            .all(|(&left, &right)| component_eq(left, right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn policy() -> RootPolicy {
        RootPolicy::for_test(
            PathBuf::from("/users/alice"),
            vec![PathBuf::from("/system")],
            vec![PathBuf::from("/users")],
            vec![PathBuf::from("/volumes")],
        )
    }

    #[cfg(unix)]
    #[test]
    fn containment_is_segment_aligned_and_normalized() {
        assert!(is_within(
            Path::new("/users/alice/work"),
            Path::new("/users/alice/work/a/../report.md")
        ));
        assert!(!is_within(
            Path::new("/users/alice/work"),
            Path::new("/users/alice/work-old/report.md")
        ));
        assert!(!is_within(Path::new("/"), Path::new("/etc/passwd")));
    }

    #[cfg(windows)]
    #[test]
    fn canonical_windows_prefixes_match_lexical_policy_prefixes() {
        assert!(is_within(
            Path::new(r"C:\Windows"),
            Path::new(r"\\?\C:\Windows\System32")
        ));
        assert!(is_within(
            Path::new(r"\\server\share\projects"),
            Path::new(r"\\?\UNC\server\share\projects\app")
        ));
        // A whole network share carries no named folder, so it is a filesystem
        // root like `C:\` or `/` — too broad to register, and it contains
        // nothing for policy purposes. The share-relative root above is the
        // shallowest UNC path containment is defined for.
        assert!(is_filesystem_root(Path::new(r"\\server\share")));
        assert!(!is_within(
            Path::new(r"\\server\share"),
            Path::new(r"\\?\UNC\server\share\folder")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_device_and_general_verbatim_namespaces() {
        let policy = RootPolicy::for_test(
            PathBuf::from(r"C:\Users\alice"),
            vec![PathBuf::from(r"C:\Windows")],
            vec![PathBuf::from(r"C:\Users")],
            Vec::new(),
        );
        for path in [
            r"\\.\GLOBALROOT\Device\HarddiskVolume1\Windows",
            r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\Windows",
        ] {
            assert!(matches!(
                policy.validate_canonical(Path::new(path)),
                Err(RootPolicyError::UnsupportedNamespace)
            ));
        }
    }

    #[cfg(windows)]
    #[test]
    fn relocated_profile_container_rejects_other_whole_profiles() {
        let home = PathBuf::from(r"D:\Users\alice");
        let policy = RootPolicy::for_test(
            home.clone(),
            vec![PathBuf::from(r"D:\Windows")],
            vec![home.parent().unwrap().to_path_buf()],
            Vec::new(),
        );
        assert!(matches!(
            policy.validate_canonical(Path::new(r"D:\Users\bob")),
            Err(RootPolicyError::WholeUserProfile)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_known_folders_fail_closed_without_required_host_data() {
        let missing = windows_known_folder_from(&["ProgramData"], |_| None);
        assert!(matches!(
            missing,
            Err(RootPolicyError::MissingKnownFolder("ProgramData"))
        ));
        let missing_x86_programs = windows_known_folder_from(&["ProgramFiles(x86)"], |_| None);
        assert!(matches!(
            missing_x86_programs,
            Err(RootPolicyError::MissingKnownFolder("ProgramFiles(x86)"))
        ));

        let relocated = windows_known_folder_from(&["SystemRoot", "windir"], |name| {
            (name == "SystemRoot").then(|| OsString::from(r"D:\Windows"))
        })
        .unwrap();
        assert_eq!(relocated, PathBuf::from(r"D:\Windows"));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_standard_and_project_folders_inside_the_current_profile() {
        for path in [
            "/users/alice/Documents",
            "/users/alice/Downloads",
            "/users/alice/work/project",
        ] {
            assert!(
                policy().validate_canonical(Path::new(path)).is_ok(),
                "{path}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn application_private_directories_cannot_be_connected_or_contained() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("app-data");
        std::fs::create_dir(&private).unwrap();
        let policy = policy().with_private_directory(&private).unwrap();
        let private = private.canonicalize().unwrap();
        let parent = temp.path().canonicalize().unwrap();
        assert!(matches!(
            policy.validate_canonical(&private),
            Err(RootPolicyError::SensitiveLocation)
        ));
        assert!(matches!(
            policy.validate_canonical(&parent),
            Err(RootPolicyError::SensitiveLocation)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_home_profiles_broad_roots_and_sensitive_overlap() {
        assert!(matches!(
            policy().validate_canonical(Path::new("/users/alice")),
            Err(RootPolicyError::TooBroad)
        ));
        assert!(matches!(
            policy().validate_canonical(Path::new("/users/bob")),
            Err(RootPolicyError::WholeUserProfile)
        ));
        assert!(matches!(
            policy().validate_canonical(Path::new("/volumes")),
            Err(RootPolicyError::TooBroad)
        ));
        assert!(matches!(
            policy().validate_canonical(Path::new("/system/secrets")),
            Err(RootPolicyError::SensitiveLocation)
        ));
        assert!(matches!(
            policy().validate_canonical(Path::new("/")),
            Err(RootPolicyError::TooBroad)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn opening_a_root_resolves_symlinks_before_policy_and_pins_a_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let allowed = home.join("Documents");
        let sensitive = temp.path().join("sensitive");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&sensitive).unwrap();
        let policy = RootPolicy::for_test(
            home.canonicalize().unwrap(),
            vec![sensitive.canonicalize().unwrap()],
            Vec::new(),
            Vec::new(),
        );

        let opened = policy.open_root(&allowed).unwrap();
        assert!(opened.directory().dir_metadata().unwrap().is_dir());
        assert_eq!(opened.canonical_path(), allowed.canonicalize().unwrap());

        let disguised = temp.path().join("disguised");
        symlink(&sensitive, &disguised).unwrap();
        assert!(matches!(
            policy.open_root(&disguised),
            Err(RootPolicyError::SensitiveLocation)
        ));
    }

    #[test]
    fn opening_a_root_never_resolves_a_relative_control_path() {
        let policy = RootPolicy::for_test(
            PathBuf::from("/users/alice"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(matches!(
            policy.open_root(Path::new("Documents/project")),
            Err(RootPolicyError::NotAbsolute)
        ));
    }

    /// The component grammar is a plain string rule, so it is checked on every
    /// host even though it only governs Windows roots. Windows path *parsing*
    /// is not available off Windows, which is why the whole-path cases below
    /// are separate.
    #[test]
    fn windows_root_components_that_can_name_something_else_are_refused() {
        for component in [
            // Alternate data stream and drive-relative forms.
            "Documents:evil",
            "C:",
            // Reserved device names, with and without an extension.
            "NUL",
            "con",
            "COM1",
            "lpt0.log",
            "CONIN$",
            // Windows strips these, so the stored name is not what was checked.
            "Reports.",
            "Reports ",
            "",
        ] {
            assert!(
                !portable_windows_component(component),
                "accepted {component:?}"
            );
        }
        for component in ["Documents", "my.project", "COM", "COM10", "résumé", "$"] {
            assert!(
                portable_windows_component(component),
                "refused {component:?}"
            );
        }
    }

    #[test]
    fn unc_hidden_shares_and_loopback_servers_are_recognized() {
        for share in ["C$", "ADMIN$", "IPC$"] {
            assert!(is_hidden_share(share), "{share}");
        }
        assert!(!is_hidden_share("projects"));
        for server in ["localhost", "LOCALHOST", "127.0.0.1", "[::1]", "::1", "."] {
            assert!(is_loopback_server(server), "{server}");
        }
        for server in ["fileserver", "127", "files.example.com"] {
            assert!(!is_loopback_server(server), "{server}");
        }
    }

    #[test]
    fn windows_component_comparison_folds_case_beyond_ascii() {
        assert!(windows_component_eq(
            OsStr::new("Ünister"),
            OsStr::new("ÜNISTER")
        ));
        assert!(!windows_component_eq(
            OsStr::new("Ünister"),
            OsStr::new("Unister")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn rejects_unc_roots_that_alias_a_local_volume() {
        let policy = RootPolicy::for_test(
            PathBuf::from(r"C:\Users\alice"),
            vec![PathBuf::from(r"C:\Windows")],
            vec![PathBuf::from(r"C:\Users")],
            Vec::new(),
        );
        for path in [
            r"\\localhost\C$\Windows\System32",
            r"\\WORKSTATION\ADMIN$\System32",
            r"\\?\UNC\127.0.0.1\projects\app",
        ] {
            assert!(
                matches!(
                    policy.validate_canonical(Path::new(path)),
                    Err(RootPolicyError::AdministrativeShare)
                ),
                "{path}"
            );
        }
        assert!(policy
            .validate_canonical(Path::new(r"\\fileserver\projects\app"))
            .is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn rejects_stream_and_device_components_in_a_canonical_root() {
        let policy = RootPolicy::for_test(
            PathBuf::from(r"C:\Users\alice"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        for path in [r"C:\Users\alice\Documents:evil", r"C:\Users\alice\NUL"] {
            assert!(
                matches!(
                    policy.validate_canonical(Path::new(path)),
                    Err(RootPolicyError::NonPortableComponent)
                ),
                "{path}"
            );
        }
    }

    /// The long-name confirmation must not refuse ordinary folders — if it
    /// did, no Windows user could connect one.
    #[cfg(windows)]
    #[test]
    fn long_name_confirmation_accepts_an_ordinary_canonical_folder() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Project Reports");
        std::fs::create_dir(&folder).unwrap();
        require_long_name_form(&folder.canonicalize().unwrap()).unwrap();
    }

    /// The escape a Windows host adds over Unix: a junction is a reparse point
    /// rather than a symlink, and nothing in the policy layer would notice one
    /// appearing inside an already-registered root. Containment for it comes
    /// from resolving every component without following links.
    #[cfg(windows)]
    #[test]
    fn a_junction_inside_a_registered_root_cannot_reach_its_target() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let root = home.join("Documents");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();

        let junction = root.join("escape");
        let created = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .status()
            .unwrap();
        assert!(created.success(), "could not create a directory junction");

        let policy = RootPolicy::for_test(
            home.canonicalize().unwrap(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let opened = policy.open_root(&root).unwrap();
        let directory = opened.directory();
        assert!(directory.open_dir_nofollow("escape").is_err());
        assert!(directory.read_dir("escape").is_err());
        assert!(directory.open("escape/secret.txt").is_err());
    }
}
