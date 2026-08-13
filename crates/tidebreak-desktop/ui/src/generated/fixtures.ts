// Generated from Rust. Do not edit.
//
// Regenerate: UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server
//
// Real serialized output from the server's own types, for the renderer's
// validator tests to consume. Hand-authored inputs encode what the author
// believed the wire looked like, which is how a rename can leave both test
// suites green and the app broken. These cannot drift from the server
// without this file changing. See docs/wire-types.md.

export const PENDING_APPROVAL = {
  "action": "exec",
  "approval": "exec_may_run_networked_command",
  "call_id": "00000000-0000-0000-0000-000000000001",
  "can_approve": true,
  "can_remember": true,
  "class": "sensitive",
  "grant_rungs": [
    "exact_action",
    {
      "command_prefix": {
        "tokens": 1
      }
    },
    "whole_tool"
  ],
  "preview": {
    "args": [
      "status"
    ],
    "command": "git",
    "cwd": ".",
    "files": [
      "documents/report.pdf"
    ],
    "summary": "Checking the repository status",
    "tool": "exec"
  },
  "turn_id": "00000000-0000-0000-0000-000000000002"
} as const;

export const PENDING_APPROVAL_WITHOUT_PREVIEW = {
  "action": "ask_user_questions",
  "approval": "unsupported",
  "call_id": "00000000-0000-0000-0000-000000000005",
  "can_approve": false,
  "can_remember": false,
  "class": "read_only",
  "grant_rungs": [],
  "turn_id": "00000000-0000-0000-0000-000000000002"
} as const;

export const PENDING_FOLDER_ACCESS_REQUEST = {
  "call_id": "00000000-0000-0000-0000-000000000003",
  "claimed": true,
  "folder_hint": "documents",
  "reason": "The assistant needs read access to files outside the folders connected to this conversation.",
  "turn_id": "00000000-0000-0000-0000-000000000004"
} as const;

export const PENDING_OUTPUT_WRITEBACK_REQUEST = {
  "call_id": "00000000-0000-0000-0000-000000000006",
  "claimed": false,
  "mode": "replace",
  "turn_id": "00000000-0000-0000-0000-000000000007"
} as const;
