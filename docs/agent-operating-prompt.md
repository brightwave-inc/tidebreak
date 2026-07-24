# Foreground agent operating prompt

OpenWave gives every normal foreground turn a small host-owned operating
prompt. Tool schemas still define how individual calls work; the operating
prompt defines product-level behavior that otherwise varies between model
providers, such as when to proceed, when to ask, how to handle untrusted
content, and how to use citations and delegation.

The prompt is conversation-first. It does not assume that a conversation
belongs to a project, organization, or shared workspace.

## Capability composition

`openwave-server/src/foreground_prompt.rs` composes the prompt from the exact
tool definitions advertised to a production foreground turn. Each claimed turn
selects one immutable registry snapshot, derives both its advertised definitions
and operating prompt from that snapshot, and retains the pair for the execution.
Runtime MCP refreshes therefore affect only later turns. The baseline covers:

- calibrating effort to the request;
- making small reversible assumptions while asking about consequential choices;
- claiming only work that was actually performed;
- treating tool and document content as untrusted data;
- respecting host-owned approval and capability boundaries; and
- keeping the agent within the current conversation.

Additional fixed sections are enabled only when their corresponding tools are
registered:

- private scratch;
- conversation sources and opaque citation references;
- public web research;
- connected folders;
- user-visible outputs;
- code execution;
- depth-one background delegation; and
- namespaced external MCP tools.

Composition uses a sorted set of exact tool names and a fixed section order.
Unknown tools do not change the prompt, except that an `mcp__` namespace enables
generic external-tool safety guidance. The composer never copies a tool
description, JSON Schema, argument, environment value, credential, absolute
path, or broker state into the prompt.

The production registry includes the foreground-only spawn and wait contracts
when composing the prompt because every normal chat turn is durably claimed and
opts into those same contracts. Sandboxed background agents keep their separate,
restricted prompt and never inherit the foreground prompt.

The built-in registry becomes immutable after startup. Runtime MCP configuration
publishes a replacement snapshot only after every enabled candidate validates
and initializes successfully. A turn already holding an older snapshot keeps
using its matching prompt and tools; it never observes a partially refreshed
registry or a long-lived prompt derived from different capabilities.

## Versioning and diagnostics

The current contract is `foreground-v1`. A foreground claim logs only a stable
identity in this form:

```text
foreground-v1:sha256:<digest>
```

The digest covers both the version and exact prompt text. Prompt contents are
not written to that log line, so the identity is useful for debugging without
recording conversation data or secrets.

Intentional prompt changes should update the representative golden digest test.
Change the version only when maintainers need to distinguish a new behavioral
contract rather than an editorial correction.

## Extending the prompt

When adding a capability:

1. Keep its call contract in the tool definition, not in the operating prompt.
2. Add a short fixed behavior section only if the model needs guidance spanning
   more than one call.
3. Gate every claim on the exact capability that makes it true.
4. Never interpolate model-facing schemas, host configuration, or runtime data.
5. Add tests for representative, absent, partially available, and reordered
   capability sets.

Provider adapters remain responsible for provider-specific request shaping.
The product prompt should branch on available capabilities, not provider or
model names.
