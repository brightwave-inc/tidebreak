//! Forced-confirmation tripwire classification.
//!
//! Given a control op and the target element's **normalized** `{role, label}`,
//! decide whether the action is *consequential* (irreversible /
//! externally-visible — a send, purchase, delete, or typing into a credential
//! field) and therefore needs an explicit per-action user confirmation on top
//! of the per-app `ControlApp` grant.
//!
//! This is the single, tested policy and it is platform-independent: the macOS
//! helper supplies the element's `AXRole` + `AXTitle`/`AXDescription`, a future
//! Windows backend supplies UIA `ControlType` + `Name`, and both flow through
//! this same classifier. The trust-independent signal is the element's *real*
//! label read at action time (a hijacked agent cannot talk its way past "the
//! button says Send"), so the classifier deliberately ignores anything the
//! agent supplies.
//!
//! Design stance: the lexicon is deliberately **short and calibrated**. The
//! per-app grant plus this gate covers the send/buy/delete risk with two
//! prompts instead of a card per action; over-asking on navigation-shaped words
//! (cancel, back, close, decline) trains click-through and makes the capability
//! unusable. So commit-shaped words trip the gate and navigation-shaped words
//! do not. This is a deliberate tradeoff recorded in decision record 0013: we
//! accept a slightly narrower tripwire in exchange for a consent flow the user
//! will actually read.

/// The control op being classified. Only ops that *activate* a specific
/// addressed element are classifiable here; `scroll` / `key_press` /
/// `focus_window` and coordinate-only ops have no activatable target label, so
/// the broker does not route them through this classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOp {
    /// A click/press on the element — consequential when the label reads like
    /// an action button.
    Click,
    /// Typing into the element — consequential when it is a
    /// secure/credential/payment field.
    TypeText,
    /// A key press. Not label-classified (a key has no activatable label);
    /// gated separately by the broker, which confirms the commit-shaped keys
    /// (chords and bare Return) rather than running them through `classify`.
    KeyPress,
}

/// The classification verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consequence {
    /// No tripwire — proceed without a per-action confirmation.
    Benign,
    /// Needs an explicit per-action confirmation; `reason` is the user-facing
    /// explanation.
    Consequential { reason: String },
}

/// Action words that mark a button as committing an external, hard-to-undo
/// effect. Matched as whole tokens (so "send" matches "Send" / "Resend" via
/// tokenization but not "ascending"). Lowercase.
///
/// Deliberately narrow: navigation- and dismissal-shaped words (cancel, back,
/// close, decline, confirm, accept, archive, block, report, approve) are NOT
/// here. Asking again on those is the over-asking failure mode — see decision
/// record 0013.
///
/// Locale bound: this lexicon is English. That is a deliberate accepted
/// bound (decision record 0013), not an omission to fill with a translation
/// dictionary. Navigation words in other languages remaining untripped is
/// the same over-ask tradeoff as keeping the English list short.
const RISK_ACTION_WORDS: &[&str] = &[
    "send",
    "resend",
    "unsend",
    "submit",
    "buy",
    "purchase",
    "pay",
    "checkout",
    "order",
    "subscribe",
    "unsubscribe",
    "delete",
    "remove",
    "trash",
    "discard",
    "destroy",
    "erase",
    "publish",
    "post",
    "transfer",
    "withdraw",
    "deposit",
    "wire",
    "overwrite",
    "sign",
    "uninstall",
];

/// Phrases (substring, normalized lowercase) that mark a field as a credential
/// / payment surface.
const CREDENTIAL_PHRASES: &[&str] = &[
    "password",
    "passcode",
    "credit card",
    "card number",
    "security code",
    "social security",
    "account number",
    "routing number",
    "one-time",
    "verification code",
    "card details",
];

/// Single tokens that mark a field as credential / payment (matched as whole
/// tokens, so "pin" does not match "ping" and "cvv" is exact). Includes the
/// no-separator compound forms ("pincode", "otpcode", …) that tokenize to one
/// word and so would not be caught by the multi-word [`CREDENTIAL_PHRASES`].
const CREDENTIAL_TOKENS: &[&str] = &[
    "password",
    "passcode",
    "pin",
    "ssn",
    "cvv",
    "cvc",
    "otp",
    "pincode",
    "otpcode",
    "cvvcode",
    "cvccode",
    "securitycode",
    "cardnumber",
];

/// Longest label echoed back into a user-facing reason (defensive: the label is
/// attacker-influenceable on-screen content, and an unbounded one should not
/// bloat the prompt or the audit record).
const MAX_LABEL_IN_REASON: usize = 80;

/// Classify a control op against the target element's normalized `role` /
/// `label`.
pub fn classify(op: ControlOp, role: Option<&str>, label: Option<&str>) -> Consequence {
    match op {
        ControlOp::Click => classify_click(role, label),
        ControlOp::TypeText => classify_type_text(role, label),
        // A key press has no element label to classify; the broker gates the
        // commit-shaped keys separately via [`key_press_needs_confirmation`].
        ControlOp::KeyPress => Consequence::Benign,
    }
}

/// Whether a key press is commit-shaped — a chord (any modifier held) or a bare
/// Return — and so needs an explicit confirmation. Keyboard shortcuts are the
/// primary commit path in the apps the gate exists for (Cmd+Shift+D / Cmd+Enter
/// sends mail, Return sends a chat message, Cmd+Delete trashes the selection),
/// and no element label describes them, so the broker confirms these rather
/// than classifying an element. Unmodified navigation and editing keys (arrows,
/// Tab, letters, Escape) are not confirmed.
pub fn key_press_needs_confirmation(key: &str, has_modifier: bool) -> bool {
    if has_modifier {
        return true;
    }
    matches!(
        key.trim().to_lowercase().as_str(),
        "return" | "enter" | "forwarddelete" | "delete"
    )
}

fn classify_click(role: Option<&str>, label: Option<&str>) -> Consequence {
    // Icon-only activatable controls have no label the lexicon can read, so
    // they cannot be classified as navigation and must not fail open as
    // Benign. See decision record 0013.
    if is_activatable_control_role(role) && label_missing_or_empty(label) {
        return Consequence::Consequential {
            reason: "This control has no accessible label, so it cannot be \
                     classified as navigation."
                .to_string(),
        };
    }
    let Some(label) = label else {
        return Consequence::Benign;
    };
    if tokens(label).any(|t| RISK_ACTION_WORDS.contains(&t.as_str())) {
        return Consequence::Consequential {
            reason: format!(
                "This activates \u{201c}{}\u{201d}, which looks like a consequential action \
                 (e.g. send / buy / delete) that cannot easily be undone.",
                truncate_label(label),
            ),
        };
    }
    Consequence::Benign
}

/// Buttons, menu items, and links — the activatable roles a click can fire.
/// Matches the AX names and any role whose lowercase form contains those
/// tokens, so a future UIA backend's `Button` / `MenuItem` / `Hyperlink`
/// strings take the same path.
fn is_activatable_control_role(role: Option<&str>) -> bool {
    let Some(role) = role else {
        return false;
    };
    let lower = role.to_lowercase();
    lower.contains("button") || lower.contains("menuitem") || lower.contains("link")
}

fn label_missing_or_empty(label: Option<&str>) -> bool {
    label.is_none_or(|label| label.trim().is_empty())
}

fn classify_type_text(role: Option<&str>, label: Option<&str>) -> Consequence {
    // Secure text fields: macOS reports role "AXSecureTextField" (a future
    // Windows UIA backend folds its password pattern into the role string), so
    // a "secure" substring in the role is the portable signal.
    if let Some(role) = role {
        if role.to_lowercase().contains("secure") {
            return Consequence::Consequential {
                reason: "This types into a secure / password field.".to_string(),
            };
        }
    }
    if let Some(label) = label {
        let normalized = label.to_lowercase();
        let phrase_hit = CREDENTIAL_PHRASES.iter().any(|p| normalized.contains(p));
        let token_hit = tokens(label).any(|t| CREDENTIAL_TOKENS.contains(&t.as_str()));
        if phrase_hit || token_hit {
            return Consequence::Consequential {
                reason: format!(
                    "This types into what looks like a credential / payment field \
                     (\u{201c}{}\u{201d}).",
                    truncate_label(label),
                ),
            };
        }
    }
    Consequence::Benign
}

/// Lowercase whole-word-ish tokens of a label: split on any non-alphanumeric
/// char so punctuation, spacing, and case do not hide a risk word ("Send…" /
/// "SEND" / "send/receive" all yield "send").
fn tokens(label: &str) -> impl Iterator<Item = String> + '_ {
    label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
}

/// Bound an attacker-influenceable element label before it is shown to the user
/// or recorded — used both for the classifier `reason` and (by the broker) for
/// the `target_label` carried in a confirmation prompt, so neither path can
/// bloat a confirmation or the audit record with an oversized button title.
pub fn truncate_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.chars().count() <= MAX_LABEL_IN_REASON {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX_LABEL_IN_REASON).collect();
    format!("{cut}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_consequential(c: &Consequence) -> bool {
        matches!(c, Consequence::Consequential { .. })
    }

    #[test]
    fn click_on_commit_buttons_is_consequential() {
        for label in [
            "Send",
            "send",
            "Send Now",
            "SEND",
            "Send…",
            "Resend",
            "Buy now",
            "Purchase",
            "Pay $99.99",
            "Place order",
            "Delete",
            "Move to Trash",
            "Submit",
            "Publish",
            "Post",
            "Transfer funds",
            "Withdraw",
            "Discard draft",
            "Sign document",
            "Overwrite",
        ] {
            assert!(
                is_consequential(&classify(ControlOp::Click, Some("AXButton"), Some(label))),
                "expected {label:?} to be consequential",
            );
        }
    }

    /// The calibration change: navigation- and dismissal-shaped labels must NOT
    /// trip the gate. Over-asking on these is exactly the failure mode the
    /// per-app grant exists to avoid — a tripwire that fires on "Cancel" trains
    /// the user to confirm everything without reading.
    #[test]
    fn click_on_navigation_and_dismissal_is_benign() {
        for label in [
            "Open",
            "Close",
            "Cancel",
            "Cancel and go back",
            "Confirm",
            "Accept",
            "Decline",
            "Archive",
            "Block",
            "Report",
            "Approve",
            "Back",
            "Settings",
            "Add row",
            "Format",
            "Bold",
            "Search",
            "Next",
            "Previous",
            "Zoom in",
            "New tab",
            "Refresh",
            "Help",
            "Done",
            "Save",
            "OK",
        ] {
            assert_eq!(
                classify(ControlOp::Click, Some("AXButton"), Some(label)),
                Consequence::Benign,
                "expected {label:?} to be benign (navigation/dismissal must not over-ask)",
            );
        }
    }

    #[test]
    fn click_does_not_match_substrings_inside_other_words() {
        // "send" is a substring of "ascending" / "addsender" but not a token →
        // benign.
        assert_eq!(
            classify(ControlOp::Click, Some("AXButton"), Some("Ascending")),
            Consequence::Benign,
        );
        assert_eq!(
            classify(ControlOp::Click, Some("AXMenuItem"), Some("Calendar")),
            Consequence::Benign,
        );
    }

    #[test]
    fn unlabeled_activatable_click_is_consequential() {
        for role in ["AXButton", "AXMenuItem", "AXLink", "Button", "menuitem"] {
            let verdict = classify(ControlOp::Click, Some(role), None);
            assert!(
                is_consequential(&verdict),
                "expected unlabeled {role:?} click to be consequential",
            );
        }
        assert!(is_consequential(&classify(
            ControlOp::Click,
            Some("AXButton"),
            Some(""),
        )));
        // A labeled navigation button is still the over-ask we refuse.
        assert_eq!(
            classify(ControlOp::Click, Some("AXButton"), Some("Cancel")),
            Consequence::Benign,
        );
    }

    #[test]
    fn unlabeled_click_without_activatable_role_is_benign() {
        assert_eq!(classify(ControlOp::Click, None, None), Consequence::Benign);
        assert_eq!(
            classify(ControlOp::Click, None, Some("")),
            Consequence::Benign
        );
    }

    #[test]
    fn type_into_secure_field_is_consequential() {
        assert!(is_consequential(&classify(
            ControlOp::TypeText,
            Some("AXSecureTextField"),
            None,
        )));
        // case-insensitive role match
        assert!(is_consequential(&classify(
            ControlOp::TypeText,
            Some("axsecuretextfield"),
            Some("anything"),
        )));
    }

    #[test]
    fn type_into_credential_labeled_field_is_consequential() {
        for (role, label) in [
            ("AXTextField", "Password"),
            ("AXTextField", "Enter your passcode"),
            ("AXTextField", "Credit card number"),
            ("AXTextField", "CVV"),
            ("AXTextField", "Verification code"),
            ("AXTextField", "Routing number"),
        ] {
            assert!(
                is_consequential(&classify(ControlOp::TypeText, Some(role), Some(label))),
                "expected typing into {label:?} to be consequential",
            );
        }
    }

    #[test]
    fn type_into_ordinary_field_is_benign() {
        for (role, label) in [
            ("AXTextField", "Search"),
            ("AXTextArea", "Message"),
            ("AXTextField", "Subject"),
            ("AXSearchField", "Find"),
        ] {
            assert_eq!(
                classify(ControlOp::TypeText, Some(role), Some(label)),
                Consequence::Benign,
                "expected typing into {label:?} to be benign",
            );
        }
    }

    #[test]
    fn truncate_label_bounds_attacker_controlled_text() {
        let long = "x".repeat(500);
        let truncated = truncate_label(&long);
        assert!(truncated.chars().count() <= MAX_LABEL_IN_REASON + 1);

        let spaced = "   Send now   ";
        assert_eq!(truncate_label(spaced), "Send now");
    }
}
