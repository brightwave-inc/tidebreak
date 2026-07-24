//! Model-facing names, descriptions, and JSON Schemas for built-in tools.

use serde_json::json;

use crate::deliverable::{MAX_DELIVERABLE_BYTES, MAX_DELIVERABLE_NAME_CHARS};
use crate::tool::ToolSpec;

pub(super) const READ_FILE: &str = "read_file";
pub(super) const LIST_DIR: &str = "list_dir";
pub(super) const WRITE_FILE: &str = "write_file";
pub(super) const CREATE_DELIVERABLE: &str = "create_deliverable";

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

pub(super) fn create_deliverable() -> ToolSpec {
    ToolSpec {
        name: CREATE_DELIVERABLE.into(),
        description: "Create or update a user-visible text file for the current conversation. \
                      Use this when the user asks for a report, plan, table, data file, web page, \
                      or other file they can preview and save from OpenWave's Outputs view."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "filename": {
                    "type": "string",
                    "description": "Portable output filename ending in .md, .txt, .csv, .json, or .html.",
                    "minLength": 1,
                    "maxLength": MAX_DELIVERABLE_NAME_CHARS
                },
                "content": {
                    "type": "string",
                    "description": "Complete UTF-8 text contents of the output file (maximum 512 KiB).",
                    "minLength": 1,
                    "maxLength": MAX_DELIVERABLE_BYTES
                }
            },
            "required": ["filename", "content"],
            "additionalProperties": false
        }),
    }
}
