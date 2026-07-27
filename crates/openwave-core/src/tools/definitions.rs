//! Model-facing names and descriptions for built-in tools.

use crate::tool::ToolSpec;

use super::{
    create_deliverable, list_dir as list_dir_tool, read_file as read_file_tool,
    write_file as write_file_tool,
};

pub(super) const READ_FILE: &str = "read_file";
pub(super) const LIST_DIR: &str = "list_dir";
pub(super) const WRITE_FILE: &str = "write_file";
pub(super) const CREATE_DELIVERABLE: &str = "create_deliverable";

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

pub(super) fn create_deliverable() -> ToolSpec {
    ToolSpec::for_args::<create_deliverable::Arguments>(
        CREATE_DELIVERABLE,
        "Create or update a user-visible text file for the current conversation. \
         Use this when the user asks for a report, plan, table, data file, web page, \
         or other file they can preview and save from OpenWave's Outputs view.",
    )
}
