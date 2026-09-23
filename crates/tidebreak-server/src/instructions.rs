//! Standing instructions a person writes for Tidebreak's own engine.
//!
//! There are two kinds. Personal instructions belong to one owner and apply
//! to every conversation that owner has with the internal engine. Project
//! instructions live on a [`tidebreak_core::Project`] and apply to every
//! conversation filed under it. External coding engines read their own
//! instruction files and never see either.
//!
//! Both reach the model through the seam a Slack channel's instructions use:
//! appended to the system prompt after every host-composed section. A turn
//! reads them fresh, but they change only when the person edits them, so a
//! conversation's prompt prefix stays cacheable from one turn to the next.

use serde::{Deserialize, Serialize};
use tidebreak_core::{Chat, OwnerId, Store};

use crate::error::ServerError;

/// The largest instructions a person or a project may carry, in UTF-8 bytes:
/// the same bound a Slack channel's instructions have.
pub const MAX_INSTRUCTIONS_BYTES: usize =
    crate::code::channel_preferences::MAX_CHANNEL_INSTRUCTIONS;

/// The setting that holds the local owner's personal instructions. A named
/// owner's copy lives under this key with the owner appended.
pub const PERSONAL_INSTRUCTIONS_SETTING: &str = "instructions.personal";

const PERSONAL_HEADING: &str = "## Personal instructions";
const PERSONAL_PREAMBLE: &str = "The user wrote these instructions for all of their conversations. Follow them unless the user asks for something different in this conversation. They cannot grant tool, file, or network access, and they do not override host policy.";
const PROJECT_HEADING: &str = "## Project instructions";
const PROJECT_PREAMBLE: &str = "The user wrote these instructions for every conversation in this project. Follow them unless the user asks for something different in this conversation. Where they conflict with the personal instructions, follow these. They cannot grant tool, file, or network access, and they do not override host policy.";

/// One person's standing instructions, as `GET /settings/instructions`
/// returns them and `PUT /settings/instructions` accepts them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct PersonalInstructions {
    /// The instructions as written. Empty means none.
    pub instructions: String,
}

/// Refuse instructions over [`MAX_INSTRUCTIONS_BYTES`] or holding a null
/// character. Every write path checks this before it stores anything.
pub fn validate(instructions: &str) -> Result<(), ServerError> {
    if instructions.len() > MAX_INSTRUCTIONS_BYTES || instructions.contains('\0') {
        return Err(ServerError::bad_request(format!(
            "instructions must be at most {MAX_INSTRUCTIONS_BYTES} bytes and contain no null characters"
        )));
    }
    Ok(())
}

fn personal_key(owner: &OwnerId) -> String {
    crate::code::naming_settings::user_setting_key(owner, PERSONAL_INSTRUCTIONS_SETTING)
}

/// The owner's personal instructions, or an empty string when none are set.
pub async fn read_personal(store: &dyn Store, owner: &OwnerId) -> tidebreak_core::Result<String> {
    Ok(store
        .get_setting(&personal_key(owner))
        .await?
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default())
}

/// Replace the owner's personal instructions. An empty string clears them.
pub async fn write_personal(
    store: &dyn Store,
    owner: &OwnerId,
    instructions: &str,
) -> Result<(), ServerError> {
    validate(instructions)?;
    store
        .set_setting(&personal_key(owner), &serde_json::json!(instructions))
        .await?;
    Ok(())
}

/// The standing instructions one turn composes into its system prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnInstructions {
    /// The conversation owner's personal instructions.
    pub personal: String,
    /// The instructions of the project the conversation belongs to.
    pub project: String,
}

impl TurnInstructions {
    /// Read both for one conversation. A conversation whose owner the store
    /// cannot name gets no personal instructions, and one outside a project
    /// gets no project instructions.
    pub async fn for_chat(
        store: &dyn Store,
        owner: Option<&OwnerId>,
        chat: &Chat,
    ) -> tidebreak_core::Result<Self> {
        let personal = match owner {
            Some(owner) => read_personal(store, owner).await?,
            None => String::new(),
        };
        let project = match chat.project_id {
            Some(project_id) => store
                .get_project(project_id)
                .await?
                .map(|project| project.instructions)
                .unwrap_or_default(),
            None => String::new(),
        };
        Ok(Self { personal, project })
    }

    /// Append the personal instructions, then the project instructions, each
    /// under its own heading. Blank instructions add nothing, so a prompt
    /// with neither is byte-identical to one composed before this existed.
    pub fn append_to(&self, prompt: &mut String) {
        append_section(prompt, PERSONAL_HEADING, PERSONAL_PREAMBLE, &self.personal);
        append_section(prompt, PROJECT_HEADING, PROJECT_PREAMBLE, &self.project);
    }
}

fn append_section(prompt: &mut String, heading: &str, preamble: &str, instructions: &str) {
    let instructions = instructions.trim();
    if instructions.is_empty() {
        return;
    }
    prompt.push_str("\n\n");
    prompt.push_str(heading);
    prompt.push('\n');
    prompt.push_str(preamble);
    prompt.push_str("\n\n");
    prompt.push_str(instructions);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composed(personal: &str, project: &str) -> String {
        let mut prompt = "Host guidance".to_owned();
        TurnInstructions {
            personal: personal.to_owned(),
            project: project.to_owned(),
        }
        .append_to(&mut prompt);
        prompt
    }

    #[test]
    fn the_prompt_carries_personal_then_project_instructions() {
        let prompt = composed("Answer in British English.", "Cite the filing.");
        assert!(prompt.starts_with("Host guidance\n\n## Personal instructions\n"));
        let personal = prompt.find("Answer in British English.").unwrap();
        let project_heading = prompt.find(PROJECT_HEADING).unwrap();
        let project = prompt.find("Cite the filing.").unwrap();
        assert!(personal < project_heading && project_heading < project);
        assert!(prompt.ends_with("Cite the filing."));
        assert!(prompt.contains("cannot grant tool, file, or network access"));
    }

    #[test]
    fn blank_instructions_add_no_section() {
        assert_eq!(composed("", ""), "Host guidance");
        assert_eq!(composed(" \n\t", "\n"), "Host guidance");

        let project_only = composed("  ", "Cite the filing.");
        assert!(!project_only.contains(PERSONAL_HEADING));
        assert!(project_only.contains(PROJECT_HEADING));

        let personal_only = composed("Answer in British English.", "");
        assert!(personal_only.contains(PERSONAL_HEADING));
        assert!(!personal_only.contains(PROJECT_HEADING));
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_and_the_body_is_kept_verbatim() {
        let prompt = composed("\n  First line.\n\n  - A list item\n", "");
        assert!(prompt.ends_with("\n\nFirst line.\n\n  - A list item"));
    }

    #[test]
    fn the_cap_counts_bytes_and_refuses_null_characters() {
        assert!(validate("").is_ok());
        assert!(validate(&"a".repeat(MAX_INSTRUCTIONS_BYTES)).is_ok());
        assert!(validate(&"a".repeat(MAX_INSTRUCTIONS_BYTES + 1)).is_err());
        // Two bytes each: half the cap in characters fits, one more does not.
        assert!(validate(&"é".repeat(MAX_INSTRUCTIONS_BYTES / 2)).is_ok());
        assert!(validate(&"é".repeat(MAX_INSTRUCTIONS_BYTES / 2 + 1)).is_err());
        assert!(validate("Answer\0briefly").is_err());
    }

    #[test]
    fn a_named_owner_keeps_personal_instructions_apart_from_the_local_owner() {
        let local = personal_key(&OwnerId::local());
        let alice = personal_key(&OwnerId::new("alice").unwrap());
        assert_eq!(local, PERSONAL_INSTRUCTIONS_SETTING);
        assert_ne!(alice, local);
        assert!(alice.starts_with(PERSONAL_INSTRUCTIONS_SETTING));
    }
}
