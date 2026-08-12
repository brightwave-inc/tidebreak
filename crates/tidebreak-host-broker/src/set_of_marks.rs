//! Set-of-Marks extraction: turn an app's accessibility tree into a bounded,
//! numbered list of interactive elements the agent can reference by number.
//!
//! The numbered marks are drawn over the screenshot (the helper renders them)
//! so the model can ground "click the Send button" to a stable number, and each
//! mark resolves back to the element's index-path id + fingerprint so acting on
//! it goes through the normal stale-element re-check. Extraction is a pure
//! function over the tree JSON, ranked in reading order (top-to-bottom, then
//! left-to-right), so a row of controls numbers left-to-right rather than by
//! sub-pixel vertical jitter.

use serde::{Deserialize, Serialize};

/// Roles considered interactive / targetable. Lowercase; matched against the
/// lowercased AX role. This is the generic list — app knowledge (which specific
/// control in a specific app) lives in plugin skills, not here.
const INTERACTIVE_ROLES: &[&str] = &[
    "axbutton",
    "axlink",
    "axcheckbox",
    "axradiobutton",
    "axpopupbutton",
    "axmenubutton",
    "axmenuitem",
    "axmenubaritem",
    "axtextfield",
    "axtextarea",
    "axsecuretextfield",
    "axsearchfield",
    "axcombobox",
    "axtab",
    "axdisclosuretriangle",
    "axslider",
    "axincrementor",
    "axstepper",
    "axsegmentedcontrol",
    "axcolorwell",
    "axswitch",
    "axtogglebutton",
];

/// Max label length echoed into a mark (the label is app/on-screen content —
/// attacker-influenceable and possibly long — so it should not bloat the model
/// context or an annotation).
const MAX_LABEL_LEN: usize = 100;

/// Rows within this many points of each other are treated as the same line for
/// reading-order ranking, so near-aligned controls number left-to-right rather
/// than by sub-pixel `y` jitter.
const ROW_BUCKET_PX: f64 = 12.0;

/// A node of the helper's AX tree, deserialized from the opaque `tree` JSON.
/// Mirrors the helper's node shape. Defensive: every field but the recursive
/// `children` is optional so a partial or odd tree never fails extraction (it
/// just yields fewer marks).
#[derive(Debug, Deserialize)]
struct AxNode {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    fingerprint: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    frame: Option<MarkFrame>,
    #[serde(default)]
    children: Vec<AxNode>,
}

/// On-screen rectangle of a mark (global, top-left origin — the same space the
/// control ops act in).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MarkFrame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl MarkFrame {
    fn is_on_screen(&self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }
    fn center_y(&self) -> f64 {
        self.y + self.height / 2.0
    }
}

/// One numbered, targetable element.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    /// 1-based number the user and agent see on the annotated screenshot and
    /// pass to "click mark N".
    pub mark: u32,
    /// The element's index-path id + fingerprint, so a mark resolves to a
    /// normal control op.
    pub element_id: String,
    pub element_fingerprint: String,
    /// Raw role (e.g. "AXButton"), kept for the agent's judgment.
    pub role: String,
    /// Best human label: title, else value, else the de-prefixed role. Bounded
    /// by [`MAX_LABEL_LEN`].
    pub label: String,
    pub frame: MarkFrame,
}

/// Extract up to `max_marks` interactive marks from an AX `tree`, ranked in
/// reading order (top-to-bottom, then left-to-right). Returns an empty vec for
/// a missing or unparsable tree.
pub fn extract_marks(tree: &serde_json::Value, max_marks: usize) -> Vec<Mark> {
    let Ok(root) = serde_json::from_value::<AxNode>(tree.clone()) else {
        return Vec::new();
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    collect(&root, &mut candidates);

    // Reading order: bucket the vertical center into rows so a row of controls
    // numbers left-to-right, then by x within the row. Stable so equal keys
    // keep tree order.
    candidates.sort_by(|a, b| {
        let row_a = (a.frame.center_y() / ROW_BUCKET_PX).floor() as i64;
        let row_b = (b.frame.center_y() / ROW_BUCKET_PX).floor() as i64;
        row_a.cmp(&row_b).then(
            a.frame
                .x
                .partial_cmp(&b.frame.x)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    candidates
        .into_iter()
        .take(max_marks)
        .enumerate()
        .map(|(index, candidate)| Mark {
            mark: (index + 1) as u32,
            element_id: candidate.element_id,
            element_fingerprint: candidate.element_fingerprint,
            role: candidate.role,
            label: candidate.label,
            frame: candidate.frame,
        })
        .collect()
}

struct Candidate {
    element_id: String,
    element_fingerprint: String,
    role: String,
    label: String,
    frame: MarkFrame,
}

fn collect(node: &AxNode, out: &mut Vec<Candidate>) {
    if let Some(candidate) = as_candidate(node) {
        out.push(candidate);
    }
    for child in &node.children {
        collect(child, out);
    }
}

/// A node is a candidate iff it has an interactive role, an on-screen frame,
/// and a usable identity (`id` + `fingerprint` — without them a mark could not
/// be acted on).
fn as_candidate(node: &AxNode) -> Option<Candidate> {
    let role = node.role.as_deref()?;
    if !INTERACTIVE_ROLES.contains(&role.to_lowercase().as_str()) {
        return None;
    }
    let frame = node.frame.filter(MarkFrame::is_on_screen)?;
    let (Some(element_id), Some(element_fingerprint)) = (&node.id, &node.fingerprint) else {
        return None;
    };
    Some(Candidate {
        element_id: element_id.clone(),
        element_fingerprint: element_fingerprint.clone(),
        role: role.to_string(),
        label: label_for(node, role),
        frame,
    })
}

/// Best label: title, else value, else the de-prefixed role ("AXButton" →
/// "button"). Trimmed and bounded.
fn label_for(node: &AxNode, role: &str) -> String {
    let raw = node
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            node.value
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_string)
        .unwrap_or_else(|| role.strip_prefix("AX").unwrap_or(role).to_lowercase());

    if raw.chars().count() <= MAX_LABEL_LEN {
        raw
    } else {
        let cut: String = raw.chars().take(MAX_LABEL_LEN).collect();
        format!("{cut}\u{2026}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, role: &str, title: Option<&str>, x: f64, y: f64) -> serde_json::Value {
        let mut value = json!({
            "id": id,
            "fingerprint": format!("fp-{id}"),
            "role": role,
            "frame": { "x": x, "y": y, "width": 80.0, "height": 20.0 },
            "children": [],
        });
        if let Some(title) = title {
            value["title"] = json!(title);
        }
        value
    }

    #[test]
    fn extracts_only_interactive_nodes_numbered_in_reading_order() {
        // A toolbar row (two buttons) above a text field, with a
        // non-interactive group + static text mixed in. Marks should be the 3
        // interactive nodes, numbered top-to-bottom then left-to-right.
        let tree = json!({
            "id": "0", "fingerprint": "fp-0", "role": "AXApplication",
            "frame": { "x": 0.0, "y": 0.0, "width": 800.0, "height": 600.0 },
            "children": [
                { "id": "0.0", "fingerprint": "fp-grp", "role": "AXGroup",
                  "frame": { "x": 0.0, "y": 0.0, "width": 800.0, "height": 40.0 },
                  "children": [
                    node("0.0.1", "AXButton", Some("Send"), 200.0, 10.0),
                    node("0.0.0", "AXButton", Some("Cancel"), 100.0, 10.0),
                  ] },
                node("0.1", "AXStaticText", Some("just text"), 0.0, 100.0),
                node("0.2", "AXTextField", None, 50.0, 100.0),
            ],
        });

        let marks = extract_marks(&tree, 50);
        assert_eq!(marks.len(), 3);
        // Reading order: Cancel (x=100) then Send (x=200) on the top row, then
        // the text field below.
        assert_eq!(marks[0].mark, 1);
        assert_eq!(marks[0].label, "Cancel");
        assert_eq!(marks[1].label, "Send");
        assert_eq!(marks[2].role, "AXTextField");
        assert_eq!(marks[2].mark, 3);
        // Each mark carries the identity needed to act on it.
        assert_eq!(marks[0].element_id, "0.0.0");
        assert_eq!(marks[0].element_fingerprint, "fp-0.0.0");
    }

    #[test]
    fn label_falls_back_title_then_value_then_role() {
        let mut with_value = node("0.1", "AXTextField", None, 0.0, 0.0);
        with_value["value"] = json!("hello@example.com");
        let tree = json!({
            "id": "0", "fingerprint": "fp-0", "role": "AXApplication", "children": [
                node("0.0", "AXButton", Some("Save"), 0.0, 0.0),   // title
                with_value,                                          // value
                node("0.2", "AXCheckBox", None, 0.0, 40.0),         // role -> "checkbox"
            ],
        });
        let marks = extract_marks(&tree, 50);
        let by_id = |id: &str| marks.iter().find(|m| m.element_id == id).unwrap();
        assert_eq!(by_id("0.0").label, "Save");
        assert_eq!(by_id("0.1").label, "hello@example.com");
        assert_eq!(by_id("0.2").label, "checkbox");
    }

    #[test]
    fn excludes_zero_size_and_identityless_nodes() {
        let tree = json!({
            "id": "0", "fingerprint": "fp-0", "role": "AXApplication", "children": [
                // zero-size (off-screen / collapsed) → excluded
                { "id": "0.0", "fingerprint": "fp-a", "role": "AXButton", "title": "Hidden",
                  "frame": { "x": 0.0, "y": 0.0, "width": 0.0, "height": 0.0 }, "children": [] },
                // missing fingerprint → cannot be acted on → excluded
                { "id": "0.1", "role": "AXButton", "title": "NoFp",
                  "frame": { "x": 0.0, "y": 0.0, "width": 80.0, "height": 20.0 }, "children": [] },
                node("0.2", "AXButton", Some("Real"), 0.0, 0.0),
            ],
        });
        let marks = extract_marks(&tree, 50);
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].label, "Real");
    }

    #[test]
    fn caps_at_max_marks() {
        let children: Vec<_> = (0..10)
            .map(|i| {
                node(
                    &format!("0.{i}"),
                    "AXButton",
                    Some("b"),
                    0.0,
                    (i as f64) * 30.0,
                )
            })
            .collect();
        let tree = json!({
            "id": "0", "fingerprint": "fp-0", "role": "AXApplication", "children": children,
        });
        let marks = extract_marks(&tree, 3);
        assert_eq!(marks.len(), 3);
        assert_eq!(
            marks.iter().map(|m| m.mark).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn unparsable_or_empty_tree_yields_no_marks() {
        assert!(extract_marks(&serde_json::Value::Null, 50).is_empty());
        assert!(extract_marks(&json!("not a tree"), 50).is_empty());
        assert!(extract_marks(&json!({ "id": "0", "role": "AXApplication" }), 50).is_empty());
    }

    #[test]
    fn long_label_is_truncated() {
        let long = "x".repeat(500);
        let tree = json!({
            "id": "0", "fingerprint": "fp-0", "role": "AXApplication", "children": [
                node("0.0", "AXButton", Some(&long), 0.0, 0.0),
            ],
        });
        let marks = extract_marks(&tree, 50);
        assert!(marks[0].label.chars().count() <= MAX_LABEL_LEN + 1);
        assert!(marks[0].label.ends_with('\u{2026}'));
    }
}
