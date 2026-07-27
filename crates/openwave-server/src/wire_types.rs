//! Generation of the desktop's TypeScript wire types from these Rust
//! definitions.
//!
//! The renderer used to describe the server's JSON in hand-written TypeScript.
//! Nothing connected the two, so both sides could be green over a broken
//! contract — and twice were, both times on optionality.
//!
//! Everything here is test-only. The generator is a test so that the checked-in
//! output is verified by the same command that produces it: run normally it
//! compares and fails on a diff, and CI therefore fails on a stale file. The
//! check is the guarantee, not the generation.
//!
//! See `docs/wire-types.md` for the workflow and for what is deliberately left
//! hand-written.

#[cfg(test)]
pub(crate) mod generate {
    use std::any::TypeId;
    use std::collections::{BTreeMap, HashSet};

    use ts_rs::{Config, Dependency, TypeVisitor, TS};

    /// One generated declaration, keyed by its TypeScript name.
    type Declarations = BTreeMap<String, String>;

    /// The generator settings, which are part of the contract.
    ///
    /// `ts-rs` renders `i64` as `bigint` by default. That is the right choice for
    /// a language binding and the wrong one here: these types describe what
    /// `JSON.parse` produces from the server's response, and `JSON.parse` yields
    /// a `number` for a JSON number. A `bigint` declaration would be false about
    /// every value the renderer actually receives.
    ///
    /// The cost is the usual one — a JSON number above 2^53 loses precision in
    /// JavaScript. The only `i64` on this surface is the journal sequence
    /// counter, which is nowhere near that, and declaring `bigint` would not
    /// have prevented the loss anyway since it happens during parsing.
    pub(crate) fn config() -> Config {
        Config::new().with_large_int("number")
    }

    /// Collects the declaration of every type reachable from the roots.
    ///
    /// Walking the closure rather than listing every type is what keeps this
    /// honest: adding a field of a new type to something already generated
    /// pulls that type in automatically, so the output cannot quietly describe
    /// fewer types than the server can send.
    struct Collect<'a> {
        cfg: &'a Config,
        seen: HashSet<TypeId>,
        out: &'a mut Declarations,
    }

    impl TypeVisitor for Collect<'_> {
        fn visit<T: TS + 'static + ?Sized>(&mut self) {
            // `Dependency::from_ty` is `None` for anything without its own
            // declaration — primitives, `Vec`, `Option`, `String`. Those still
            // need visiting so the recursion reaches their inner types, but they
            // are not themselves declarations.
            let declares = Dependency::from_ty::<T>(self.cfg).is_some();
            if !self.seen.insert(TypeId::of::<T>()) {
                return;
            }
            if declares {
                let name = T::ident(self.cfg);
                let docs = T::docs().unwrap_or_default();
                self.out
                    .insert(name, format!("{docs}export {}\n", T::decl(self.cfg)));
            }
            T::visit_dependencies(self);
        }
    }

    /// Add `T` and everything it references to `out`.
    pub(crate) fn collect_from<T: TS + 'static>(cfg: &Config, out: &mut Declarations) {
        let mut visitor = Collect {
            cfg,
            seen: HashSet::new(),
            out,
        };
        visitor.visit::<T>();
    }

    /// The runtime tool-name list, emitted beside the generated union.
    ///
    /// The desktop needs these names at runtime to check a provider-supplied
    /// string against the allowlist, and TypeScript types are erased. So the
    /// generated union is parsed back into its members and re-emitted as a
    /// `const`, with [`super::tests::the_runtime_tool_list_reconstructs_the_union`]
    /// verifying the parse by rebuilding the union from it.
    pub(crate) fn tool_names_from_union(decl: &str) -> Vec<String> {
        let body = decl
            .split_once('=')
            .expect("a ts-rs type alias always has a right-hand side")
            .1;
        body.trim()
            .trim_end_matches(';')
            .split('|')
            .map(|member| member.trim().trim_matches('"').to_owned())
            .collect()
    }

    /// The `RENDERER_TOOL_NAMES` const declaration.
    pub(crate) fn render_tool_name_list(names: &[String]) -> String {
        let listed = names
            .iter()
            .map(|name| format!("  \"{name}\",\n"))
            .collect::<String>();
        format!(
            "/**\n\
             \x20* Every tool name the renderer will accept, at runtime.\n\
             \x20*\n\
             \x20* An allowlist, not a display transformation. Tool events come from\n\
             \x20* providers, so a name outside this set must never reach a card, an icon,\n\
             \x20* or a copy table. The server folds anything unrecognized to `other`.\n\
             \x20*\n\
             \x20* Emitted from the same enum as `RendererToolName` above, so the runtime\n\
             \x20* list and the type cannot disagree.\n\
             \x20*/\n\
             export const RENDERER_TOOL_NAMES = [\n{listed}] as const;\n"
        )
    }

    /// Render the generated module, header and all, so ordering and preamble are
    /// part of the diff a reviewer sees.
    pub(crate) fn render(declarations: &Declarations, trailing: &[String]) -> String {
        let body = declarations
            .values()
            .map(String::as_str)
            .chain(trailing.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "// Generated from Rust. Do not edit.\n\
             //\n\
             // Regenerate: UPDATE_WIRE_TYPES=1 cargo test -p openwave-server\n\
             //\n\
             // These describe the JSON the server actually sends, derived from the same\n\
             // serde attributes serde itself reads. They are the input to the runtime\n\
             // validators in api.ts, not a replacement for them: the validators enforce\n\
             // bounds, reject control characters, and cross-check server policy, none of\n\
             // which a type can express. See docs/wire-types.md.\n\
             \n\
             {body}"
        )
    }

    /// Every field of a `TS`-deriving type whose key serde may omit, paired with
    /// whether the type will be told to declare it optional.
    ///
    /// This exists because of the one thing `ts-rs` gets wrong for this codebase.
    /// It decides a field is optional from `maybe_omitted && has_default`, and
    /// serde treats a missing `Option` as `None` without needing
    /// `#[serde(default)]` — so an `Option` field with only
    /// `skip_serializing_if` renders as `T | null`, claiming a key the server
    /// never sends. That is exactly the mismatch that shipped twice before any
    /// of this was generated, and generating it does not fix it by itself.
    ///
    /// Scoped to the span of each `TS`-deriving item, so a `skip_serializing_if`
    /// on a neighbouring non-generated type in the same file is not flagged.
    pub(crate) fn omittable_fields_missing_optional(source: &str) -> Vec<String> {
        let mut findings = Vec::new();
        for span in ts_deriving_items(source) {
            // Attributes accumulate above a field, so split on the field
            // boundary: each chunk is one field's attribute cluster plus its
            // declaration.
            for cluster in span.split(",\n") {
                let omits = cluster.contains("skip_serializing_if")
                    || cluster.contains("skip_serializing)")
                    || cluster.contains("skip_serializing,");
                if !omits {
                    continue;
                }
                let declared_optional =
                    cluster.contains("ts(optional") || cluster.contains("serde(default");
                if !declared_optional {
                    let field = cluster
                        .lines()
                        .filter(|line| !line.trim_start().starts_with('#'))
                        .find(|line| line.contains(':'))
                        .unwrap_or("<unknown field>")
                        .trim();
                    findings.push(field.to_owned());
                }
            }
        }
        findings
    }

    /// Text spans of items whose derive list includes `TS`, from the derive to
    /// the closing brace of the item.
    fn ts_deriving_items(source: &str) -> Vec<&str> {
        let mut spans = Vec::new();
        let mut rest = source;
        while let Some(at) = rest.find("#[derive(") {
            let after = &rest[at..];
            let Some(close) = after.find(")]") else { break };
            const DERIVE: &str = "#[derive(";
            let derives = &after[DERIVE.len()..close];
            rest = &after[close + 2..];
            if !derives.split(',').any(|d| d.trim() == "TS") {
                continue;
            }
            // Walk to the closing brace of the item this derive belongs to.
            let mut depth = 0usize;
            let mut end = rest.len();
            for (index, ch) in rest.char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = index + 1;
                            break;
                        }
                    }
                    // A unit struct or tuple struct ends at the semicolon.
                    ';' if depth == 0 => {
                        end = index + 1;
                        break;
                    }
                    _ => {}
                }
            }
            spans.push(&rest[..end]);
        }
        spans
    }

    /// Compare `rendered` against the checked-in file, or rewrite it when
    /// `UPDATE_WIRE_TYPES` is set.
    pub(crate) fn check_or_update(relative_path: &str, rendered: &str, regenerate_with: &str) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative_path);
        if std::env::var_os("UPDATE_WIRE_TYPES").is_some() {
            std::fs::create_dir_all(path.parent().expect("the bindings path has a parent"))
                .expect("the bindings directory is creatable");
            std::fs::write(&path, rendered).expect("the bindings path is writable");
            return;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            existing, rendered,
            "the generated wire types are out of date; regenerate with {regenerate_with}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::generate;
    use std::collections::BTreeMap;

    /// Path of the generated bindings, relative to this crate.
    ///
    /// One module, not one per root: the roots share types, and two files would
    /// each declare `RendererToolName`, which cannot both be imported.
    const BINDINGS: &str = "../openwave-desktop/ui/src/generated/wire.ts";

    const REGENERATE: &str = "UPDATE_WIRE_TYPES=1 cargo test -p openwave-server";

    fn event_declarations() -> BTreeMap<String, String> {
        let cfg = generate::config();
        let mut out = BTreeMap::new();
        // One root. Everything the event stream can carry is reachable from it,
        // including the shared preview and approval types the REST surface uses.
        generate::collect_from::<crate::event_projection::RendererSequencedEvent>(&cfg, &mut out);
        out
    }

    fn rendered_bindings() -> String {
        let declarations = event_declarations();
        let union = declarations
            .get("RendererToolName")
            .expect("the vocabulary is reachable from the event root");
        let names = generate::tool_names_from_union(union);
        generate::render(&declarations, &[generate::render_tool_name_list(&names)])
    }

    /// The WebSocket frame types, which until now had no contract at all: the
    /// renderer parses each frame with a bare cast and no runtime validation, so
    /// the generated type is the only thing describing that payload.
    #[test]
    fn the_generated_bindings_are_current() {
        generate::check_or_update(BINDINGS, &rendered_bindings(), REGENERATE);
    }

    /// The runtime list is parsed out of the generated union, so the parse has to
    /// be exact. Rebuilding the union from the parsed names and comparing it to
    /// what `ts-rs` produced is what makes that safe: a dropped, merged, or
    /// mis-trimmed member cannot survive the round trip.
    #[test]
    fn the_runtime_tool_list_reconstructs_the_union() {
        let declarations = event_declarations();
        let decl = declarations["RendererToolName"].clone();
        let union = decl
            .lines()
            .find(|line| line.starts_with("export type"))
            .expect("the union is declared")
            .trim_start_matches("export ")
            .to_owned();
        let names = generate::tool_names_from_union(&union);

        let rebuilt = format!(
            "type RendererToolName = {};",
            names
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        assert_eq!(
            rebuilt, union,
            "the tool-name parse does not reproduce what ts-rs generated"
        );
        assert_eq!(
            names.iter().collect::<std::collections::HashSet<_>>().len(),
            names.len(),
            "two variants share a wire spelling"
        );
        // Guards against the union collapsing to a single `string`, which would
        // turn the allowlist into a passthrough without failing anything else.
        assert!(names.len() > 1, "the vocabulary did not render as a union");
        assert!(
            names.iter().all(|name| !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')),
            "a tool name is not a plain snake_case identifier: {names:?}"
        );
    }

    /// Rust sources that can contribute a generated type.
    fn generated_sources() -> Vec<(String, String)> {
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for relative in ["src", "../openwave-core/src"] {
            collect_rust_files(&crate_root.join(relative), &mut files);
        }
        assert!(
            files.len() > 20,
            "expected to scan the server and core sources, found {}",
            files.len()
        );
        files
    }

    fn collect_rust_files(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                // This file holds deliberately-wrong examples, in the scanner's
                // own self-test. Scanning it flags those, which is the scanner
                // working rather than a real finding.
                if path.file_name().is_some_and(|name| name == "wire_types.rs") {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push((path.display().to_string(), text));
                }
            }
        }
    }

    /// A field serde omits must be declared optional, not nullable.
    ///
    /// `ts-rs` will not do this for you: it requires `#[serde(default)]` to infer
    /// optionality, while serde treats a missing `Option` as `None` regardless.
    /// So an `Option` field carrying only `skip_serializing_if` generates
    /// `T | null` — a claim that the key is always present, which is the exact
    /// mismatch that shipped twice before any of this was generated.
    #[test]
    fn a_field_serde_omits_is_declared_optional() {
        let mut problems = Vec::new();
        for (path, source) in generated_sources() {
            for field in generate::omittable_fields_missing_optional(&source) {
                problems.push(format!("{path}: {field}"));
            }
        }
        assert!(
            problems.is_empty(),
            "these fields are omitted by serde but not declared optional in \
             TypeScript. Add #[ts(optional)] beside the skip_serializing_if, or \
             the generated type will claim a key the server never sends:\n{}",
            problems.join("\n")
        );
    }

    /// The scanner above only helps if it can actually see the mistake, and a
    /// silently-matching-nothing scanner would pass forever. Feed it both shapes.
    #[test]
    fn the_optional_scanner_detects_a_missing_annotation() {
        let bad = r#"
            #[derive(Serialize, TS)]
            pub struct Snapshot {
                pub id: String,
                #[serde(skip_serializing_if = "Option::is_none")]
                pub title: Option<String>,
            }
        "#;
        assert_eq!(
            generate::omittable_fields_missing_optional(bad),
            vec!["pub title: Option<String>".to_owned()],
            "the scanner missed an unannotated omittable field"
        );

        let annotated = r#"
            #[derive(Serialize, TS)]
            pub struct Snapshot {
                #[serde(skip_serializing_if = "Option::is_none")]
                #[ts(optional)]
                pub title: Option<String>,
            }
        "#;
        assert!(generate::omittable_fields_missing_optional(annotated).is_empty());

        // A type that does not derive TS is not generated, so it is not this
        // test's business — flagging it would be a false failure.
        let not_generated = r#"
            #[derive(Serialize)]
            pub struct StoredOnly {
                #[serde(skip_serializing_if = "Option::is_none")]
                pub title: Option<String>,
            }
        "#;
        assert!(generate::omittable_fields_missing_optional(not_generated).is_empty());
    }

    /// Ids serialize as a bare UUID string, and must generate as one.
    ///
    /// `ts-rs` cannot parse `#[serde(transparent)]` — the attribute every id
    /// carries — and ignores it. It happens to reach the right answer because a
    /// single-field tuple struct already renders as its inner type. That is a
    /// coincidence of two independent rules agreeing, so it is pinned here
    /// rather than trusted, especially now that the warning is silenced.
    #[test]
    fn ids_generate_as_bare_strings() {
        let declarations = event_declarations();
        for id in ["CallId", "TurnId", "MessageId"] {
            let decl = declarations
                .get(id)
                .unwrap_or_else(|| panic!("{id} is reachable from the event root"));
            assert!(
                decl.contains(&format!("export type {id} = string;")),
                "{id} should generate as a bare string, got: {decl}"
            );
        }
    }

    /// The closure walk is the reason only one root is named, so assert it
    /// actually reached past that root. A visitor that silently stopped at the
    /// top level would still produce a file, and the diff check would happily
    /// pin a nearly empty one.
    #[test]
    fn the_walk_reaches_the_types_the_root_only_references() {
        let declarations = event_declarations();
        for expected in [
            "RendererSequencedEvent",
            "RendererAgentEvent",
            "RendererToolName",
            "RendererToolStatus",
            "ToolActionPreview",
            "ToolResultPreview",
            "ToolApprovalKind",
            "ApprovalClass",
        ] {
            assert!(
                declarations.contains_key(expected),
                "{expected} was not reached from the root; generated: {:?}",
                declarations.keys().collect::<Vec<_>>()
            );
        }
    }
}
