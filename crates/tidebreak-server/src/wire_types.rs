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
                let declaration = T::decl(self.cfg)
                    .lines()
                    .map(str::trim_end)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.out
                    .insert(name, format!("{docs}export {declaration}\n"));
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

    /// The guard limits, emitted beside the generated types.
    ///
    /// The renderer bounds every opaque string it draws — ids, timestamps,
    /// cursors — and the CLI bounds the same fields before printing them. The
    /// numbers live in `crate::wire::limits` and are generated here so the two
    /// clients cannot disagree about what a valid payload is.
    pub(crate) fn render_wire_limits() -> String {
        use crate::wire::limits::{
            MAX_WIRE_CURSOR_CHARS, MAX_WIRE_ID_CHARS, MAX_WIRE_TIMESTAMP_CHARS,
        };
        format!(
            "/**\n\
             \x20* Guard limits shared with the server and the CLI, in code points.\n\
             \x20*\n\
             \x20* The decoders bound every opaque string they will draw. These are the\n\
             \x20* ceilings, generated from `tidebreak_server::wire::limits` so every\n\
             \x20* client applies the same numbers.\n\
             \x20*/\n\
             export const MAX_WIRE_ID_CHARS = {MAX_WIRE_ID_CHARS};\n\
             export const MAX_WIRE_TIMESTAMP_CHARS = {MAX_WIRE_TIMESTAMP_CHARS};\n\
             export const MAX_WIRE_CURSOR_CHARS = {MAX_WIRE_CURSOR_CHARS};\n"
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
             // Regenerate: UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server\n\
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
            // Match the last path segment: the derive is written both as `TS`
            // and as `ts_rs::TS`, and matching only the bare name silently
            // skipped every type that used the qualified form — which was most
            // of them.
            if !derives
                .split(',')
                .any(|d| d.trim().rsplit("::").next() == Some("TS"))
            {
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

    /// Render a TypeScript module of real serialized server values.
    ///
    /// The renderer's validators are a trust boundary and stay hand-written, so
    /// generation cannot check them. What it can do is stop them being *tested*
    /// against hand-authored objects: until now every validator test built its
    /// own input from the TypeScript author's belief about the wire, so a field
    /// renamed server-side left both suites green and the app broken. That is
    /// the failure this closes.
    ///
    /// Each entry is serialized from a real Rust value, so the fixture is the
    /// server's actual output rather than a description of it.
    pub(crate) fn render_fixtures(entries: &[(&str, serde_json::Value)]) -> String {
        let body = entries
            .iter()
            .map(|(name, value)| {
                let json = serde_json::to_string_pretty(value).expect("a fixture serializes");
                format!("export const {name} = {json} as const;\n")
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "// Generated from Rust. Do not edit.\n\
             //\n\
             // Regenerate: UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server\n\
             //\n\
             // Real serialized output from the server's own types, for the renderer's\n\
             // validator tests to consume. Hand-authored inputs encode what the author\n\
             // believed the wire looked like, which is how a rename can leave both test\n\
             // suites green and the app broken. These cannot drift from the server\n\
             // without this file changing. See docs/wire-types.md.\n\
             \n\
             {body}"
        )
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
    const BINDINGS: &str = "../tidebreak-desktop/ui/src/generated/wire.ts";

    const REGENERATE: &str = "UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server";

    fn event_declarations() -> BTreeMap<String, String> {
        let cfg = generate::config();
        let mut out = BTreeMap::new();
        // One root. Everything the socket can carry is reachable from it — both
        // frame kinds, and the shared preview and approval types the REST surface
        // uses.
        generate::collect_from::<crate::event_projection::RendererChatFrame>(&cfg, &mut out);
        // The terminal tool card rebuilt from the journal. Shares the tool
        // vocabulary and the action preview with the live stream, which is why
        // it belongs with them rather than with the rest of the REST surface.
        generate::collect_from::<tidebreak_core::ChatToolActivitySnapshot>(&cfg, &mut out);
        // The visible transcript entry. Its role is deliberately narrower than
        // the stored one, and generating it is what keeps the renderer's
        // two-arm branch honest.
        generate::collect_from::<crate::routes::ChatMessageSnapshot>(&cfg, &mut out);
        // The two consent surfaces. Their TypeScript describes the validator's
        // output rather than the wire, so the generated types are what the
        // validators narrow *from* — see the aliases in api.ts.
        generate::collect_from::<crate::routes::PendingApprovalSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::StandingGrantSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::consent::ConsentStatementSnapshot>(&cfg, &mut out);
        // The app-invoke refusal envelope. The invoke payloads themselves are
        // opaque passthrough for the sandboxed frame and stay hand-written;
        // only the refusal is generated, because the renderer branches on its
        // closed kind (`consent_required` opens the grant sheet).
        generate::collect_from::<crate::routes::AppInvokeRefusal>(&cfg, &mut out);
        // The grant-state projection the consent sheet renders: server and
        // tool names with coverage/staleness booleans, never definitions.
        generate::collect_from::<crate::routes::AppGrantState>(&cfg, &mut out);
        // The Apps library projections: names, counts, timestamps, and the
        // grant verdict — never manifests, bundles, or definitions.
        generate::collect_from::<crate::routes::AppLibrary>(&cfg, &mut out);
        generate::collect_from::<crate::routes::AppDetail>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::PendingUserQuestions>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::PendingPlanApproval>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::PlanDecisionChoice>(&cfg, &mut out);
        // The task-plan route's own root: the journal carries only a refresh
        // hint, so the steps reach the renderer from here.
        generate::collect_from::<tidebreak_core::TaskPlan>(&cfg, &mut out);
        generate::collect_from::<crate::routes::client_execution::PendingFolderAccessRequest>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::routes::client_execution::PendingOutputWritebackRequest>(
            &cfg, &mut out,
        );
        // The cross-chat attention read model. It shares the tool vocabulary
        // with the approval card it points at, and nothing else.
        generate::collect_from::<crate::routes::InboxEntrySnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::InboxConversation>(&cfg, &mut out);
        generate::collect_from::<crate::routes::InboxItemSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::NotificationSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::NotificationPage>(&cfg, &mut out);
        generate::collect_from::<crate::routes::NotificationUnreadCount>(&cfg, &mut out);
        generate::collect_from::<crate::routes::MarkNotificationsReadResult>(&cfg, &mut out);
        generate::collect_from::<crate::routes::CompactionRun>(&cfg, &mut out);
        // Configuration, catalog, and project surfaces. These carry no shared
        // types with the conversation path, but one generated module keeps the
        // renderer importing from a single place.
        generate::collect_from::<crate::routes::Settings>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::QueuedTurn>(&cfg, &mut out);
        generate::collect_from::<crate::routes::ModelInfo>(&cfg, &mut out);
        generate::collect_from::<crate::routes::ModelRoleInfo>(&cfg, &mut out);
        // Memory: backend capabilities, records, search, context, revisions,
        // and the request bodies the manager UI sends.
        generate::collect_from::<tidebreak_core::MemoryCaps>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::MemoryRecord>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::MemorySearchHit>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::MemoryDigest>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::MemoryRevision>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::MemoryIngestReceipt>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::MemorySweepStatus>(&cfg, &mut out);
        generate::collect_from::<crate::routes::CreateMemoryRecordBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::UpdateMemoryRecordBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::MemoryStatusBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::MemoryIngestBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::ChatTranscript>(&cfg, &mut out);
        generate::collect_from::<crate::providers::ProviderInfo>(&cfg, &mut out);
        generate::collect_from::<crate::providers::ProviderAuthMode>(&cfg, &mut out);
        generate::collect_from::<crate::chatgpt_runtime::ChatGptSignInStatus>(&cfg, &mut out);
        generate::collect_from::<crate::web_search::WebSearchConfigInfo>(&cfg, &mut out);
        generate::collect_from::<crate::web_search::WebSearchCredentialReadiness>(&cfg, &mut out);
        generate::collect_from::<crate::code_execution::ExecConfigInfo>(&cfg, &mut out);
        generate::collect_from::<crate::code_execution::ExecCredentialReadiness>(&cfg, &mut out);
        generate::collect_from::<crate::mcp_config::McpServersInfo>(&cfg, &mut out);
        // The installed plugin/skill catalog, its host-derived capability
        // badges, and the toggle body the management surface sends back.
        generate::collect_from::<crate::routes::PluginCatalog>(&cfg, &mut out);
        generate::collect_from::<crate::routes::PluginEnableUpdate>(&cfg, &mut out);
        // Its own endpoint root: a skill's instruction body is fetched on
        // demand rather than carried in the catalog.
        generate::collect_from::<crate::routes::SkillInstructions>(&cfg, &mut out);
        // Likewise for a reusable prompt's insertable text.
        generate::collect_from::<crate::routes::PromptBody>(&cfg, &mut out);
        // The Connected apps settings listing: per-kind health/catalog
        // projections and credential *status* — never definitions or values.
        generate::collect_from::<crate::routes::ConnectedAppsInfo>(&cfg, &mut out);
        // The spec-preview response: what a document declares, for the REST
        // form's operation picker.
        generate::collect_from::<crate::routes::SpecPreviewInfo>(&cfg, &mut out);
        generate::collect_from::<crate::openapi_discovery::SpecDiscoveryInfo>(&cfg, &mut out);
        // Named separately because `serde(flatten)` inlines it into
        // `McpServerInfo` rather than referencing it, so the walk never reaches
        // it — and the renderer uses it on its own as the PUT body shape.
        generate::collect_from::<crate::mcp_config::McpServerDefinition>(&cfg, &mut out);
        generate::collect_from::<crate::routes::McpViewSession>(&cfg, &mut out);
        generate::collect_from::<crate::routes::AppViewSession>(&cfg, &mut out);
        generate::collect_from::<crate::gateway_runtime::GatewayStatus>(&cfg, &mut out);
        generate::collect_from::<crate::managed_policy::ManagedPolicy>(&cfg, &mut out);
        generate::collect_from::<crate::gateway_runtime::GatewayApps>(&cfg, &mut out);
        // Where a gateway-bound app lives at its gateway: the closed outcome
        // the renderer branches on, and the address it opens when there is one.
        generate::collect_from::<crate::routes::AppGatewayPageResult>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::Project>(&cfg, &mut out);
        generate::collect_from::<tidebreak_core::Chat>(&cfg, &mut out);
        generate::collect_from::<crate::routes::AgentRunSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::AgentRunCancellationSnapshot>(&cfg, &mut out);
        // A separate endpoint root: the ordered activity history is returned by
        // its own route, so the snapshot walk never reaches it.
        generate::collect_from::<crate::routes::AgentActivityHistoryItem>(&cfg, &mut out);
        // Its own endpoint root too: the snapshot carries a run's plan progress,
        // while the full ordered list is fetched per run.
        generate::collect_from::<tidebreak_core::AgentRunTaskPlan>(&cfg, &mut out);
        // Likewise its own endpoint root: the live progress stream is paged by
        // its own route rather than embedded in a snapshot.
        generate::collect_from::<crate::routes::AgentRunProgressPage>(&cfg, &mut out);
        generate::collect_from::<crate::workspace_config::WorkspaceConfigDocument>(&cfg, &mut out);
        generate::collect_from::<crate::workspace_config::WorkspaceConfigPreview>(&cfg, &mut out);
        generate::collect_from::<crate::workspace_config::WorkspaceConfigApplyRequest>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::workspace_config::WorkspaceConfigApplyResult>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::routes::code::CodeRepoSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspaceSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeConnectPage>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeGrantSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeSessionSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeTurnSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeAnalyticsSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::QueuedCodeTurn>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::SequencedCodeEventFrame>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::HarnessDoctorReport>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::HarnessModelList>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspaceFiles>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspaceTree>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspaceSearch>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspaceBlob>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspaceDiff>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeTerminalSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeTerminalRead>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeTerminalActivityNotice>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeSessionDigest>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeUpdateNotice>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeApprovalSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeApprovalDecisionBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeCommitSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodePushSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspacePrSnapshot>(&cfg, &mut out);
        // Its own endpoint root: the PR conversation is fetched per request
        // rather than embedded in the digest.
        generate::collect_from::<crate::routes::code::CodePrCommentsSnapshot>(&cfg, &mut out);
        // The merge request body, so the renderer sends exactly the accepted
        // method vocabulary.
        generate::collect_from::<crate::routes::code::MergeCodePrBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryRepositoriesSnapshot>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::routes::code::ResolveCodeDeliveryRepositoriesBody>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::routes::code::CodeDeliveryPullRequestQuery>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryPullRequestsPage>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeWorkspacePullRequests>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryPullRequestTarget>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::routes::code::CodeDeliveryPullRequestFile>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryPullRequestDetail>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::routes::code::CodeDeliveryPullRequestActionBody>(
            &cfg, &mut out,
        );
        generate::collect_from::<crate::routes::code::CodeDeliveryRunQuery>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryRunsPage>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryRunTarget>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryRunDetail>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryRunActionBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeDeliveryActionResult>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeActionSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeCloneJobSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeHarnessInstallSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeCloneDefaults>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeRepoSources>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeGithubRepositories>(&cfg, &mut out);
        // Where new worktrees land, and the body that moves it.
        generate::collect_from::<crate::routes::code::CodeWorktreeRoot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeForkTranscript>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CodeForkBody>(&cfg, &mut out);
        // Failing CI job logs the fix-errors action downloads before it prompts.
        generate::collect_from::<crate::routes::code::CodeCheckLogsSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::SetCodeWorktreeRootBody>(&cfg, &mut out);
        // Triggers: the rules the sweep reads, and the bodies that arm them.
        generate::collect_from::<crate::routes::code::CodeTriggerSnapshot>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::CreateCodeTriggerBody>(&cfg, &mut out);
        generate::collect_from::<crate::routes::code::UpdateCodeTriggerBody>(&cfg, &mut out);
        out
    }

    fn rendered_bindings() -> String {
        let declarations = event_declarations();
        let union = declarations
            .get("RendererToolName")
            .expect("the vocabulary is reachable from the event root");
        let names = generate::tool_names_from_union(union);
        generate::render(
            &declarations,
            &[
                generate::render_tool_name_list(&names),
                generate::render_wire_limits(),
            ],
        )
    }

    /// The WebSocket frame types, which until now had no contract at all: the
    /// renderer parses each frame with a bare cast and no runtime validation, so
    /// the generated type is the only thing describing that payload.
    #[test]
    fn the_generated_bindings_are_current() {
        generate::check_or_update(BINDINGS, &rendered_bindings(), REGENERATE);
    }

    #[test]
    fn the_generated_bindings_have_no_trailing_whitespace() {
        let generated = rendered_bindings();
        assert!(
            generated.lines().all(|line| line == line.trim_end()),
            "generated wire types contain trailing whitespace"
        );
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
        for relative in ["src", "../tidebreak-core/src"] {
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

    /// Path of the generated validator fixtures, relative to this crate.
    const FIXTURES: &str = "../tidebreak-desktop/ui/src/generated/fixtures.ts";

    /// Fixed ids, so the fixture does not change on every run.
    fn id(n: u128) -> uuid::Uuid {
        uuid::Uuid::from_u128(n)
    }

    /// Real server values for the shapes the renderer validates by hand.
    ///
    /// These are the five camelCase types whose TypeScript describes the
    /// *validator's output* rather than the wire, so their remaps cannot be
    /// generated away. Serializing the real value is what ties them to the
    /// server: a field renamed here changes this file, and the renderer test
    /// that consumes it fails.
    fn validator_fixtures() -> Vec<(&'static str, serde_json::Value)> {
        let approval = crate::routes::PendingApprovalSnapshot {
            auto_judge_status: None,
            call_id: tidebreak_core::CallId(id(1)),
            turn_id: tidebreak_core::TurnId(id(2)),
            action: tidebreak_core::RendererToolName::Exec,
            approval: tidebreak_core::ToolApprovalKind::ExecMayRunNetworkedCommand,
            class: tidebreak_core::ApprovalClass::Sensitive,
            preview: Some(tidebreak_core::ToolActionPreview::Exec {
                command: "git".into(),
                args: vec!["status".into()],
                cwd: ".".into(),
                files: vec!["documents/report.pdf".into()],
                summary: Some("Checking the repository status".into()),
            }),
            can_approve: true,
            can_remember: true,
            grant_rungs: vec![
                crate::routes::ApprovalGrantRung::ExactAction,
                crate::routes::ApprovalGrantRung::CommandPrefix { tokens: 1 },
                crate::routes::ApprovalGrantRung::WholeTool,
            ],
        };
        // The same shape with the omittable field absent, because "the key is
        // missing" is a distinct case the validator has to survive and the one
        // that hand-authored inputs habitually get wrong.
        let approval_without_preview = crate::routes::PendingApprovalSnapshot {
            auto_judge_status: None,
            call_id: tidebreak_core::CallId(id(5)),
            turn_id: tidebreak_core::TurnId(id(2)),
            action: tidebreak_core::RendererToolName::AskUserQuestions,
            approval: tidebreak_core::ToolApprovalKind::Unsupported,
            class: tidebreak_core::ApprovalClass::ReadOnly,
            preview: None,
            can_approve: false,
            can_remember: false,
            grant_rungs: Vec::new(),
        };

        let folder_access = crate::routes::client_execution::PendingFolderAccessRequest {
            call_id: tidebreak_core::CallId(id(3)),
            turn_id: tidebreak_core::TurnId(id(4)),
            reason: crate::routes::client_execution::RENDERER_FOLDER_ACCESS_REASON.to_owned(),
            folder_hint: Some(tidebreak_core::RequestedFolderHint::Documents),
            claimed: true,
        };
        let output_writeback = crate::routes::client_execution::PendingOutputWritebackRequest {
            call_id: tidebreak_core::CallId(id(6)),
            turn_id: tidebreak_core::TurnId(id(7)),
            mode: tidebreak_core::OutputWriteMode::Replace,
            claimed: false,
        };

        vec![
            (
                "PENDING_APPROVAL",
                serde_json::to_value(&approval).expect("an approval serializes"),
            ),
            (
                "PENDING_APPROVAL_WITHOUT_PREVIEW",
                serde_json::to_value(&approval_without_preview).expect("an approval serializes"),
            ),
            (
                "PENDING_FOLDER_ACCESS_REQUEST",
                serde_json::to_value(&folder_access).expect("a folder request serializes"),
            ),
            (
                "PENDING_OUTPUT_WRITEBACK_REQUEST",
                serde_json::to_value(&output_writeback)
                    .expect("an output write-back request serializes"),
            ),
        ]
    }

    /// The renderer's validators are tested against these exact bytes.
    ///
    /// Regenerate with `UPDATE_WIRE_TYPES=1 cargo test -p tidebreak-server`. A diff
    /// here means the wire changed under a hand-written validator, which is the
    /// one thing generation cannot check for us.
    #[test]
    fn the_validator_fixtures_are_current() {
        let rendered = generate::render_fixtures(&validator_fixtures());
        generate::check_or_update(FIXTURES, &rendered, REGENERATE);
    }

    /// A fixture that silently lost its interesting field would still pass the
    /// diff check, so assert the two properties the renderer tests depend on.
    #[test]
    fn the_fixtures_cover_the_omitted_key_case() {
        let fixtures = validator_fixtures();
        let with = &fixtures
            .iter()
            .find(|(name, _)| *name == "PENDING_APPROVAL")
            .expect("the approval fixture exists")
            .1;
        let without = &fixtures
            .iter()
            .find(|(name, _)| *name == "PENDING_APPROVAL_WITHOUT_PREVIEW")
            .expect("the no-preview approval fixture exists")
            .1;
        assert!(
            with.get("preview").is_some(),
            "the approval fixture should carry a preview"
        );
        assert!(
            without.get("preview").is_none(),
            "serde omits an absent preview, so the fixture must not contain the key"
        );
    }

    /// Shapes that must never appear in generated output.
    ///
    /// Each one is a whole class of mistake rather than a named case, which is
    /// the point: the named hazards were found by reading the code, and reading
    /// does not scale. These fail on the *next* one.
    ///
    /// This is a backstop, not the primary defence. `serde_json::Value` cannot
    /// reach a generated type at all today, because `serde-json-impl` is
    /// deliberately off and so `Value` does not implement `TS` — a field holding
    /// one is a compile error, which is stronger than any assertion here. This
    /// test is what catches it if that feature is ever switched on.
    #[test]
    fn the_generated_types_contain_no_imprecise_shapes() {
        let generated = rendered_bindings();
        for (shape, why) in [
            (
                ": any",
                "a Rust type reached the roots that ts-rs cannot express — most \
                 likely serde_json::Value, which means the serde-json-impl \
                 feature was enabled. Restrict the roots or give the field a \
                 concrete type; `any` disables checking for every consumer",
            ),
            (
                "bigint",
                "an integer rendered as bigint. These types describe what \
                 JSON.parse produces, which is a number, so a bigint \
                 declaration is false about every value received. The generator \
                 sets large_int to number — check it is being used",
            ),
            (
                ": unknown",
                "a field rendered as unknown, which tells a consumer nothing \
                 and forces a cast at every use",
            ),
            ("Record<string, any>", "an untyped map reached the output"),
        ] {
            assert!(
                !generated.contains(shape),
                "generated wire types contain `{shape}`: {why}"
            );
        }
    }

    /// Fields whose precision the renderer depends on, and which would silently
    /// widen to `string` if their Rust type were loosened.
    ///
    /// A short, purposeful list rather than a copy of any vocabulary: each entry
    /// is a field where a compile-time guarantee elsewhere is keyed on the union.
    /// `ChatToolActivitySnapshot.tool` was `&'static str` and generated as
    /// `string`, which compiled on both sides while deleting the totality check
    /// that makes a tool without an icon a compile error.
    ///
    /// Scoped per declaration, not per field name. A global check read
    /// `McpServerDefinition.name: string` as the event stream's `name:
    /// RendererToolName` having been widened — a false alarm on a field that is
    /// legitimately a string, and unrelated.
    ///
    /// Also a backstop rather than the only defence: now that those fields hold
    /// enums, loosening one back to a string breaks the code that assigns to it.
    /// This catches the case where a change is consistent enough to compile —
    /// a new field, or a type swapped along with its call sites.
    #[test]
    fn precision_critical_fields_generate_as_unions() {
        let declarations = event_declarations();
        for (declaration, field, expected) in [
            ("RendererAgentEvent", "name", "RendererToolName"),
            ("RendererAgentEvent", "action", "RendererToolName"),
            ("RendererAgentEvent", "approval", "ToolApprovalKind"),
            ("RendererAgentEvent", "class", "ApprovalClass"),
            ("RendererAgentEvent", "status", "RendererToolStatus"),
            ("ChatToolActivitySnapshot", "tool", "RendererToolName"),
            (
                "ChatToolActivitySnapshot",
                "status",
                "ChatToolActivityStatus",
            ),
            ("PendingApprovalSnapshot", "action", "RendererToolName"),
            ("PendingApprovalSnapshot", "approval", "ToolApprovalKind"),
            ("PendingApprovalSnapshot", "class", "ApprovalClass"),
            // Stored Role has four variants; TranscriptRole is the visible
            // subset plus the synthetic compaction divider. A widened union
            // renders a system message as a user bubble, and the branch that
            // reads it still compiles.
            ("ChatMessageSnapshot", "role", "TranscriptRole"),
            ("ModelInfo", "input_modalities", "Array<InputModality>"),
        ] {
            let decl = declarations.get(declaration).unwrap_or_else(|| {
                panic!("{declaration} is not generated; the precision list is stale")
            });
            assert!(
                decl.contains(&format!("{field}: {expected}")),
                "expected `{field}: {expected}` in {declaration}. If the Rust \
                 field was loosened to a String it now generates as `string`, \
                 which compiles everywhere and silently drops the allowlist the \
                 renderer's tables are keyed on.\n{decl}"
            );
            assert!(
                !decl.contains(&format!("{field}: string")),
                "{declaration}.{field} generates as a bare string, losing the \
                 {expected} union the renderer depends on"
            );
        }
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

    /// Path of the shared chat-frame fixtures, relative to this crate.
    ///
    /// JSON rather than TypeScript because three decoders read it: the
    /// renderer's tests, the CLI's tests, and this crate's own round trip.
    const CHAT_FRAMES: &str = "fixtures/chat-frames.json";

    /// One real frame of every kind the socket can carry.
    ///
    /// Serialized from the server's own types, so a client test that decodes
    /// every entry proves its decoder accepts what the server sends today — and
    /// [`the_chat_frame_fixtures_cover_every_event`] proves the list cannot
    /// silently fall behind the event union.
    fn chat_frame_fixtures() -> Vec<(&'static str, serde_json::Value)> {
        use crate::event_projection::{
            RendererAgentEvent, RendererChatFrame, RendererChatMetadata, RendererModelIdentity,
            RendererRefusal, RendererSequencedEvent, RendererToolFailure, RendererToolFailureCode,
            RendererToolFailureReason, RendererToolStatus, RendererTurnUsage, TurnFailureCategory,
        };
        use tidebreak_core::{
            ApprovalClass, CallId, MessageId, RendererToolName, ToolActionPreview,
            ToolApprovalKind, ToolResultPreview, TurnId,
        };

        let turn = TurnId(id(0x10));
        let call = CallId(id(0x11));
        let usage = RendererTurnUsage {
            input_tokens: 120,
            output_tokens: 34,
            cache_read_input_tokens: 800,
            cache_creation_input_tokens: 0,
        };
        let exec_preview = ToolActionPreview::Exec {
            command: "git".into(),
            args: vec!["status".into()],
            cwd: ".".into(),
            files: Vec::new(),
            summary: Some("Checking the repository status".into()),
        };
        let event = |seq: i64, event: RendererAgentEvent| {
            RendererChatFrame::Event(Box::new(RendererSequencedEvent {
                seq,
                event,
                replayed: None,
            }))
        };
        let frames: Vec<(&'static str, RendererChatFrame)> = vec![
            (
                "turn_started",
                event(1, RendererAgentEvent::TurnStarted { turn_id: turn }),
            ),
            (
                "text_delta",
                event(
                    2,
                    RendererAgentEvent::TextDelta {
                        text: "Hello".into(),
                    },
                ),
            ),
            (
                "reasoning_delta",
                event(
                    3,
                    RendererAgentEvent::ReasoningDelta {
                        text: "Considering the request".into(),
                    },
                ),
            ),
            (
                "stream_interrupted",
                event(4, RendererAgentEvent::StreamInterrupted),
            ),
            (
                "tool_call_started",
                event(
                    5,
                    RendererAgentEvent::ToolCallStarted {
                        call_id: call,
                        name: RendererToolName::Exec,
                    },
                ),
            ),
            (
                "tool_call_args_delta",
                event(6, RendererAgentEvent::ToolCallArgsDelta { call_id: call }),
            ),
            (
                "user_questions_asked",
                event(
                    7,
                    RendererAgentEvent::UserQuestionsAsked {
                        call_id: call,
                        turn_id: turn,
                    },
                ),
            ),
            (
                "plan_proposed",
                event(
                    8,
                    RendererAgentEvent::PlanProposed {
                        call_id: call,
                        turn_id: turn,
                    },
                ),
            ),
            (
                "task_plan_updated",
                event(
                    9,
                    RendererAgentEvent::TaskPlanUpdated {
                        call_id: call,
                        turn_id: turn,
                    },
                ),
            ),
            (
                "approval_required",
                event(
                    10,
                    RendererAgentEvent::ApprovalRequired {
                        call_id: call,
                        action: RendererToolName::Exec,
                        approval: ToolApprovalKind::ExecMayRunNetworkedCommand,
                        class: ApprovalClass::Sensitive,
                        auto_judging: false,
                        grant_rungs: vec![
                            crate::routes::ApprovalGrantRung::ExactAction,
                            crate::routes::ApprovalGrantRung::CommandPrefix { tokens: 1 },
                            crate::routes::ApprovalGrantRung::WholeTool,
                        ],
                        preview: Some(exec_preview.clone()),
                    },
                ),
            ),
            (
                "approval_required_without_preview",
                event(
                    11,
                    RendererAgentEvent::ApprovalRequired {
                        call_id: call,
                        action: RendererToolName::Other,
                        approval: ToolApprovalKind::Unsupported,
                        class: ApprovalClass::Sensitive,
                        auto_judging: true,
                        grant_rungs: Vec::new(),
                        preview: None,
                    },
                ),
            ),
            (
                "approval_decided",
                event(
                    12,
                    RendererAgentEvent::ApprovalDecided {
                        call_id: call,
                        approved: true,
                    },
                ),
            ),
            (
                "tool_call_completed",
                event(
                    13,
                    RendererAgentEvent::ToolCallCompleted {
                        call_id: call,
                        status: RendererToolStatus::Completed,
                        failure: None,
                        action: Some(exec_preview),
                        result: Some(ToolResultPreview::Exec {
                            exit_code: Some(0),
                            timed_out: false,
                            output_truncated: false,
                            stdout: "ok\n".into(),
                            stderr: String::new(),
                            images: Vec::new(),
                            outputs: Vec::new(),
                            degraded: None,
                            backend: None,
                        }),
                    },
                ),
            ),
            (
                "tool_call_completed_with_failure",
                event(
                    14,
                    RendererAgentEvent::ToolCallCompleted {
                        call_id: call,
                        status: RendererToolStatus::Failed,
                        failure: Some(RendererToolFailure {
                            code: RendererToolFailureCode::ExecutorUnavailable,
                            reason: RendererToolFailureReason::LeaseExpired,
                        }),
                        action: None,
                        result: None,
                    },
                ),
            ),
            (
                "turn_completed",
                event(15, RendererAgentEvent::TurnCompleted { usage }),
            ),
            (
                "turn_refused",
                event(
                    16,
                    RendererAgentEvent::TurnRefused {
                        refusal: RendererRefusal {
                            category: Some("safety".into()),
                            partial_output: true,
                        },
                        usage,
                    },
                ),
            ),
            (
                "turn_failed",
                event(
                    17,
                    RendererAgentEvent::TurnFailed {
                        category: TurnFailureCategory::RateLimited,
                        detail: Some("rate limited; retry after 30s".into()),
                        model: Some(RendererModelIdentity {
                            id: "claude-opus-4-8".into(),
                            provider: crate::providers::ProviderKind::Anthropic,
                        }),
                    },
                ),
            ),
            (
                "turn_failed_without_detail",
                event(
                    18,
                    RendererAgentEvent::TurnFailed {
                        category: TurnFailureCategory::Unknown,
                        detail: None,
                        model: None,
                    },
                ),
            ),
            (
                "turn_cancelled",
                event(19, RendererAgentEvent::TurnCancelled { usage }),
            ),
            (
                "user_steered",
                event(
                    20,
                    RendererAgentEvent::UserSteered {
                        message_id: MessageId(id(0x12)),
                        text: "Focus on the tests".into(),
                    },
                ),
            ),
            (
                "context_truncated",
                event(
                    21,
                    RendererAgentEvent::ContextTruncated {
                        original_tokens: 180_000,
                        fitted_tokens: 120_000,
                    },
                ),
            ),
            (
                "compaction_started",
                event(22, RendererAgentEvent::CompactionStarted),
            ),
            (
                "compaction_finished",
                event(
                    23,
                    RendererAgentEvent::CompactionFinished { compacted: true },
                ),
            ),
            ("event_omitted", event(24, RendererAgentEvent::EventOmitted)),
            (
                "replayed_event",
                RendererChatFrame::Event(Box::new(RendererSequencedEvent {
                    seq: 25,
                    event: RendererAgentEvent::TextDelta {
                        text: "from catch-up".into(),
                    },
                    replayed: Some(true),
                })),
            ),
            (
                "metadata_titled",
                RendererChatFrame::Metadata(RendererChatMetadata::Titled {
                    title: "A chat".into(),
                }),
            ),
            (
                "metadata_file_changes_recorded",
                RendererChatFrame::Metadata(RendererChatMetadata::FileChangesRecorded {
                    turn_id: turn,
                }),
            ),
            (
                "metadata_memory_proposals_recorded",
                RendererChatFrame::Metadata(RendererChatMetadata::MemoryProposalsRecorded {
                    turn_id: turn,
                }),
            ),
            (
                "metadata_sandbox_preparing",
                RendererChatFrame::Metadata(RendererChatMetadata::SandboxPreparing {
                    preparing: true,
                }),
            ),
        ];
        frames
            .into_iter()
            .map(|(name, frame)| {
                (
                    name,
                    serde_json::to_value(&frame).expect("a chat frame serializes"),
                )
            })
            .collect()
    }

    /// The checked-in fixture file: a JSON array of `{ "name", "frame" }`.
    fn rendered_chat_frames() -> String {
        let entries = chat_frame_fixtures()
            .into_iter()
            .map(|(name, frame)| serde_json::json!({ "name": name, "frame": frame }))
            .collect::<Vec<_>>();
        let mut rendered =
            serde_json::to_string_pretty(&entries).expect("the fixture list serializes");
        rendered.push('\n');
        rendered
    }

    /// Three decoders read these bytes — the renderer's, the CLI's, and this
    /// crate's own. A diff here means the socket's shape changed, and every
    /// client test that consumes the file re-runs against the new shape.
    #[test]
    fn the_chat_frame_fixtures_are_current() {
        generate::check_or_update(CHAT_FRAMES, &rendered_chat_frames(), REGENERATE);
    }

    /// Every event variant the generated union declares has a fixture, read
    /// from the generated declaration rather than a hand-kept list so a new
    /// variant fails here until it has one.
    #[test]
    fn the_chat_frame_fixtures_cover_every_event() {
        let declarations = event_declarations();
        let union = &declarations["RendererAgentEvent"];
        let declared: std::collections::BTreeSet<String> = union
            .split("\"type\": \"")
            .skip(1)
            .map(|rest| rest.split('"').next().expect("a closed tag").to_owned())
            .collect();
        assert!(
            declared.len() > 10,
            "the union parse found too few tags: {declared:?}"
        );
        let covered: std::collections::BTreeSet<String> = chat_frame_fixtures()
            .iter()
            .filter_map(|(_, frame)| frame.get("event"))
            .filter_map(|event| event.get("type"))
            .filter_map(|tag| tag.as_str())
            .map(str::to_owned)
            .collect();
        let missing: Vec<_> = declared.difference(&covered).collect();
        assert!(
            missing.is_empty(),
            "event types without a chat-frame fixture: {missing:?}"
        );
        let metadata: std::collections::BTreeSet<String> = chat_frame_fixtures()
            .iter()
            .filter_map(|(_, frame)| frame.get("metadata"))
            .filter_map(|tag| tag.as_str())
            .map(str::to_owned)
            .collect();
        let declared_metadata: std::collections::BTreeSet<String> = declarations
            ["RendererChatMetadata"]
            .split("\"metadata\": \"")
            .skip(1)
            .map(|rest| rest.split('"').next().expect("a closed tag").to_owned())
            .collect();
        let missing: Vec<_> = declared_metadata.difference(&metadata).collect();
        assert!(
            missing.is_empty(),
            "metadata kinds without a chat-frame fixture: {missing:?}"
        );
    }

    /// The server's own round trip: every fixture decodes through the public
    /// wire types and serializes back to the same bytes, so a field that only
    /// serializes, or only deserializes, shows up here rather than in a client.
    #[test]
    fn every_chat_frame_fixture_round_trips() {
        for (name, frame) in chat_frame_fixtures() {
            let decoded: crate::wire::RendererChatFrame = serde_json::from_value(frame.clone())
                .unwrap_or_else(|error| panic!("fixture {name} does not decode: {error}"));
            let again = serde_json::to_value(&decoded).expect("a decoded frame serializes");
            assert_eq!(again, frame, "fixture {name} changed across the round trip");
        }
    }

    // ------------------------------------------------------------------
    // REST records (brightwave-inc/tidebreak#3005): the response types the
    // CLI reads over HTTP, fixtured the same way as the chat frames.
    // ------------------------------------------------------------------

    /// Path of the shared REST record fixtures, relative to this crate.
    const REST_RECORDS: &str = "fixtures/rest-records.json";

    /// The record types with a fixture, spelled as the fixture file tags them.
    ///
    /// A client decodes each entry through the type its tag names, so the
    /// list is the contract: a type added to [`crate::wire`] without a tag
    /// here has no fixture, and [`rest_record_fixtures_cover_every_type`]
    /// fails until it gets one.
    const REST_RECORD_TYPES: &[&str] = &[
        "ModelCatalog",
        "ProvidersList",
        "McpServersInfo",
        "AgentRunSnapshot",
        "DeliverablesCatalog",
        "DeliverablePreview",
        "OutputRevisionsCatalog",
    ];

    fn at(seconds: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(seconds, 0).expect("a fixed timestamp")
    }

    /// One real value per REST record the CLI reads, plus the shapes inside
    /// them that a hand-authored mirror habitually got wrong: an omitted
    /// optional key, a flattened definition, a typed timestamp, and every
    /// producer of an output revision.
    ///
    /// Each entry is `(name, type tag, value)`; the type tag names the
    /// [`crate::wire`] type the entry decodes through.
    fn rest_record_fixtures() -> Vec<(&'static str, &'static str, serde_json::Value)> {
        use crate::wire::{
            AgentActivityKind, AgentActivitySnapshot, AgentActivityStatus, AgentRunSnapshot,
            AgentRunTaskPlanProgress, AgentRunUsageSnapshot, CustomModelConfig, DeliverablePreview,
            DeliverableSummary, DeliverablesCatalog, ExecProviderSnapshot, InputModality,
            McpCuration, McpHealth, McpServerDefinition, McpServerInfo, McpServersInfo,
            ModelCatalog, ModelInfo, ModelRole, ModelRoleInfo, OutputRevisionInfo,
            OutputRevisionProducer, OutputRevisionSource, OutputRevisionsCatalog, ProviderAuthMode,
            ProviderInfo, ProviderKind, ProvidersList, SubmittedOutputSnapshot, VerificationTier,
        };
        use tidebreak_core::{
            AgentRunExecutionLocation, AgentRunId, AgentRunStatus, AgentRunTier,
            AssistantCitationId, CallId, CitationLocator, DocumentId, OutputId, OutputRevisionId,
            ReasoningEffort,
        };

        let catalog = ModelCatalog {
            models: vec![
                ModelInfo {
                    key: "anthropic::claude-opus-5".into(),
                    id: "claude-opus-5".into(),
                    display_name: "Claude Opus 5".into(),
                    provider: ProviderKind::Anthropic,
                    vendor: None,
                    verification: VerificationTier::Verified,
                    recommended: true,
                    available: true,
                    context_window: 1_000_000,
                    max_output_tokens: 64_000,
                    input_modalities: vec![InputModality::Text, InputModality::Image],
                    supports_reasoning: true,
                    supports_tools: true,
                    supports_structured_output: true,
                    reasoning_efforts: vec![
                        ReasoningEffort::Low,
                        ReasoningEffort::Medium,
                        ReasoningEffort::High,
                    ],
                    multimodal: true,
                },
                ModelInfo {
                    key: "model_gateway::claude-opus-5".into(),
                    id: "claude-opus-5".into(),
                    display_name: "Claude Opus 5".into(),
                    provider: ProviderKind::ModelGateway,
                    vendor: Some(ProviderKind::Anthropic),
                    verification: VerificationTier::Unverified,
                    recommended: false,
                    available: false,
                    context_window: 200_000,
                    max_output_tokens: 32_000,
                    input_modalities: vec![InputModality::Text],
                    supports_reasoning: false,
                    supports_tools: true,
                    supports_structured_output: false,
                    reasoning_efforts: Vec::new(),
                    multimodal: false,
                },
            ],
            roles: vec![
                ModelRoleInfo {
                    role: ModelRole::Chat,
                    selection: Some("anthropic::claude-opus-5".into()),
                    resolved_key: Some("anthropic::claude-opus-5".into()),
                },
                ModelRoleInfo {
                    role: ModelRole::Utility,
                    selection: None,
                    resolved_key: None,
                },
            ],
        };

        let providers = ProvidersList {
            providers: vec![
                ProviderInfo {
                    kind: ProviderKind::Anthropic,
                    enabled: true,
                    base_url: None,
                    has_credential: true,
                    auth_mode: Some(ProviderAuthMode::ApiKey),
                    models: Vec::new(),
                },
                ProviderInfo {
                    kind: ProviderKind::OpenaiCompatible,
                    enabled: false,
                    base_url: Some("http://localhost:11434/v1".into()),
                    has_credential: false,
                    auth_mode: None,
                    models: vec![CustomModelConfig {
                        id: "llama".into(),
                        display_name: Some("Llama".into()),
                        ..CustomModelConfig::default()
                    }],
                },
            ],
        };

        let mut stdio = McpServerDefinition {
            name: "filesystem".into(),
            command: Some("npx".into()),
            args: vec![
                "-y".into(),
                "@modelcontextprotocol/server-filesystem".into(),
            ],
            env: ["FS_ROOT".to_owned()].into_iter().collect(),
            env_values: Default::default(),
            env_from: vec!["HOME".into()],
            cwd: Some("/tmp".into()),
            url: None,
            bearer_token_env: None,
            gateway_endpoint: None,
            request_timeout_ms: 30_000,
            enabled: true,
            plugin: None,
            launch: None,
        };
        // The value never serializes; setting one proves the fixture does not
        // carry it.
        stdio
            .env_values
            .insert("FS_ROOT".into(), "never-on-the-wire".into());
        let gateway = McpServerDefinition {
            name: "linear".into(),
            command: None,
            args: Vec::new(),
            env: Default::default(),
            env_values: Default::default(),
            env_from: Vec::new(),
            cwd: None,
            url: None,
            bearer_token_env: None,
            gateway_endpoint: Some("linear".into()),
            request_timeout_ms: 30_000,
            enabled: false,
            plugin: Some("linear-plugin".into()),
            launch: None,
        };
        let mcp = McpServersInfo {
            servers: vec![
                McpServerInfo {
                    definition: stdio,
                    health: McpHealth::Healthy,
                    tool_count: 11,
                    diagnostic: None,
                    resolved_command: Some("/usr/local/bin/npx".into()),
                    curated: Some(McpCuration {
                        display_name: "Filesystem".into(),
                        tested_on: "2026-08-01".into(),
                        notes: "Read and write under one root.".into(),
                    }),
                },
                McpServerInfo {
                    definition: gateway,
                    health: McpHealth::Disabled,
                    tool_count: 0,
                    diagnostic: Some("turned off".into()),
                    resolved_command: None,
                    curated: None,
                },
            ],
        };

        let running = AgentRunSnapshot {
            id: AgentRunId(id(0x30)),
            parent_id: None,
            tier: AgentRunTier::Background,
            execution_location: AgentRunExecutionLocation::Container,
            code_execution_provider: ExecProviderSnapshot::Docker,
            status: AgentRunStatus::Running,
            model_steps: 3,
            usage: AgentRunUsageSnapshot {
                input_tokens: 1200,
                output_tokens: 340,
                cache_read_input_tokens: 800,
                cache_creation_input_tokens: 0,
            },
            task: Some("Summarize the quarterly report".into()),
            started_at: Some(at(1_756_700_000)),
            finished_at: None,
            last_error_code: None,
            activity: Some(AgentActivitySnapshot {
                kind: AgentActivityKind::Exec,
                status: AgentActivityStatus::Running,
            }),
            submitted_outputs: Vec::new(),
            task_plan: Some(AgentRunTaskPlanProgress {
                completed: 1,
                total: 3,
                current: Some("Read the report".into()),
                updated_at: at(1_756_700_030),
            }),
            terminal_text: None,
            created_at: at(1_756_699_990),
            updated_at: at(1_756_700_030),
            spawn_call_id: Some(CallId(id(0x31))),
        };
        // The settled shape, with the omittable plan absent.
        let completed = AgentRunSnapshot {
            id: AgentRunId(id(0x32)),
            parent_id: Some(AgentRunId(id(0x30))),
            tier: AgentRunTier::Foreground,
            execution_location: AgentRunExecutionLocation::InProcess,
            code_execution_provider: ExecProviderSnapshot::Off,
            status: AgentRunStatus::Completed,
            model_steps: 7,
            usage: AgentRunUsageSnapshot::default(),
            task: None,
            started_at: Some(at(1_756_700_100)),
            finished_at: Some(at(1_756_700_200)),
            last_error_code: Some("provider_rate_limited".into()),
            activity: None,
            submitted_outputs: vec![SubmittedOutputSnapshot {
                output_id: OutputId(id(0x40)),
                filename: "summary.md".into(),
            }],
            task_plan: None,
            terminal_text: Some("Done.".into()),
            created_at: at(1_756_700_090),
            updated_at: at(1_756_700_200),
            spawn_call_id: None,
        };

        let outputs = DeliverablesCatalog {
            deliverables: vec![DeliverableSummary {
                output_id: OutputId(id(0x40)),
                filename: "summary.md".into(),
                media_type: "text/markdown".into(),
                size_bytes: 1234,
                revision_count: 3,
                updated_at: at(1_756_700_300),
                producing_run_id: Some(id(0x32)),
            }],
            truncated: false,
        };
        let preview = DeliverablePreview {
            output_id: OutputId(id(0x40)),
            filename: "summary.md".into(),
            media_type: "text/markdown".into(),
            revision_count: 3,
            revision_id: OutputRevisionId(id(0x43)),
            content: "# Summary\n\nOne paragraph.\n".into(),
            truncated: true,
        };
        let revisions = OutputRevisionsCatalog {
            output_id: OutputId(id(0x40)),
            revisions: vec![
                OutputRevisionInfo {
                    revision_id: OutputRevisionId(id(0x41)),
                    ordinal: 1,
                    size_bytes: 900,
                    created_at: at(1_756_700_100),
                    produced_by: OutputRevisionProducer::Agent,
                    is_current: false,
                    sources: vec![
                        OutputRevisionSource::Document {
                            citation_id: AssistantCitationId(id(0x50)),
                            document_id: DocumentId(id(0x51)),
                            locator: CitationLocator::Pages { start: 2, end: 3 },
                        },
                        OutputRevisionSource::Web {
                            url: "https://example.com/report".into(),
                            label: "Quarterly report".into(),
                            domain: "example.com".into(),
                        },
                    ],
                },
                OutputRevisionInfo {
                    revision_id: OutputRevisionId(id(0x42)),
                    ordinal: 2,
                    size_bytes: 1100,
                    created_at: at(1_756_700_200),
                    produced_by: OutputRevisionProducer::BackgroundAgent,
                    is_current: false,
                    sources: Vec::new(),
                },
                OutputRevisionInfo {
                    revision_id: OutputRevisionId(id(0x43)),
                    ordinal: 3,
                    size_bytes: 1234,
                    created_at: at(1_756_700_300),
                    produced_by: OutputRevisionProducer::User,
                    is_current: true,
                    sources: Vec::new(),
                },
            ],
        };

        fn value<T: serde::Serialize>(record: &T) -> serde_json::Value {
            serde_json::to_value(record).expect("a REST record serializes")
        }
        vec![
            ("model_catalog", "ModelCatalog", value(&catalog)),
            ("providers", "ProvidersList", value(&providers)),
            ("mcp_servers", "McpServersInfo", value(&mcp)),
            ("agent_run_running", "AgentRunSnapshot", value(&running)),
            ("agent_run_completed", "AgentRunSnapshot", value(&completed)),
            ("outputs", "DeliverablesCatalog", value(&outputs)),
            ("output_preview", "DeliverablePreview", value(&preview)),
            (
                "output_revisions",
                "OutputRevisionsCatalog",
                value(&revisions),
            ),
        ]
    }

    /// The checked-in fixture file: a JSON array of `{ "name", "type", "value" }`.
    fn rendered_rest_records() -> String {
        let entries = rest_record_fixtures()
            .into_iter()
            .map(|(name, kind, value)| {
                serde_json::json!({ "name": name, "type": kind, "value": value })
            })
            .collect::<Vec<_>>();
        let mut rendered =
            serde_json::to_string_pretty(&entries).expect("the fixture list serializes");
        rendered.push('\n');
        rendered
    }

    /// Two decoders read these bytes — the CLI's and this crate's own. A diff
    /// here means a REST record's shape changed.
    #[test]
    fn the_rest_record_fixtures_are_current() {
        generate::check_or_update(REST_RECORDS, &rendered_rest_records(), REGENERATE);
    }

    /// Every tag in the fixtures is a type the list declares, and every
    /// declared type has at least one fixture.
    #[test]
    fn rest_record_fixtures_cover_every_type() {
        let tagged: std::collections::BTreeSet<&str> = rest_record_fixtures()
            .iter()
            .map(|(_, kind, _)| *kind)
            .collect();
        let declared: std::collections::BTreeSet<&str> =
            REST_RECORD_TYPES.iter().copied().collect();
        assert_eq!(tagged, declared);
    }

    /// The server's own round trip through the public wire types, the same
    /// way the CLI decodes them.
    #[test]
    fn every_rest_record_fixture_round_trips() {
        fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(
            name: &str,
            value: &serde_json::Value,
        ) {
            let decoded: T = serde_json::from_value(value.clone())
                .unwrap_or_else(|error| panic!("fixture {name} does not decode: {error}"));
            let again = serde_json::to_value(&decoded).expect("a decoded record serializes");
            assert_eq!(
                &again, value,
                "fixture {name} changed across the round trip"
            );
        }
        for (name, kind, value) in rest_record_fixtures() {
            match kind {
                "ModelCatalog" => round_trip::<crate::wire::ModelCatalog>(name, &value),
                "ProvidersList" => round_trip::<crate::wire::ProvidersList>(name, &value),
                "McpServersInfo" => round_trip::<crate::wire::McpServersInfo>(name, &value),
                "AgentRunSnapshot" => round_trip::<crate::wire::AgentRunSnapshot>(name, &value),
                "DeliverablesCatalog" => {
                    round_trip::<crate::wire::DeliverablesCatalog>(name, &value)
                }
                "DeliverablePreview" => round_trip::<crate::wire::DeliverablePreview>(name, &value),
                "OutputRevisionsCatalog" => {
                    round_trip::<crate::wire::OutputRevisionsCatalog>(name, &value)
                }
                other => panic!("fixture {name} has an unknown type tag {other}"),
            }
        }
    }

    /// The fixtures carry the shapes a hand-written mirror got wrong: a
    /// secret-bearing field that never serializes, an omitted optional key,
    /// and every producer of an output revision.
    #[test]
    fn rest_record_fixtures_cover_the_awkward_shapes() {
        let fixtures = rest_record_fixtures();
        let by_name = |name: &str| {
            &fixtures
                .iter()
                .find(|(entry, _, _)| *entry == name)
                .unwrap_or_else(|| panic!("the {name} fixture exists"))
                .2
        };
        let servers = by_name("mcp_servers");
        assert!(servers["servers"][0].get("env_values").is_none());
        assert!(servers["servers"][0].get("launch").is_none());
        assert!(by_name("agent_run_running").get("task_plan").is_some());
        assert!(by_name("agent_run_completed").get("task_plan").is_none());
        assert!(by_name("providers")["providers"][0]
            .get("base_url")
            .is_none());
        let producers: std::collections::BTreeSet<&str> = by_name("output_revisions")["revisions"]
            .as_array()
            .expect("revisions")
            .iter()
            .filter_map(|row| row["producedBy"].as_str())
            .collect();
        assert_eq!(
            producers,
            ["agent", "backgroundAgent", "user"].into_iter().collect()
        );
    }

    /// Unknown keys fail every REST record but the flattened MCP server row,
    /// which is documented on [`crate::wire`].
    #[test]
    fn rest_records_reject_unknown_keys() {
        fn rejects<T: serde::de::DeserializeOwned>(name: &str, value: &serde_json::Value) {
            let mut extra = value.clone();
            extra["extra"] = serde_json::json!(1);
            assert!(
                serde_json::from_value::<T>(extra).is_err(),
                "fixture {name} should reject an unknown key"
            );
        }
        for (name, kind, value) in rest_record_fixtures() {
            match kind {
                "ModelCatalog" => rejects::<crate::wire::ModelCatalog>(name, &value),
                "ProvidersList" => rejects::<crate::wire::ProvidersList>(name, &value),
                "McpServersInfo" => rejects::<crate::wire::McpServersInfo>(name, &value),
                "AgentRunSnapshot" => rejects::<crate::wire::AgentRunSnapshot>(name, &value),
                "DeliverablesCatalog" => rejects::<crate::wire::DeliverablesCatalog>(name, &value),
                "DeliverablePreview" => rejects::<crate::wire::DeliverablePreview>(name, &value),
                "OutputRevisionsCatalog" => {
                    rejects::<crate::wire::OutputRevisionsCatalog>(name, &value)
                }
                other => panic!("fixture {name} has an unknown type tag {other}"),
            }
        }
        // Nested records too: a row inside the envelope is guarded on its own.
        let mut model = rest_record_fixtures()[0].2["models"][0].clone();
        model["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<crate::wire::ModelInfo>(model).is_err());
        let mut source = rest_record_fixtures()
            .iter()
            .find(|(name, _, _)| *name == "output_revisions")
            .expect("the revisions fixture")
            .2["revisions"][0]["sources"][0]
            .clone();
        source["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<crate::wire::OutputRevisionSource>(source).is_err());
        // The one tolerant record: a flattened definition cannot be guarded.
        let mut server = rest_record_fixtures()
            .iter()
            .find(|(name, _, _)| *name == "mcp_servers")
            .expect("the MCP fixture")
            .2["servers"][0]
            .clone();
        server["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<crate::wire::McpServerInfo>(server).is_ok());
    }
}
