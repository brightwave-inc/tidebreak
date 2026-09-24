//! Which profile a command works on.
//!
//! A profile is one data directory (chats, settings, logs, `listen.json`) plus
//! the credential item stored for it. Every command picks it the same way:
//!
//! - When `TIDEBREAK_DATA_DIR` names a directory, the profile is that one.
//! - Otherwise it is the Tidebreak app's own profile, in the data directory the
//!   desktop build of this channel uses. A client command connects to the app
//!   there (see [`crate::connect`]). `serve`, `folder`, `rehome-secrets`, and
//!   `--embed` open it in this process.
//!
//! Nothing defaults to the current directory. A command run from a project
//! folder used to start a new, empty profile in `./.tidebreak`, and because
//! the credential item did not depend on the directory, a key set there
//! rewrote the app's.
//!
//! The directory also decides the credential item. The app's own profile keeps
//! the keychain service the app uses, so an existing install reads its current
//! item and nothing migrates. Any other profile keeps its credentials under a
//! service derived from its directory, so a key set or removed there never
//! reaches the app's.
//!
//! A profile other than the app's used to share the app's item, so after an
//! upgrade its own item starts empty. `tidebreak rehome-secrets` copies the
//! shared item into it once ([`adopt_previous_bundle`]), and leaves the shared
//! one as it is. Nothing copies it without being asked: that would hand every
//! new profile the app's credentials, and on macOS reading an item another
//! build created can raise an access prompt a headless run cannot answer.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use tidebreak_core::{AgentError, Config, Profile, Result, SecretProvider, BUNDLE_KEY};

/// The service `tidebreak-core` stores credentials under when a config names
/// none. A profile of this channel that is not the app's derives its own from
/// it.
const DEFAULT_KEYCHAIN_SERVICE: &str = "tidebreak";

/// The folder the desktop app puts code worktrees in, under the home
/// directory. Every channel shares it.
const WORKTREE_ROOT_FOLDER: &str = "Tidebreak";

/// A packaged desktop channel.
///
/// The values mirror `tidebreak-desktop`'s `channel` module, which the CLI
/// cannot depend on. `the_channels_match_the_desktop` pins them to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    Production,
    Dev,
    Staging,
}

impl Channel {
    /// The channel this binary was built as, chosen the way the desktop
    /// chooses its own. A debug build is the dev channel. A release build is
    /// staging only when `TIDEBREAK_CHANNEL=staging` was set at compile time,
    /// which the staging workflow does for the app and its bundled CLI alike.
    pub(crate) fn current() -> Self {
        if cfg!(debug_assertions) {
            return Self::Dev;
        }
        match option_env!("TIDEBREAK_CHANNEL") {
            Some("staging") => Self::Staging,
            _ => Self::Production,
        }
    }

    /// The app's bundle identifier, which names its data directory and its
    /// managed-preferences domain.
    fn identifier(self) -> &'static str {
        match self {
            Self::Production => "io.brightwave.tidebreak",
            Self::Dev => "io.brightwave.tidebreak.dev",
            Self::Staging => "io.brightwave.tidebreak.staging",
        }
    }

    /// The keychain service the app's profile uses. Production names none,
    /// which is the server's default.
    fn keychain_service(self) -> Option<&'static str> {
        match self {
            Self::Production => None,
            Self::Dev => Some("tidebreak.dev"),
            Self::Staging => Some("tidebreak.staging"),
        }
    }
}

/// The configuration of the profile a command works on.
pub(crate) fn config() -> Result<Config> {
    let channel = Channel::current();
    let app_dir = app_data_dir_for(channel);
    let mut config = Config::from_env_with_default_data_dir(|profile| match profile {
        Profile::Desktop => app_dir.clone(),
        // A self-host server has no app to default to, so it needs the
        // variable. The image sets it.
        _ => None,
    })?;
    if config.profile == Profile::Desktop {
        let home = home_dir().map(|home| home.canonicalize().unwrap_or(home));
        let identity = identity(
            channel,
            &config.data_dir,
            app_dir.as_deref(),
            home.as_deref(),
        );
        config.keychain_service = identity.keychain_service;
        config.bundle_id = identity.bundle_id;
        config.code_worktree_root_default = identity.worktree_root_default;
    }
    if let Some(service) = keychain_service_override() {
        config.keychain_service = Some(service);
    }
    Ok(config)
}

/// The keychain service a debug build was pointed at with
/// `TIDEBREAK_KEYCHAIN_SERVICE`, which wins over the profile's own.
///
/// A headless rig can point a debug build at a scratch service of its own
/// choosing. A freshly re-linked binary reading items another build created
/// trips the macOS access prompt, which blocks a session with no window
/// forever; a scratch service starts empty. A release build ignores it.
fn keychain_service_override() -> Option<String> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var("TIDEBREAK_KEYCHAIN_SERVICE")
        .ok()
        .filter(|service| !service.is_empty())
}

/// Where a profile other than the app's kept its credentials before it had a
/// keychain service of its own. `None` for the app's own profile, a self-host
/// profile, and a debug build pointed at `TIDEBREAK_KEYCHAIN_SERVICE`.
#[cfg_attr(not(feature = "keychain"), allow(dead_code))]
pub(crate) fn previous_keychain_service(config: &Config) -> Option<&'static str> {
    if config.profile != Profile::Desktop || keychain_service_override().is_some() {
        return None;
    }
    let channel = Channel::current();
    previous_service_for(
        channel,
        &config.data_dir,
        app_data_dir_for(channel).as_deref(),
    )
}

/// Until each profile had its own, the CLI kept every desktop profile under
/// one service: `tidebreak.dev` in a debug build, and the default `tidebreak`
/// in every release build, staging included.
#[cfg_attr(not(feature = "keychain"), allow(dead_code))]
fn previous_service_for(
    channel: Channel,
    data_dir: &Path,
    app_dir: Option<&Path>,
) -> Option<&'static str> {
    if app_dir.is_some_and(|app_dir| resolved(app_dir) == resolved(data_dir)) {
        return None;
    }
    Some(match channel {
        Channel::Dev => "tidebreak.dev",
        Channel::Production | Channel::Staging => DEFAULT_KEYCHAIN_SERVICE,
    })
}

/// What [`adopt_previous_bundle`] did.
#[cfg_attr(not(feature = "keychain"), allow(dead_code))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Adoption {
    /// The profile already has its own item, so nothing was read or written.
    AlreadyOwn,
    /// The shared item holds nothing.
    NothingToCopy,
    /// The shared item was copied into the profile's own and left as it was.
    Copied,
}

/// Copy the credential bundle `previous` holds into `own`, but only while
/// `own` holds none, so running it again never overwrites a key set since.
/// `previous` is read and never changed: the app still uses it.
#[cfg_attr(not(feature = "keychain"), allow(dead_code))]
pub(crate) async fn adopt_previous_bundle(
    previous: &dyn SecretProvider,
    own: &dyn SecretProvider,
) -> Result<Adoption> {
    if own.get_secret(BUNDLE_KEY).await?.is_some() {
        return Ok(Adoption::AlreadyOwn);
    }
    let Some(bundle) = previous.get_secret(BUNDLE_KEY).await? else {
        return Ok(Adoption::NothingToCopy);
    };
    own.set_secret(BUNDLE_KEY, &bundle).await?;
    match own.get_secret(BUNDLE_KEY).await? {
        Some(stored) if stored == bundle => Ok(Adoption::Copied),
        _ => Err(AgentError::Secret(
            "the copied credentials did not read back unchanged; the previous entry is \
             untouched, so run rehome-secrets again"
                .to_owned(),
        )),
    }
}

/// Whether `TIDEBREAK_DATA_DIR` names a directory. An empty value names none,
/// the same rule the configuration applies.
pub(crate) fn data_dir_is_named() -> bool {
    std::env::var_os("TIDEBREAK_DATA_DIR").is_some_and(|dir| !dir.is_empty())
}

/// The Tidebreak app's data directory for this build's channel, or `None`
/// when this computer has no per-user data directory to find it in.
pub(crate) fn app_data_dir() -> Option<PathBuf> {
    app_data_dir_for(Channel::current())
}

/// Where the app of `channel` keeps its data: the platform's per-user data
/// directory joined with the app's identifier, the way the app itself
/// resolves it.
fn app_data_dir_for(channel: Channel) -> Option<PathBuf> {
    platform_data_dir().map(|dir| dir.join(channel.identifier()))
}

/// The platform's per-user application data directory: `~/Library/Application
/// Support` on macOS, `%APPDATA%` on Windows, and `$XDG_DATA_HOME` or
/// `~/.local/share` elsewhere.
fn platform_data_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        home_dir().map(|home| home.join("Library").join("Application Support"))
    } else if cfg!(windows) {
        absolute_env_path("APPDATA")
    } else {
        absolute_env_path("XDG_DATA_HOME")
            .or_else(|| home_dir().map(|home| home.join(".local").join("share")))
    }
}

fn absolute_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| home.is_absolute())
}

/// What a desktop-profile configuration takes from the profile its data
/// directory holds.
#[derive(Debug, PartialEq, Eq)]
struct Identity {
    keychain_service: Option<String>,
    bundle_id: Option<String>,
    worktree_root_default: Option<PathBuf>,
}

fn identity(
    channel: Channel,
    data_dir: &Path,
    app_dir: Option<&Path>,
    home: Option<&Path>,
) -> Identity {
    let data_dir = resolved(data_dir);
    if app_dir.is_some_and(|app_dir| resolved(app_dir) == data_dir) {
        // The app's own profile. It keeps the credential item, the
        // managed-preferences domain, and the worktree root the app uses, so
        // opening it here reads the same credentials and obeys the same
        // policy as the app does.
        return Identity {
            keychain_service: channel.keychain_service().map(str::to_owned),
            bundle_id: Some(channel.identifier().to_owned()),
            worktree_root_default: home
                .map(|home| home.join(WORKTREE_ROOT_FOLDER).join("workspaces")),
        };
    }
    let base = channel
        .keychain_service()
        .unwrap_or(DEFAULT_KEYCHAIN_SERVICE);
    Identity {
        keychain_service: Some(format!("{base}.profile.{}", profile_id(&data_dir))),
        bundle_id: None,
        worktree_root_default: None,
    }
}

/// `path` made absolute, with symlinks and `..` resolved, so two spellings of
/// one directory name one profile. A directory that does not exist yet
/// resolves the same way before and after it is created, because the part of
/// it that exists does the resolving.
fn resolved(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut lexical = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Resolve what exists first, so `..` leaves a symlink's
                // target rather than the folder the link sits in.
                if let Ok(canonical) = lexical.canonicalize() {
                    lexical = canonical;
                }
                lexical.pop();
            }
            other => lexical.push(other),
        }
    }
    let mut existing = lexical.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(canonical, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return lexical,
        }
    }
}

/// The first 16 hex digits of the SHA-256 of a resolved directory: the same
/// for one directory every time, and different for two.
fn profile_id(resolved: &Path) -> String {
    Sha256::digest(resolved.as_os_str().as_encoded_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's own profile keeps the item the app reads today. This is what
    /// lets an existing install keep its credentials with no migration.
    #[test]
    fn the_app_profile_keeps_the_app_keychain_item() {
        let home = tempfile::tempdir().unwrap();
        let app = home.path().join("app-data");
        for (channel, service) in [
            (Channel::Production, None),
            (Channel::Dev, Some("tidebreak.dev")),
            (Channel::Staging, Some("tidebreak.staging")),
        ] {
            assert_eq!(
                identity(channel, &app, Some(&app), Some(home.path())),
                Identity {
                    keychain_service: service.map(str::to_owned),
                    bundle_id: Some(channel.identifier().to_owned()),
                    worktree_root_default: Some(home.path().join("Tidebreak").join("workspaces")),
                },
                "{channel:?}"
            );
        }
    }

    /// Any other profile gets a service of its own, so its credentials never
    /// reach the app's item and two profiles never share one.
    #[test]
    fn another_profile_gets_its_own_keychain_service() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("app-data");
        let first = root.path().join("first");
        let second = root.path().join("second");

        let service = |channel, dir: &Path| {
            identity(channel, dir, Some(&app), None)
                .keychain_service
                .expect("a profile other than the app's names its service")
        };
        let first_service = service(Channel::Production, &first);
        assert!(
            first_service.starts_with("tidebreak.profile."),
            "{first_service}"
        );
        assert_eq!(first_service.len(), "tidebreak.profile.".len() + 16);
        assert_ne!(first_service, service(Channel::Production, &second));
        assert!(service(Channel::Dev, &first).starts_with("tidebreak.dev.profile."));
        assert!(service(Channel::Staging, &first).starts_with("tidebreak.staging.profile."));

        let other = identity(Channel::Production, &first, Some(&app), None);
        assert_eq!(other.bundle_id, None);
        assert_eq!(other.worktree_root_default, None);
        // With no app directory to compare against, nothing is the app's.
        let unknown_app = identity(Channel::Production, &app, None, None)
            .keychain_service
            .expect("a profile with no app to match names its own service");
        assert!(
            unknown_app.starts_with("tidebreak.profile."),
            "{unknown_app}"
        );
    }

    /// One directory is one profile however it is spelled, and whether or not
    /// it exists yet.
    #[test]
    fn two_spellings_of_one_directory_are_one_profile() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("profile");
        let dotted = root.path().join("elsewhere").join("..").join("profile");
        let before = resolved(&dir);
        assert_eq!(resolved(&dotted), before);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(resolved(&dir), before, "creating it changes nothing");

        #[cfg(unix)]
        {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&dir, &link).unwrap();
            assert_eq!(resolved(&link), resolved(&dir));
            let app = root.path().join("app-data");
            assert_eq!(
                identity(Channel::Dev, &link, Some(&app), None),
                identity(Channel::Dev, &dir, Some(&app), None)
            );
            // A link to the app's directory is the app's profile.
            let app_link = root.path().join("app-link");
            std::fs::create_dir_all(&app).unwrap();
            std::os::unix::fs::symlink(&app, &app_link).unwrap();
            assert_eq!(
                identity(Channel::Dev, &app_link, Some(&app), None).keychain_service,
                Some("tidebreak.dev".to_owned())
            );
        }
    }

    /// The app's data directory is the platform's per-user data directory
    /// joined with the channel's identifier.
    #[test]
    fn the_app_data_dir_ends_in_the_channel_identifier() {
        if let Some(dir) = app_data_dir_for(Channel::Production) {
            assert!(dir.is_absolute(), "{}", dir.display());
            assert!(
                dir.ends_with("io.brightwave.tidebreak"),
                "{}",
                dir.display()
            );
        }
        if let Some(dir) = app_data_dir_for(Channel::Staging) {
            assert!(dir.ends_with("io.brightwave.tidebreak.staging"));
        }
    }

    /// The CLI cannot depend on the desktop crate, so the values it copies are
    /// checked against the desktop's source.
    #[test]
    fn the_channels_match_the_desktop() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../tidebreak-desktop/src/channel.rs");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        for (constant, value) in [
            ("PRODUCTION_IDENTIFIER", Channel::Production.identifier()),
            ("DEV_IDENTIFIER", Channel::Dev.identifier()),
            ("STAGING_IDENTIFIER", Channel::Staging.identifier()),
            (
                "DEV_KEYCHAIN_SERVICE",
                Channel::Dev.keychain_service().unwrap(),
            ),
            (
                "STAGING_KEYCHAIN_SERVICE",
                Channel::Staging.keychain_service().unwrap(),
            ),
            ("PRODUCTION_PRODUCT_NAME", WORKTREE_ROOT_FOLDER),
        ] {
            let line = format!("pub const {constant}: &str = \"{value}\";");
            assert!(source.contains(&line), "{} lacks `{line}`", path.display());
        }
        assert!(
            source.contains("Self::Production => None,"),
            "production must keep the default keychain service"
        );
    }

    /// One keychain service in memory: item name to value.
    #[derive(Default)]
    struct Items(std::sync::Mutex<std::collections::HashMap<String, String>>);

    #[async_trait::async_trait]
    impl SecretProvider for Items {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }

        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
            Ok(())
        }

        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    /// A made-up credential bundle. Built from pieces so nothing here reads as
    /// a key.
    fn bundle(value: &str) -> String {
        let credential = ["fixture", value].join("-");
        serde_json::json!({ "provider.openai.credential": credential }).to_string()
    }

    /// The keys a profile stored in the shared item come back into its own,
    /// once, and the shared item the app still reads is left as it was.
    #[tokio::test]
    async fn rehoming_copies_the_shared_item_once_and_leaves_it() {
        let shared = Items::default();
        let own = Items::default();
        shared
            .set_secret(BUNDLE_KEY, &bundle("before"))
            .await
            .unwrap();

        assert_eq!(
            adopt_previous_bundle(&shared, &own).await.unwrap(),
            Adoption::Copied
        );
        assert_eq!(
            own.get_secret(BUNDLE_KEY).await.unwrap(),
            Some(bundle("before"))
        );
        assert_eq!(
            shared.get_secret(BUNDLE_KEY).await.unwrap(),
            Some(bundle("before")),
            "the app's item is never changed"
        );

        // A key set in the profile since is never overwritten by a second run.
        own.set_secret(BUNDLE_KEY, &bundle("since")).await.unwrap();
        assert_eq!(
            adopt_previous_bundle(&shared, &own).await.unwrap(),
            Adoption::AlreadyOwn
        );
        assert_eq!(
            own.get_secret(BUNDLE_KEY).await.unwrap(),
            Some(bundle("since"))
        );

        let empty = Items::default();
        let fresh = Items::default();
        assert_eq!(
            adopt_previous_bundle(&empty, &fresh).await.unwrap(),
            Adoption::NothingToCopy
        );
        assert_eq!(fresh.get_secret(BUNDLE_KEY).await.unwrap(), None);
    }

    /// Only a profile that is not the app's had its credentials somewhere
    /// else before; the app's own profile never moved.
    #[test]
    fn only_a_profile_other_than_the_apps_has_a_previous_entry() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join("app-data");
        let named = root.path().join("named");
        assert_eq!(
            previous_service_for(Channel::Dev, &named, Some(&app)),
            Some("tidebreak.dev")
        );
        assert_eq!(
            previous_service_for(Channel::Production, &named, Some(&app)),
            Some("tidebreak")
        );
        // A staging release build used production's service.
        assert_eq!(
            previous_service_for(Channel::Staging, &named, Some(&app)),
            Some("tidebreak")
        );
        assert_eq!(previous_service_for(Channel::Dev, &app, Some(&app)), None);
    }
}
