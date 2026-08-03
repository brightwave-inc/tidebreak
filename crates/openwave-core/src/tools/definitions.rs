//! Model-facing names and descriptions for built-in tools.

use crate::tool::ToolSpec;

use super::{
    create_app as create_app_tool, list_dir as list_dir_tool, read_file as read_file_tool,
    write_file as write_file_tool,
};

pub(super) const READ_FILE: &str = "read_file";
pub(super) const LIST_DIR: &str = "list_dir";
pub(super) const WRITE_FILE: &str = "write_file";

pub(super) fn read_file() -> ToolSpec {
    ToolSpec::for_args::<read_file_tool::Arguments>(
        READ_FILE,
        "Read a UTF-8 text file, relative to the private scratch directory.",
    )
}

pub(super) fn list_dir() -> ToolSpec {
    ToolSpec::for_args::<list_dir_tool::Arguments>(
        LIST_DIR,
        "List the entries of a private scratch directory (defaults to the root).",
    )
}

pub(super) fn write_file() -> ToolSpec {
    ToolSpec::for_args::<write_file_tool::Arguments>(
        WRITE_FILE,
        "Write a UTF-8 text file into private scratch, creating parent directories. \
         Overwrites an existing file.",
    )
}

pub(super) fn create_app() -> ToolSpec {
    ToolSpec::for_args::<create_app_tool::Arguments>(
        crate::local_app::CREATE_APP_TOOL,
        "Publish a local mini-app the user can reopen from their Apps library: a \
         complete self-contained HTML document plus a manifest naming the app and \
         pinning the exact mounted MCP tools it may call through the host. Each \
         manifest binding names a connected app by its id and lists full mounted \
         tool names (`mcp__{namespace}__{tool}`) under it; the configured \
         connected apps and their ids are listed at the end of this description. \
         The app renders in a sandboxed frame with no network access; pinned \
         tools run only after the user grants them. Pass the app_id from an \
         earlier create_app result to publish a new revision of that app — \
         revisions append, never overwrite.",
    )
}
