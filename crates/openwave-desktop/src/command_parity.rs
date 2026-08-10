//! Parity gate for the desktop's Tauri IPC surface.
//!
//! Decision record 5 makes the server HTTP/WS API the canonical product
//! surface and closes the set of features that may legitimately be native-only.
//! Nothing enforces that in review reliably, so this scans the crate's own
//! sources for `#[tauri::command]` and holds the result against
//! `native-only-commands.txt`. A new desktop-only command fails here until it
//! is either given server routes or consciously added to the allowlist.

/// Where the allowlist lives, relative to the crate root — named in the failure
/// messages so the fix path needs no searching.
const ALLOWLIST_FILE: &str = "crates/openwave-desktop/native-only-commands.txt";
const RECORD: &str = "docs/decisions/0005-cli-headless-feature-parity.md";

const ALLOWLIST: &str = include_str!("../native-only-commands.txt");

/// Command names listed in the allowlist, in file order.
fn allowlisted() -> Vec<String> {
    ALLOWLIST
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _justification)| name.trim().to_owned())
        .collect()
}

/// Every `#[tauri::command]` function in the crate's sources, as
/// `(command name, file it was declared in)`.
///
/// Deliberately textual: the alternative is a registry macro, which is more
/// machinery than a list of names is worth. The scan tolerates the shapes the
/// crate actually uses — the attribute on its own line, `fn`/`pub fn`/
/// `pub(crate) fn`, sync or `async`, and doc comments or further attributes
/// between the two.
fn declared_commands() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for file in rust_sources(&root) {
        let relative = file
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&file)
            .display()
            .to_string();
        let source = std::fs::read_to_string(&file).expect("desktop source is readable");
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.trim_start().starts_with("#[tauri::command") {
                continue;
            }
            let name = lines[index + 1..]
                .iter()
                .find_map(|line| function_name(line))
                .unwrap_or_else(|| {
                    panic!(
                        "{relative}:{} has a #[tauri::command] with no function after it",
                        index + 1
                    )
                });
            found.push((name, relative.clone()));
        }
    }
    found
}

/// The name in a function signature line, if the line declares one.
fn function_name(line: &str) -> Option<String> {
    let line = line.trim_start();
    let rest = ["pub(crate) ", "pub(super) ", "pub "]
        .iter()
        .find_map(|visibility| line.strip_prefix(visibility))
        .unwrap_or(line);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn rust_sources(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("desktop source tree is readable") {
            let path = entry.expect("directory entry is readable").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn allowlist_covers_every_tauri_command() {
    let allowlisted = allowlisted();
    let declared = declared_commands();

    let unlisted: Vec<String> = declared
        .iter()
        .filter(|(name, _)| !allowlisted.contains(name))
        .map(|(name, file)| format!("  {name}  ({file})"))
        .collect();
    assert!(
        unlisted.is_empty(),
        "these #[tauri::command] handlers are not in {ALLOWLIST_FILE}:\n{}\n\n\
         Decision record 5 ({RECORD}) makes the server HTTP/WS API the product \
         surface and closes the set of native-only features. Give the feature \
         `openwave-server` routes and call them from the UI, or — if it \
         genuinely needs the native shell — add it to {ALLOWLIST_FILE} with a \
         justification saying which native capability it needs.",
        unlisted.join("\n"),
    );

    let stale: Vec<&String> = allowlisted
        .iter()
        .filter(|name| !declared.iter().any(|(declared, _)| declared == *name))
        .collect();
    assert!(
        stale.is_empty(),
        "{ALLOWLIST_FILE} lists commands that no longer exist: {stale:?}\n\n\
         A stale entry silently pre-approves a name a later change could reuse. \
         Remove these lines — see decision record 5 ({RECORD})."
    );

    let mut seen = allowlisted.clone();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        allowlisted.len(),
        "{ALLOWLIST_FILE} lists a command twice"
    );
}

#[test]
fn function_name_reads_the_shapes_the_crate_uses() {
    for line in [
        "fn plain(app: AppHandle) {",
        "pub fn exported() {",
        "    pub(crate) async fn nested(",
        "async fn bare() {",
    ] {
        assert!(
            function_name(line).is_some(),
            "signature not recognized: {line}"
        );
    }
    assert_eq!(
        function_name("    pub(crate) async fn nested(").as_deref(),
        Some("nested")
    );
    assert_eq!(function_name("#[derive(Debug)]"), None);
    assert_eq!(function_name("/// fn in a doc comment"), None);
}
