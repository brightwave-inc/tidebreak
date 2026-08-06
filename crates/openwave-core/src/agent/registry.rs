//! Name-keyed registry of tools available to the agent.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::agent_tools::{
    validate_spawn_sandbox_agent_arguments, validate_wait_for_agents_arguments,
    SpawnSandboxAgentArgs, WaitForAgentsArgs,
};
use crate::error::Result;
use crate::id::AgentRunId;
use crate::model::ToolCallExecution;
use crate::tool::{ApprovalClass, Tool, ToolCtx, ToolErrorCategory, ToolOutput, ToolSpec};


/// A name-keyed registry of the tools available to the agent.
///
/// The map is ordered by name so that [advertisement](Self::specs) is a pure
/// function of *which* tools are registered, never of how or when they got
/// there. A `HashMap` reordered the advertised block between turns, which
/// invalidates the provider-side prompt-prefix cache and makes a run harder to
/// reproduce. Registration order would be stable within one process but is not
/// set-determined: MCP servers mount and unmount mid-session, so unmounting a
/// server and remounting it would advertise the same tools in a new order.
/// Sorting by name has neither problem.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
}

#[derive(Clone)]
struct RegisteredSpec {
    spec: ToolSpec,
    validator: Option<jsonschema::Validator>,
}

impl RegisteredSpec {
    fn new(spec: ToolSpec) -> Self {
        // The provider-facing schemas generated in this crate deliberately omit
        // `$schema` and are 2020-12. MCP servers may declare an older supported
        // dialect explicitly, so honor that declaration instead of applying
        // 2020-12 semantics to (for example) draft-07 tuple-form `items`.
        let validator = match advertised_schema_draft(&spec.input_schema) {
            Ok(draft) => {
                let unsupported = unsupported_schema_keywords(&spec.input_schema, draft);
                if !unsupported.is_empty() {
                    tracing::warn!(
                        tool = %spec.name,
                        keywords = %unsupported.into_iter().collect::<Vec<_>>().join(", "),
                        "tool argument schema contains unsupported keywords; ignoring them while enforcing supported constraints"
                    );
                }
                match jsonschema::options()
                    .with_draft(draft)
                    .build(&spec.input_schema)
                {
                    Ok(validator) => Some(validator),
                    Err(error) => {
                        // A bad advertised schema is the tool/server's bug, not
                        // the model's. Bricking every call would break working
                        // MCP servers, so compilation failures are observable
                        // but fail open.
                        tracing::warn!(
                            tool = %spec.name,
                            %error,
                            "tool argument schema could not be compiled; calls will proceed without schema validation"
                        );
                        None
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    tool = %spec.name,
                    %error,
                    "tool argument schema declares an unsupported dialect; calls will proceed without schema validation"
                );
                None
            }
        };
        Self { spec, validator }
    }

    fn mismatch(&self, arguments: &Value) -> Option<String> {
        self.validator
            .as_ref()?
            .validate(arguments)
            .err()
            .map(|error| error.to_string())
    }
}

fn advertised_schema_draft(schema: &Value) -> std::result::Result<jsonschema::Draft, String> {
    let Some(declared) = schema.as_object().and_then(|schema| schema.get("$schema")) else {
        return Ok(jsonschema::Draft::Draft202012);
    };
    let Some(uri) = declared.as_str() else {
        return Err("$schema must be a string".to_owned());
    };
    let draft = jsonschema::Draft::from_schema_uri(uri);
    if draft == jsonschema::Draft::Unknown {
        return Err(format!("unrecognized $schema URI: {uri}"));
    }
    Ok(draft)
}

/// Keywords that the validator treats as annotations rather than constraints.
///
/// `Draft::is_known_keyword` intentionally reports validation/applicator/core
/// keywords only. These standard annotations must not be diagnosed as custom
/// extensions just because they do not affect acceptance.
const SCHEMA_ANNOTATION_KEYWORDS: &[&str] = &[
    "$comment",
    "$vocabulary",
    "default",
    "deprecated",
    "description",
    "examples",
    "readOnly",
    "title",
    "writeOnly",
];

fn unsupported_schema_keywords(schema: &Value, draft: jsonschema::Draft) -> BTreeSet<String> {
    fn visit(schema: &Value, draft: jsonschema::Draft, unsupported: &mut BTreeSet<String>) {
        if let Some(object) = schema.as_object() {
            for keyword in object.keys() {
                if !draft.is_known_keyword(keyword)
                    && !SCHEMA_ANNOTATION_KEYWORDS.contains(&keyword.as_str())
                {
                    unsupported.insert(keyword.clone());
                }
            }
        }
        for subschema in draft.subresources_of(schema) {
            visit(subschema, draft, unsupported);
        }
    }

    let mut unsupported = BTreeSet::new();
    visit(schema, draft, &mut unsupported);
    unsupported
}

/// Registry-owned guard around every server-side tool executor.
///
/// The foreground agent and MCP server validate before their approval gates so
/// a malformed call never asks for consent. This wrapper is the final dispatch
/// boundary for every other registry consumer, including mounted MCP proxies:
/// direct callers receive the same typed correction instead of bypassing the
/// advertised contract and forwarding the call.
struct SchemaValidatedTool {
    inner: Arc<dyn Tool>,
    registered: RegisteredSpec,
}

#[async_trait]
impl Tool for SchemaValidatedTool {
    fn spec(&self) -> ToolSpec {
        self.registered.spec.clone()
    }

    fn approval_class(&self) -> ApprovalClass {
        self.inner.approval_class()
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if let Some(mismatch) = self.registered.mismatch(&args) {
            return Ok(ToolOutput::failed(
                ToolErrorCategory::InvalidArguments,
                format!(
                    "arguments for {} do not satisfy its schema: {mismatch}; re-send the call \
                     with arguments matching this schema: {}",
                    self.registered.spec.name, self.registered.spec.input_schema
                ),
            ));
        }
        self.inner.execute(ctx, args).await
    }
}

#[derive(Clone)]
enum RegisteredTool {
    Server {
        tool: Arc<dyn Tool>,
        registered: RegisteredSpec,
    },
    Client {
        registered: RegisteredSpec,
        validate_arguments: Option<fn(&Value) -> bool>,
        class: ApprovalClass,
    },
    ForegroundClient {
        registered: RegisteredSpec,
        validate_arguments: fn(&Value) -> bool,
        class: ApprovalClass,
    },
    ForegroundOrchestration {
        registered: RegisteredSpec,
        kind: ForegroundOrchestrationKind,
        class: ApprovalClass,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForegroundOrchestrationKind {
    Spawn,
    Wait,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool under its advertised name (replacing any existing one).
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let registered = RegisteredSpec::new(tool.spec());
        let tool: Arc<dyn Tool> = Arc::new(SchemaValidatedTool {
            inner: Arc::from(tool),
            registered: registered.clone(),
        });
        self.tools.insert(
            registered.spec.name.clone(),
            RegisteredTool::Server { tool, registered },
        );
    }

    /// Register a client-owned tool contract with no server-side executor.
    ///
    /// The declared class is the host's reading of what the tool touches, kept
    /// on the registration because a client tool has no [`Tool`] impl to ask.
    /// Plan mode advertises and checkpoints only `ReadOnly` registrations.
    pub fn register_client(&mut self, spec: ToolSpec, class: ApprovalClass) {
        self.tools.insert(
            spec.name.clone(),
            RegisteredTool::Client {
                registered: RegisteredSpec::new(spec),
                validate_arguments: None,
                class,
            },
        );
    }

    /// Register a client-owned contract with payload validation at checkpoint time.
    pub fn register_validated_client(
        &mut self,
        spec: ToolSpec,
        class: ApprovalClass,
        validate_arguments: fn(&Value) -> bool,
    ) {
        self.tools.insert(
            spec.name.clone(),
            RegisteredTool::Client {
                registered: RegisteredSpec::new(spec),
                validate_arguments: Some(validate_arguments),
                class,
            },
        );
    }

    /// Register a validated client continuation that is visible only to a
    /// claimed foreground coordinator, never to sandbox/direct agent surfaces.
    pub fn register_validated_foreground_client(
        &mut self,
        spec: ToolSpec,
        class: ApprovalClass,
        validate_arguments: fn(&Value) -> bool,
    ) {
        self.tools.insert(
            spec.name.clone(),
            RegisteredTool::ForegroundClient {
                registered: RegisteredSpec::new(spec),
                validate_arguments,
                class,
            },
        );
    }

    /// Register the closed foreground-only spawn and ordered-wait contracts.
    ///
    /// A claimed foreground worker must still opt in before either definition
    /// is advertised. Sandboxed workers never opt in, keeping delegation depth
    /// bounded at one.
    ///
    /// Both declare [`ApprovalClass::Sensitive`]. A delegated child reaches the
    /// public web and, where the host routes children to a container, runs
    /// commands there; none of those calls pass back through this chat's
    /// approval gate, so the delegation itself is the boundary that carries
    /// their weight. The pair shares one class because it is one contract: the
    /// wait exists only to consume what a spawn produced, and advertising half
    /// of it in a surface that forbids the other half would only invite a call
    /// that cannot be honored.
    ///
    /// The class decides advertisement, not admission: a spawn writes its tool
    /// call already completed, so there is no pending record for the approval
    /// gate to park on. Issue #1477 holds what closing that would take.
    pub fn register_foreground_agent_orchestration(&mut self) {
        for (spec, kind) in [
            (
                crate::spawn_sandbox_agent_tool_spec(),
                ForegroundOrchestrationKind::Spawn,
            ),
            (
                crate::wait_for_agents_tool_spec(),
                ForegroundOrchestrationKind::Wait,
            ),
        ] {
            self.tools.insert(
                spec.name.clone(),
                RegisteredTool::ForegroundOrchestration {
                    registered: RegisteredSpec::new(spec),
                    kind,
                    class: ApprovalClass::Sensitive,
                },
            );
        }
    }

    /// Builder-style [`register`](Self::register).
    #[must_use]
    pub fn with(mut self, tool: Box<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        match self.tools.get(name) {
            Some(RegisteredTool::Server { tool, .. }) => Some(tool.as_ref()),
            Some(RegisteredTool::Client { .. })
            | Some(RegisteredTool::ForegroundClient { .. })
            | Some(RegisteredTool::ForegroundOrchestration { .. })
            | None => None,
        }
    }

    /// Whether any tool is registered under `name`, whatever its execution
    /// surface. Callers registering names they do not control use this to avoid
    /// replacing an existing registration.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// The shared server-side executor registered under `name`, for callers
    /// that decorate a registration (re-registering under the same name with
    /// an amended spec) while delegating execution to the original tool.
    #[must_use]
    pub fn server_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        match self.tools.get(name)? {
            RegisteredTool::Server { tool, .. } => Some(tool.clone()),
            RegisteredTool::Client { .. }
            | RegisteredTool::ForegroundClient { .. }
            | RegisteredTool::ForegroundOrchestration { .. } => None,
        }
    }

    /// Resolve the trusted execution surface for a registered tool name.
    #[must_use]
    pub fn execution(&self, name: &str) -> Option<ToolCallExecution> {
        Some(match self.tools.get(name)? {
            RegisteredTool::Server { .. } => ToolCallExecution::Server,
            RegisteredTool::Client { .. } | RegisteredTool::ForegroundClient { .. } => {
                ToolCallExecution::Client
            }
            RegisteredTool::ForegroundOrchestration { .. } => return None,
        })
    }

    /// The specs of every registered tool, to advertise to the model.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.specs_for_foreground(false)
    }

    /// The model-visible definitions for one execution surface.
    ///
    /// The foreground coordinator may opt into the sandbox control tool. All
    /// other contexts receive the ordinary server/client tool set only.
    #[must_use]
    pub fn specs_for_foreground(&self, allow_agent_orchestration: bool) -> Vec<ToolSpec> {
        self.specs_for_surface(allow_agent_orchestration, false)
    }

    /// The model-visible definitions for one execution surface, optionally
    /// narrowed to the read-only planning subset.
    ///
    /// A plan-mode turn advertises only `ReadOnly` registrations — server
    /// tools by their declared [`Tool::approval_class`], client and
    /// orchestration tools by the class declared at registration. The
    /// orchestration pair is `Sensitive`, so a plan turn drops it by that same
    /// rule: spawned agents execute, and executing is exactly what a plan turn
    /// must not do.
    #[must_use]
    pub fn specs_for_surface(
        &self,
        allow_agent_orchestration: bool,
        read_only: bool,
    ) -> Vec<ToolSpec> {
        self.tools
            .values()
            .filter_map(|tool| match tool {
                RegisteredTool::Server { tool, .. }
                    if read_only && tool.approval_class() != ApprovalClass::ReadOnly =>
                {
                    None
                }
                RegisteredTool::Server { registered, .. } => Some(registered.spec.clone()),
                RegisteredTool::Client { class, .. }
                | RegisteredTool::ForegroundClient { class, .. }
                | RegisteredTool::ForegroundOrchestration { class, .. }
                    if read_only && *class != ApprovalClass::ReadOnly =>
                {
                    None
                }
                RegisteredTool::Client { registered, .. } => Some(registered.spec.clone()),
                // The plan continuation exists only where a plan can be
                // proposed: outside plan mode the tool would park a turn on a
                // decision whose accept is meaningless.
                RegisteredTool::ForegroundClient { registered, .. }
                    if !read_only && registered.spec.name == crate::EXIT_PLAN_MODE_TOOL =>
                {
                    None
                }
                RegisteredTool::ForegroundClient { registered, .. }
                    if allow_agent_orchestration =>
                {
                    Some(registered.spec.clone())
                }
                RegisteredTool::ForegroundClient { .. } => None,
                RegisteredTool::ForegroundOrchestration { registered, .. }
                    if allow_agent_orchestration =>
                {
                    Some(registered.spec.clone())
                }
                RegisteredTool::ForegroundOrchestration { .. } => None,
            })
            .collect()
    }

    /// The declared approval class of every registered tool.
    ///
    /// `None` means only that nothing is registered under `name`. Every
    /// registration declares a class, including the orchestration pair, so a
    /// caller asking what a name costs is never answered with silence it has
    /// to interpret.
    #[must_use]
    pub fn registered_class(&self, name: &str) -> Option<ApprovalClass> {
        match self.tools.get(name)? {
            RegisteredTool::Server { tool, .. } => Some(tool.approval_class()),
            RegisteredTool::Client { class, .. }
            | RegisteredTool::ForegroundClient { class, .. }
            | RegisteredTool::ForegroundOrchestration { class, .. } => Some(*class),
        }
    }

    /// Validate arguments against the exact schema stored at registration.
    ///
    /// A returned string names the first failing instance path and constraint.
    /// `None` means either that the arguments conform or that the tool supplied
    /// an invalid schema, which remains a fail-open registration bug.
    #[must_use]
    pub fn schema_mismatch(&self, name: &str, arguments: &Value) -> Option<String> {
        let registered = match self.tools.get(name)? {
            RegisteredTool::Server { registered, .. }
            | RegisteredTool::Client { registered, .. }
            | RegisteredTool::ForegroundClient { registered, .. }
            | RegisteredTool::ForegroundOrchestration { registered, .. } => registered,
        };
        registered.mismatch(arguments)
    }

    /// Validate canonical arguments against a registered client-owned contract.
    #[must_use]
    pub fn client_arguments_are_valid(&self, name: &str, arguments: &Value) -> bool {
        match self.tools.get(name) {
            Some(RegisteredTool::Client {
                validate_arguments: Some(validate),
                ..
            }) => validate(arguments),
            Some(RegisteredTool::Client {
                validate_arguments: None,
                ..
            }) => true,
            Some(RegisteredTool::ForegroundClient {
                validate_arguments, ..
            }) => validate_arguments(arguments),
            Some(RegisteredTool::Server { .. })
            | Some(RegisteredTool::ForegroundOrchestration { .. })
            | None => false,
        }
    }

    /// Whether `name` is a client continuation restricted to a claimed
    /// foreground coordinator.
    #[must_use]
    pub fn is_foreground_client(&self, name: &str) -> bool {
        matches!(
            self.tools.get(name),
            Some(RegisteredTool::ForegroundClient { .. })
        )
    }

    /// Whether `name` identifies the foreground-only sandbox control tool.
    #[must_use]
    pub fn is_foreground_sandbox_spawn(&self, name: &str) -> bool {
        matches!(
            self.tools.get(name),
            Some(RegisteredTool::ForegroundOrchestration {
                kind: ForegroundOrchestrationKind::Spawn,
                ..
            })
        )
    }

    /// Parse and validate one foreground sandbox task.
    #[must_use]
    pub fn sandbox_spawn_task(&self, name: &str, arguments: &Value) -> Option<String> {
        if !self.is_foreground_sandbox_spawn(name)
            || !validate_spawn_sandbox_agent_arguments(arguments)
        {
            return None;
        }
        serde_json::from_value::<SpawnSandboxAgentArgs>(arguments.clone())
            .ok()
            .map(|arguments| arguments.task)
    }

    /// Whether `name` identifies the foreground-only ordered wait tool.
    #[must_use]
    pub fn is_foreground_agent_wait(&self, name: &str) -> bool {
        matches!(
            self.tools.get(name),
            Some(RegisteredTool::ForegroundOrchestration {
                kind: ForegroundOrchestrationKind::Wait,
                ..
            })
        )
    }

    /// Parse and validate one ordered foreground child wait.
    #[must_use]
    pub fn wait_for_agent_ids(&self, name: &str, arguments: &Value) -> Option<Vec<AgentRunId>> {
        if !self.is_foreground_agent_wait(name) || !validate_wait_for_agents_arguments(arguments) {
            return None;
        }
        serde_json::from_value::<WaitForAgentsArgs>(arguments.clone())
            .ok()
            .map(|arguments| arguments.agent_ids)
    }

    /// Whether no tools are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

