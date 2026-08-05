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
        "Read a UTF-8 text file, relative to the private scratch directory. Files under \
         output/ are readable here, but only exec can write them.",
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
         Overwrites an existing file. Scratch files are intermediate work, not \
         user-visible outputs: output/ is reserved and a write there is refused, \
         because a user-visible output is published by saving it into output/ from \
         an exec command.",
    )
}

pub(super) fn create_app() -> ToolSpec {
    ToolSpec::for_args::<create_app_tool::Arguments>(
        crate::local_app::CREATE_APP_TOOL,
        "Publish a local mini-app the user can reopen from their Apps library: a \
         complete self-contained HTML document plus a manifest naming the app and \
         pinning the exact capabilities it may call through the host. A manifest \
         binding names either a rest_api connected app by id with the declared \
         OpenAPI operationIds it may execute (`operation_ids`), or an approved \
         connected folder by root id with `access: \"read\"` or `\"read_write\"` — \
         request write access only when the app needs it, since it is a louder \
         consent. Mounted MCP tools cannot be bound. The available connected apps \
         and folders and their ids are listed at the end of this description. The \
         app renders in a sandboxed frame with no network access; pinned \
         capabilities run only after the user grants them. From inside the frame \
         the bundle calls them by posting JSON-RPC 2.0 to its parent window: \
         `operations/call` with `{operation_id, parameters?, body?}`, whose \
         result carries the raw response as `{status, content_type, \
         body_base64}`; or `fs/list`, `fs/read`, and `fs/write` with `{folder, \
         path?, content_base64?, replace?}`, whose results carry `{entries}`, \
         `{content_base64}`, or `{replaced}` with file bytes base64-encoded both \
         ways. Pass the app_id from an earlier create_app result to publish a \
         new revision of that app — revisions append, never overwrite.",
    )
}
