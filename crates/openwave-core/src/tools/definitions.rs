//! Model-facing names, descriptions, and JSON Schemas for built-in tools.

use serde_json::json;

use crate::tool::ToolSpec;

pub(super) const READ_FILE: &str = "read_file";
pub(super) const LIST_DIR: &str = "list_dir";
pub(super) const WRITE_FILE: &str = "write_file";

pub(super) fn read_file() -> ToolSpec {
    ToolSpec {
        name: READ_FILE.into(),
        description: "Read a UTF-8 text file, relative to the private scratch directory.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Private-scratch-relative file path." }
            },
            "required": ["path"]
        }),
    }
}

pub(super) fn list_dir() -> ToolSpec {
    ToolSpec {
        name: LIST_DIR.into(),
        description: "List the entries of a private scratch directory (defaults to the root)."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Private-scratch-relative directory (optional)." }
            }
        }),
    }
}

pub(super) fn write_file() -> ToolSpec {
    ToolSpec {
        name: WRITE_FILE.into(),
        description: "Write a UTF-8 text file into private scratch, creating parent directories. \
                      Overwrites an existing file."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Private-scratch-relative file path." },
                "content": { "type": "string", "description": "File contents to write." }
            },
            "required": ["path", "content"]
        }),
    }
}
