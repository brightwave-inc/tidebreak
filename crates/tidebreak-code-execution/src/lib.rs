//! Provider-neutral, bounded command execution for Tidebreak.
//!
//! The model-facing [`ExecTool`] sends a normalized [`ExecRequest`] to
//! a host-selected [`ExecProvider`]. Requests carry a stable execution
//! identity and an opaque workspace identity, so a future managed adapter can
//! reconcile retries and map a chat to its own remote session without exposing
//! provider credentials or accepting model-authored host paths.
//!
//! [`LocalExecutionProvider`] runs directly on macOS under the native Seatbelt
//! sandbox. Managed providers run the same direct command contract in a remote
//! sandbox and share session, idempotency, and bounded-output primitives.

pub mod agent_plugins;
mod credential;
mod daytona;
mod docker;
mod e2b;
pub mod host_paths;
pub mod host_tools;
mod http;
mod local;
pub mod managed_node;
#[cfg(target_os = "macos")]
mod network;
pub mod office_render;
mod output;
pub mod overlay;
pub mod package_cache;
pub mod plugins;
mod preview;
pub mod prompts;
mod receipt;
mod remote;
mod sandbox_image;
pub mod sbpl;
pub mod skills;
pub mod sync;
mod tool;
mod types;

pub use agent_plugins::{
    canonical_mcp_config, expand_plugin_placeholders, is_valid_agent_plugin_name,
    load_plugin_mcp_config, parse_agent_plugin_manifest, parse_plugin_mcp_config,
    AgentPluginManifest, AgentPluginParseError, IgnoredManifestField, McpConfigError,
    McpHttpServer, McpServer, McpStdioServer, McpTransport, ParsedAgentPluginManifest,
    ParsedPluginMcpConfig, PluginMcpConfig, SkippedMcpServer, AGENT_PLUGIN_MANIFEST_FILE,
    AGENT_PLUGIN_MCP_FILE, AGENT_PLUGIN_MCP_SCHEMA_ID, AGENT_PLUGIN_SCHEMA_ID,
    AGENT_PLUGIN_SKILLS_DIR, AGENT_PLUGIN_SPEC_VERSION, PLUGIN_DATA_VARIABLE, PLUGIN_ROOT_VARIABLE,
    TIDEBREAK_EXTENSION_NAMESPACE,
};
pub use daytona::{DaytonaCredential, DaytonaExecutionProvider, DAYTONA_CREDENTIAL_KEY};
pub use docker::{resolve_container_runtime_binary, DockerExecutionProvider};
pub use e2b::{E2BCredential, E2BExecutionProvider, E2B_CREDENTIAL_KEY};
pub use host_paths::{
    resolve_scratch_directory, try_resolve_scratch_directory, FilePrecondition, FileStamp,
    ScratchDir, ScratchEntry, ScratchEntryKind, ScratchRefusal,
};
pub use host_tools::{HostToolBroker, HostToolStatus};
pub use local::LocalExecutionProvider;
pub use office_render::{
    render_office_outputs, HostOfficeConverter, OfficeConvertError, OFFICE_RENDER_DIR,
};
pub use overlay::{
    materialize_file, materialized_file_matches, sweep_abandoned_overlays,
    MaterializationPrecondition, MaterializedChange, MaterializedChangeKind, NativeTrash,
    OverlayInspector, OverlayOutcome, OverlaySlot, PreparedWriteSnapshot, PriorContents,
    RejectedChange, RejectedChangeReason, StagedChange, TrashSink, WriteOverlay, WriteSnapshotSink,
    OVERLAY_DIR,
};
pub use package_cache::{
    PromotionReport, SharedPackageCache, PACKAGE_CACHE_DIR, PACKAGE_CACHE_ENV,
};
pub use plugins::{
    assess_plugin_compatibility, derived_capabilities, is_valid_plugin_name,
    is_valid_plugin_router_preamble, load_plugins, merged_plugins, parse_plugin_manifest,
    LoadedPlugin, PluginCapability, PluginCategory, PluginCompatibility, PluginCompatibilityIssue,
    PluginCompatibilityStatus, PluginInstallStamp, PluginOrigin, PluginPackage, PluginParseError,
    PluginSourceFormat, PLUGIN_INSTALL_STAMP_FILE, PLUGIN_INSTALL_STAMP_SCHEMA,
    PLUGIN_MANIFEST_FILE,
};
pub use preview::{scan_preview_directory, PreviewScan};
pub use prompts::{
    is_valid_prompt_name, load_prompts, merged_prompts, parse_prompt_manifest, LoadedPrompt,
    PromptOrigin, PromptPackage, PromptParseError, MAX_PROMPT_BODY_BYTES, PROMPT_MANIFEST_FILE,
};
pub use remote::RemoteSessionPool;
pub use skills::{
    is_valid_skill_description, is_valid_skill_name, load_skills, merged_skills,
    parse_skill_manifest, HostDep, LoadedSkill, SkillOrigin, SkillPackage, SkillParseError,
    SkillScript, SKILLS_DIR, SKILL_MANIFEST_FILE, SKILL_SCRIPTS_DIR,
};
pub use tool::{ExecTool, EXEC_TOOL_NAME};
pub use types::{
    ExecError, ExecFolderAccess, ExecFolderGrant, ExecProvider, ExecProviderKind, ExecRequest,
    ExecResponse, ExecUnavailableReason, ExecutionId, ExecutionWorkspaceId, OutputArtifactEntry,
    OutputArtifactScan, OutputArtifactStatus, SandboxPreparation, SandboxPreparationSink,
    StagedUpload, WorkspaceFileEntry, WorkspaceFilePath, WorkspaceLifecycle, WorkspaceListing,
    MAX_ARGUMENTS, MAX_ARGUMENT_BYTES, MAX_CAPTURE_BYTES, MAX_COMMAND_BYTES, MAX_CWD_BYTES,
    MAX_EXEC_FOLDER_GRANTS, MAX_STAGED_PATHS, MAX_WORKSPACE_FILE_BYTES, MAX_WORKSPACE_LIST_ENTRIES,
    MAX_WORKSPACE_PATH_BYTES,
};

/// Stable workspace-relative location populated by hosts that ship the
/// document helper library.
pub const DOCUMENT_SCRIPTS_DIR: &str = ".tidebreak/exec-scripts";

/// Exact registry endpoints admitted by the provider-neutral package-manager
/// policy class.
pub const PACKAGE_MANAGER_DOMAINS: &[&str] = &[
    "api.nuget.org",
    "crates.io",
    "files.pythonhosted.org",
    "globalcdn.nuget.org",
    "index.crates.io",
    "plugins.gradle.org",
    "proxy.golang.org",
    "pypi.org",
    "registry.npmjs.org",
    "repo.maven.apache.org",
    "repo1.maven.org",
    "repo.packagist.org",
    "rubygems.org",
    "static.crates.io",
    "sum.golang.org",
];

/// Files copied as one indivisible helper library into an exec workspace.
pub const DOCUMENT_SCRIPT_FILES: [&str; 13] = [
    "_tidebreak_preview.py",
    "_tidebreak_calc.py",
    "_tidebreak_ooxml.py",
    "render_pdf.py",
    "extract_pdf_figures.py",
    "render_office.py",
    "analyze_xlsx.py",
    "office_unpack.py",
    "office_pack.py",
    "pptx_clean.py",
    "calc_uno.py",
    "xlsx_recalc.py",
    "docx_clean.py",
];

/// The baseline Python packages guaranteed on every execution backend, as
/// exact `package==version` pins.
///
/// Skill pins cover what one document skill needs; these cover what an ad-hoc
/// script may import with no skill in play. The declaration lives in
/// `baseline_python_deps.txt` beside this crate because three consumers in two
/// languages read it: the documents image closure generator, the local
/// backend's offline package-cache population, and the operating prompt that
/// tells the model which libraries it can count on.
///
/// Malformed lines are skipped rather than panicking a running host; the
/// crate's own test and `scripts/sandbox-image-pins.test.mjs` are what make a
/// bad edit loud.
#[must_use]
pub fn baseline_python_deps() -> &'static [&'static str] {
    static PINS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
        include_str!("../baseline_python_deps.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter(|line| skills::is_pinned_python_dep(line))
            .collect()
    });
    &PINS
}

#[cfg(test)]
mod tests {
    /// The declaration is read by pip, by the image generator, and by prompt
    /// composition; a line that does not parse as an exact pin would drop out
    /// of the set silently on every one of those paths.
    #[test]
    fn every_declared_baseline_line_is_an_exact_pin() {
        let declared = include_str!("../baseline_python_deps.txt")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect::<Vec<_>>();
        assert_eq!(declared, super::baseline_python_deps());
        assert!(declared.iter().any(|pin| pin.starts_with("numpy==")));
    }
}
