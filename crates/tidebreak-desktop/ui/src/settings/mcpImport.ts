import type { McpServerInfo } from "../api";
import { isRecord } from "../lib/guards";

const MAX_SERVERS = 32;
const MAX_ARGS = 128;
const MAX_ENVIRONMENT_VARIABLES = 128;
const MAX_ENVIRONMENT_NAME_BYTES = 256;
const MAX_PROCESS_STRING_BYTES = 32 * 1024;
const MAX_REQUEST_TIMEOUT_MS = 3_600_000;
const DEFAULT_REQUEST_TIMEOUT_MS = 60_000;
const MAX_NAMESPACE_BYTES = 32;
const MAX_IMPORT_BYTES = 1024 * 1024;

type ImportShape = "tidebreak" | "common";

type SourceEntry = {
  fallbackName: string;
  raw: unknown;
  shape: ImportShape;
  path: string;
};

export type McpImportSkip = {
  name: string;
  path: string;
  reason: string;
};

export type McpImportSecret = {
  server: string;
  name: string;
};

export type McpImportResult = {
  servers: McpServerInfo[];
  skipped: McpImportSkip[];
  secrets: McpImportSecret[];
};

type ParsedEntry = {
  server: McpServerInfo;
  secrets: McpImportSecret[];
};

type FieldResult<T> = { value: T } | { error: string };

const SUPPORTED_SHAPES =
  'Use a Claude Desktop or Cursor {"mcpServers": {"name": { ... }}} file, a VS Code {"servers": {"name": { ... }}} file, a Tidebreak {"servers": [{"name": "...", ...}]} file, a single server object, or an array of server objects.';

const VALID_NAMESPACE = "A valid value looks like docs or remote-tools.";
const VALID_COMMAND = "A valid value looks like npx or /usr/local/bin/server.";
const VALID_URL = "A valid value looks like https://example.test/mcp.";
const VALID_ENV_OBJECT =
  'A valid value looks like { "LOG_LEVEL": "debug" } or ["LOG_LEVEL"].';
const VALID_BEARER = "A valid value looks like Bearer ${env:TOKEN}.";

export function parseMcpImportText(
  text: string,
  existingServers: readonly McpServerInfo[],
): McpImportResult {
  if (new TextEncoder().encode(text).length > MAX_IMPORT_BYTES) {
    throw new Error("Use a JSON document no larger than 1 MB.");
  }
  if (text.trim().length === 0) {
    throw new Error(
      "Paste a JSON object or array of MCP servers. A valid file starts with { or [.",
    );
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    throw new Error(jsonSyntaxMessage(text, error));
  }
  return parseMcpImport(value, existingServers);
}

export function parseMcpImport(
  value: unknown,
  existingServers: readonly McpServerInfo[],
): McpImportResult {
  const entries = sourceEntries(value);
  const taken = new Set(existingServers.map((server) => server.name));
  const seen = new Set<string>();
  const remaining = Math.max(
    0,
    MAX_SERVERS -
      existingServers.filter((server) => server.plugin === null).length,
  );
  const servers: McpServerInfo[] = [];
  const skipped: McpImportSkip[] = [];
  const secrets: McpImportSecret[] = [];

  for (const [index, entry] of entries.entries()) {
    const rawName =
      entry.shape === "common"
        ? entry.fallbackName
        : isRecord(entry.raw) && typeof entry.raw.name === "string"
          ? entry.raw.name
          : entry.fallbackName;
    const namePath =
      entry.shape === "common"
        ? entry.path
        : joinPath(entry.path, "name") || "name";
    const label = rawName || `Server ${index + 1}`;

    if (!validNamespace(rawName)) {
      skipped.push({
        name: label,
        path: namePath,
        reason: namespaceIssue(namePath, rawName),
      });
      continue;
    }
    if (seen.has(rawName)) {
      skipped.push({
        name: rawName,
        path: namePath,
        reason: issue(
          namePath,
          "Namespace appears more than once in this file.",
          "Choose a different name.",
        ),
      });
      continue;
    }
    seen.add(rawName);
    if (taken.has(rawName)) {
      skipped.push({
        name: rawName,
        path: namePath,
        reason: issue(
          namePath,
          "Namespace already exists.",
          "Choose a different name.",
        ),
      });
      continue;
    }
    if (servers.length >= remaining) {
      skipped.push({
        name: rawName,
        path: entry.path,
        reason: issue(
          entry.path,
          `Tidebreak supports up to ${MAX_SERVERS} configured servers.`,
          "Remove extra entries.",
        ),
      });
      continue;
    }

    const parsed = parseEntry(rawName, entry.raw, entry.shape, entry.path);
    if ("error" in parsed) {
      skipped.push({ name: rawName, path: entry.path, reason: parsed.error });
      continue;
    }
    servers.push(parsed.value.server);
    secrets.push(...parsed.value.secrets);
    taken.add(rawName);
  }

  return { servers, skipped, secrets };
}

function sourceEntries(value: unknown): SourceEntry[] {
  if (Array.isArray(value)) {
    if (value.length === 0) {
      throw new Error(SUPPORTED_SHAPES);
    }
    return value.map((raw, index) => ({
      fallbackName: "",
      raw,
      shape: "tidebreak",
      path: mcpJsonPath([index]),
    }));
  }
  if (!isRecord(value)) {
    throw new Error(
      "Use a JSON object or array. A valid file starts with { or [.",
    );
  }
  if (value.mcpServers !== undefined && value.servers !== undefined) {
    throw new Error(
      "Use either mcpServers or servers as the top-level key, not both. Pick the Claude/Cursor mcpServers object or the Tidebreak/VS Code servers value.",
    );
  }
  if (value.mcpServers !== undefined) {
    if (!isRecord(value.mcpServers)) {
      throw new Error(
        issue(
          "mcpServers",
          "Wrap named servers in an object.",
          'A valid value looks like { "docs": { "command": "npx" } }.',
        ),
      );
    }
    return Object.entries(value.mcpServers).map(([name, raw]) => ({
      fallbackName: name,
      raw,
      shape: "common",
      path: mcpJsonPath(["mcpServers", name]),
    }));
  }
  if (value.servers !== undefined) {
    if (Array.isArray(value.servers)) {
      return value.servers.map((raw, index) => ({
        fallbackName: "",
        raw,
        shape: "tidebreak",
        path: mcpJsonPath(["servers", index]),
      }));
    }
    if (isRecord(value.servers)) {
      return Object.entries(value.servers).map(([name, raw]) => ({
        fallbackName: name,
        raw,
        shape: "common",
        path: mcpJsonPath(["servers", name]),
      }));
    }
    throw new Error(
      issue(
        "servers",
        "Use a Tidebreak array of server objects or a VS Code object of named servers.",
        'A valid value looks like [{ "name": "docs", "command": "npx" }] or { "docs": { "command": "npx" } }.',
      ),
    );
  }
  if (looksLikeServer(value)) {
    const fallbackName =
      typeof value.name === "string" && value.name.length > 0
        ? value.name
        : "imported";
    return [
      {
        fallbackName,
        raw: value,
        shape:
          value.gateway_endpoint !== undefined ||
          typeof value.name === "string"
            ? "tidebreak"
            : "common",
        path: "",
      },
    ];
  }
  throw new Error(SUPPORTED_SHAPES);
}

function looksLikeServer(value: Record<string, unknown>): boolean {
  return (
    typeof value.command === "string" ||
    Array.isArray(value.command) ||
    typeof value.url === "string" ||
    typeof value.serverUrl === "string" ||
    typeof value.gateway_endpoint === "string" ||
    typeof value.type === "string"
  );
}

function parseEntry(
  name: string,
  raw: unknown,
  shape: ImportShape,
  path: string,
): FieldResult<ParsedEntry> {
  if (!isRecord(raw)) {
    return {
      error: issue(
        path,
        "Server definition must be an object.",
        'A valid value looks like { "command": "npx" } or { "url": "https://example.test/mcp" }.',
      ),
    };
  }

  const typeHint = readType(raw, path);
  if ("error" in typeHint) return typeHint;
  const commandSpec = readCommand(raw, path);
  if ("error" in commandSpec) return commandSpec;
  const url = readUrl(raw, path);
  if ("error" in url) return url;
  const gateway =
    shape === "tidebreak"
      ? nullableString(raw, "gateway_endpoint", path)
      : { value: null };
  if ("error" in gateway) return gateway;

  const command = commandSpec.value.command;
  if (typeHint.value === "stdio") {
    if (url.value !== null) {
      return {
        error: issue(
          joinPath(path, "url") || "url",
          "stdio servers set command, not url.",
          VALID_COMMAND,
        ),
      };
    }
    if (gateway.value !== null) {
      return {
        error: issue(
          joinPath(path, "gateway_endpoint") || "gateway_endpoint",
          "stdio servers set command, not a gateway endpoint.",
          VALID_COMMAND,
        ),
      };
    }
    if (command === null) {
      return {
        error: issue(
          joinPath(path, "command") || "command",
          "stdio servers set command.",
          VALID_COMMAND,
        ),
      };
    }
  } else if (typeHint.value === "http") {
    if (command !== null) {
      return {
        error: issue(
          joinPath(path, "command") || "command",
          "http and sse servers set url, not command.",
          VALID_URL,
        ),
      };
    }
    if (gateway.value !== null) {
      return {
        error: issue(
          joinPath(path, "gateway_endpoint") || "gateway_endpoint",
          "http and sse servers set url, not a gateway endpoint.",
          VALID_URL,
        ),
      };
    }
    if (url.value === null) {
      return {
        error: issue(
          joinPath(path, "url") || "url",
          "http and sse servers set url.",
          VALID_URL,
        ),
      };
    }
  } else {
    const transports = [command, url.value, gateway.value].filter(
      (candidate) => candidate !== null,
    );
    if (transports.length !== 1) {
      return {
        error: issue(
          path,
          shape === "tidebreak"
            ? "Configure exactly one command, URL, or gateway endpoint."
            : "Configure exactly one command or URL.",
          "A stdio server sets command; an HTTP server sets url.",
        ),
      };
    }
  }

  const argsField = stringArray(
    raw.args,
    joinPath(path, "args") || "args",
    "Arguments",
  );
  if ("error" in argsField) return argsField;
  if (
    commandSpec.value.argsFromCommand.length > 0 &&
    argsField.value.length > 0
  ) {
    return {
      error: issue(
        joinPath(path, "args") || "args",
        "Set arguments on command or on args, not both.",
        'A valid value looks like ["-y", "package"].',
      ),
    };
  }
  const args = [...commandSpec.value.argsFromCommand, ...argsField.value];
  if (args.length > MAX_ARGS) {
    return {
      error: issue(
        joinPath(path, "args") || "args",
        `Arguments contain at most ${MAX_ARGS} items.`,
        "Remove extra arguments.",
      ),
    };
  }

  const environment = readEnvironment(raw, path);
  if ("error" in environment) return environment;
  const forwardedEnvironment = stringArray(
    raw.env_from,
    joinPath(path, "env_from") || "env_from",
    "Forwarded environment",
  );
  if ("error" in forwardedEnvironment) return forwardedEnvironment;

  let secretValueNames: string[] = [];
  if (raw.env_values !== undefined && raw.env_values !== null) {
    if (!isRecord(raw.env_values)) {
      return {
        error: issue(
          joinPath(path, "env_values") || "env_values",
          "Environment values must be a JSON object.",
          'A valid value looks like { "LOG_LEVEL": "debug" }.',
        ),
      };
    }
    const envValuesPath = joinPath(path, "env_values") || "env_values";
    for (const [key, item] of Object.entries(raw.env_values)) {
      if (typeof item !== "string") {
        return {
          error: issue(
            joinPath(envValuesPath, key),
            "Environment values are strings.",
            'Use a text value such as "debug".',
          ),
        };
      }
    }
    secretValueNames = Object.keys(raw.env_values);
  }
  const env = unique([...environment.value.env, ...secretValueNames]);
  const envFrom = unique([
    ...environment.value.envFrom,
    ...forwardedEnvironment.value,
  ]);
  if (env.length + envFrom.length > MAX_ENVIRONMENT_VARIABLES) {
    return {
      error: issue(
        joinPath(path, "env") || "env",
        `Environment contains at most ${MAX_ENVIRONMENT_VARIABLES} names.`,
        "Remove extra names.",
      ),
    };
  }
  const environmentError = validateEnvironmentNames(
    [...env, ...envFrom],
    joinPath(path, "env") || "env",
  );
  if (environmentError !== null) return { error: environmentError };
  if (new Set([...env, ...envFrom]).size !== env.length + envFrom.length) {
    return {
      error: issue(
        joinPath(path, "env") || "env",
        "An environment variable name is configured more than once.",
        "Use each name once in env or env_from.",
      ),
    };
  }

  const cwd = nullableString(raw, "cwd", path);
  if ("error" in cwd) return cwd;
  const bearer = nullableString(raw, "bearer_token_env", path);
  if ("error" in bearer) return bearer;
  const headerBearer = bearerEnvironmentFromHeaders(raw.headers, path);
  if ("error" in headerBearer) return headerBearer;
  if (
    bearer.value !== null &&
    headerBearer.value !== null &&
    bearer.value !== headerBearer.value
  ) {
    return {
      error: issue(
        joinPath(path, "bearer_token_env") || "bearer_token_env",
        "Bearer token variables are configured more than once.",
        "Set bearer_token_env or an Authorization header, not both.",
      ),
    };
  }
  const bearerEnvironment = bearer.value ?? headerBearer.value;
  if (bearerEnvironment !== null) {
    const bearerError = validateEnvironmentNames(
      [bearerEnvironment],
      joinPath(path, "bearer_token_env") || "bearer_token_env",
    );
    if (bearerError !== null) return { error: bearerError };
  }

  const enabled = booleanField(raw, "enabled", true, path);
  if ("error" in enabled) return enabled;
  const timeout = numberField(
    raw,
    "request_timeout_ms",
    DEFAULT_REQUEST_TIMEOUT_MS,
    path,
  );
  if ("error" in timeout) return timeout;
  if (
    !Number.isInteger(timeout.value) ||
    timeout.value < 1 ||
    timeout.value > MAX_REQUEST_TIMEOUT_MS
  ) {
    return {
      error: issue(
        joinPath(path, "request_timeout_ms") || "request_timeout_ms",
        `Request timeout is a whole number from 1 to ${MAX_REQUEST_TIMEOUT_MS.toLocaleString()}.`,
        "A valid value looks like 60000.",
      ),
    };
  }

  if (command !== null) {
    if (command.length === 0) {
      return {
        error: issue(
          joinPath(path, "command") || "command",
          "Command must not be empty.",
          VALID_COMMAND,
        ),
      };
    }
    if (bearerEnvironment !== null) {
      return {
        error: issue(
          joinPath(path, "bearer_token_env") || "bearer_token_env",
          "Bearer token variables apply only to URL servers.",
          "Remove the bearer variable or switch this server to HTTP.",
        ),
      };
    }
  } else if (
    args.length > 0 ||
    env.length > 0 ||
    envFrom.length > 0 ||
    cwd.value !== null
  ) {
    return {
      error: issue(
        path,
        "Arguments, environment, and working directory apply only to command servers.",
        "Move those fields onto a stdio server, or remove them.",
      ),
    };
  }

  if (url.value !== null) {
    const urlError = validateUrl(
      joinPath(path, "url") || "url",
      url.value,
      bearerEnvironment !== null,
    );
    if (urlError !== null) return { error: urlError };
  } else if (bearerEnvironment !== null) {
    return {
      error: issue(
        joinPath(path, "bearer_token_env") || "bearer_token_env",
        "Bearer token variables apply only to URL servers.",
        "Remove the bearer variable or switch this server to HTTP.",
      ),
    };
  }

  if (gateway.value !== null && !validGatewayEndpoint(gateway.value)) {
    return {
      error: issue(
        joinPath(path, "gateway_endpoint") || "gateway_endpoint",
        "Gateway endpoint uses 1–127 ASCII letters, numbers, underscores, or hyphens.",
        "A valid value looks like tools or example-security_2.",
      ),
    };
  }

  const processStrings = [
    command,
    url.value,
    cwd.value,
    ...args,
  ].filter((item): item is string => item !== null);
  if (
    processStrings.some(
      (item) => item.length > MAX_PROCESS_STRING_BYTES || item.includes("\0"),
    )
  ) {
    return {
      error: issue(
        path,
        "Command, URL, arguments, and working directory must be valid text.",
        "Use plain text without NUL characters.",
      ),
    };
  }

  return {
    value: {
      server: {
        name,
        command,
        args,
        env,
        env_from: envFrom,
        cwd: cwd.value,
        url: url.value,
        bearer_token_env: bearerEnvironment,
        gateway_endpoint: gateway.value,
        request_timeout_ms: timeout.value,
        enabled: enabled.value,
        plugin: null,
        health: "initializing",
        tool_count: 0,
        diagnostic: null,
        curated: null,
      },
      secrets: env.map((environmentName) => ({
        server: name,
        name: environmentName,
      })),
    },
  };
}

function readType(
  raw: Record<string, unknown>,
  path: string,
): FieldResult<"stdio" | "http" | null> {
  const value = raw.type;
  if (value === undefined || value === null) return { value: null };
  const field = joinPath(path, "type") || "type";
  if (typeof value !== "string") {
    return {
      error: issue(field, "Set type to a string.", 'Use "stdio", "http", or "sse".'),
    };
  }
  const normalized = value.toLowerCase();
  if (normalized === "stdio") return { value: "stdio" };
  if (normalized === "http" || normalized === "sse") return { value: "http" };
  return {
    error: issue(
      field,
      `Tidebreak imports stdio, http, and sse; ${JSON.stringify(clip(value))} is not one of those.`,
      'Set type to "stdio", "http", or "sse".',
    ),
  };
}

function readCommand(
  raw: Record<string, unknown>,
  path: string,
): FieldResult<{ command: string | null; argsFromCommand: string[] }> {
  const value = raw.command;
  const field = joinPath(path, "command") || "command";
  if (value === undefined || value === null) {
    return { value: { command: null, argsFromCommand: [] } };
  }
  if (typeof value === "string") {
    return { value: { command: value, argsFromCommand: [] } };
  }
  if (Array.isArray(value)) {
    if (value.length === 0) {
      return {
        error: issue(field, "Command must not be empty.", VALID_COMMAND),
      };
    }
    if (!value.every((item) => typeof item === "string")) {
      return {
        error: issue(
          field,
          "Command is a string or an array of strings.",
          'A valid value looks like npx or ["npx", "-y", "package"].',
        ),
      };
    }
    const [command, ...argsFromCommand] = value as string[];
    return { value: { command, argsFromCommand } };
  }
  return {
    error: issue(
      field,
      "Command is a string or an array of strings.",
      'A valid value looks like npx or ["npx", "-y", "package"].',
    ),
  };
}

function readUrl(
  raw: Record<string, unknown>,
  path: string,
): FieldResult<string | null> {
  const url = raw.url;
  const serverUrl = raw.serverUrl;
  const urlSet = url !== undefined && url !== null;
  const serverUrlSet = serverUrl !== undefined && serverUrl !== null;
  if (urlSet && serverUrlSet) {
    return {
      error: issue(
        joinPath(path, "url") || "url",
        "Set url or serverUrl, not both.",
        VALID_URL,
      ),
    };
  }
  const value = urlSet ? url : serverUrl;
  if (value === undefined || value === null) return { value: null };
  const field = urlSet
    ? joinPath(path, "url") || "url"
    : joinPath(path, "serverUrl") || "serverUrl";
  return typeof value === "string"
    ? { value }
    : { error: issue(field, "url must be a string.", VALID_URL) };
}

function readEnvironment(
  raw: Record<string, unknown>,
  path: string,
): FieldResult<{ env: string[]; envFrom: string[] }> {
  const value = raw.env;
  const field = joinPath(path, "env") || "env";
  if (value === undefined || value === null) {
    return { value: { env: [], envFrom: [] } };
  }
  if (Array.isArray(value)) {
    const names = stringArray(value, field, "Environment");
    if ("error" in names) return names;
    return { value: { env: names.value, envFrom: [] } };
  }
  if (!isRecord(value)) {
    return {
      error: issue(
        field,
        "Environment is an object of names to strings, or an array of names.",
        VALID_ENV_OBJECT,
      ),
    };
  }
  const env: string[] = [];
  const envFrom: string[] = [];
  for (const [key, item] of Object.entries(value)) {
    const itemPath = joinPath(field, key);
    if (typeof item !== "string") {
      return {
        error: issue(
          itemPath,
          "Environment values are strings.",
          'Use a text value such as "debug".',
        ),
      };
    }
    const placeholder = parsePlaceholder(item.trim());
    if (placeholder?.kind === "env" && placeholder.name === key) {
      envFrom.push(key);
    } else {
      env.push(key);
    }
  }
  return { value: { env, envFrom } };
}

function bearerEnvironmentFromHeaders(
  value: unknown,
  path: string,
): FieldResult<string | null> {
  const field = joinPath(path, "headers") || "headers";
  if (value === undefined || value === null) return { value: null };
  if (!isRecord(value)) {
    return {
      error: issue(
        field,
        "HTTP headers must be a JSON object.",
        'A valid value looks like { "Authorization": "Bearer ${env:TOKEN}" }.',
      ),
    };
  }
  const entries = Object.entries(value);
  if (entries.length === 0) return { value: null };
  if (
    entries.length !== 1 ||
    entries[0]?.[0].toLowerCase() !== "authorization"
  ) {
    return {
      error: issue(
        field,
        "Custom HTTP headers are not supported.",
        "Add this server manually, or keep a single Authorization bearer header.",
      ),
    };
  }
  const authorization = entries[0][1];
  const authorizationPath = joinPath(field, "Authorization");
  if (typeof authorization !== "string") {
    return {
      error: issue(
        authorizationPath,
        "The Authorization header must be a string.",
        VALID_BEARER,
      ),
    };
  }
  const bearer = authorization.match(/^Bearer\s+(.+)$/i);
  const token = (bearer?.[1] ?? authorization).trim();
  const placeholder = parsePlaceholder(token);
  if (placeholder !== null) return { value: placeholder.name };
  return {
    error: issue(
      authorizationPath,
      "Authorization uses a bearer environment variable, not a saved token value.",
      VALID_BEARER,
    ),
  };
}

function parsePlaceholder(
  value: string,
): { kind: "env" | "input"; name: string } | null {
  const env = value.match(
    /^(?:\$\{(?:env:)?([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*))$/,
  );
  const envName = env?.[1] ?? env?.[2];
  if (envName) return { kind: "env", name: envName };
  const input = value.match(/^\$\{input:([A-Za-z0-9_-]+)\}$/);
  return input?.[1] ? { kind: "input", name: input[1] } : null;
}

function nullableString(
  raw: Record<string, unknown>,
  field: string,
  path: string,
): FieldResult<string | null> {
  const value = raw[field];
  if (value === undefined || value === null) return { value: null };
  return typeof value === "string"
    ? { value }
    : {
        error: issue(
          joinPath(path, field) || field,
          `${field} must be a string.`,
          "Use a text value.",
        ),
      };
}

function stringArray(
  value: unknown,
  path: string,
  label: string,
): FieldResult<string[]> {
  if (value === undefined || value === null) return { value: [] };
  if (
    !Array.isArray(value) ||
    !value.every((item) => typeof item === "string")
  ) {
    return {
      error: issue(
        path,
        `${label} must be an array of strings.`,
        'A valid value looks like ["-y", "package"].',
      ),
    };
  }
  return { value: value as string[] };
}

function booleanField(
  raw: Record<string, unknown>,
  field: string,
  fallback: boolean,
  path: string,
): FieldResult<boolean> {
  const value = raw[field];
  if (value === undefined || value === null) return { value: fallback };
  return typeof value === "boolean"
    ? { value }
    : {
        error: issue(
          joinPath(path, field) || field,
          `${field} must be true or false.`,
          "Use true or false.",
        ),
      };
}

function numberField(
  raw: Record<string, unknown>,
  field: string,
  fallback: number,
  path: string,
): FieldResult<number> {
  const value = raw[field];
  if (value === undefined || value === null) return { value: fallback };
  return typeof value === "number"
    ? { value }
    : {
        error: issue(
          joinPath(path, field) || field,
          `${field} must be a number.`,
          "A valid value looks like 60000.",
        ),
      };
}

function validateEnvironmentNames(
  names: string[],
  path: string,
): string | null {
  const invalid = names.find(
    (name) =>
      name.length === 0 ||
      name.length > MAX_ENVIRONMENT_NAME_BYTES ||
      name.includes("=") ||
      name.includes("\0"),
  );
  return invalid === undefined
    ? null
    : issue(
        path,
        `Environment variable name ${JSON.stringify(clip(invalid))} is invalid.`,
        "Use a name without = or NUL, up to 256 characters.",
      );
}

function validateUrl(
  path: string,
  value: string,
  hasCredentials: boolean,
): string | null {
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    return issue(
      path,
      "Use an http or https URL without credentials.",
      VALID_URL,
    );
  }
  if (!["http:", "https:"].includes(parsed.protocol)) {
    return issue(
      path,
      "Use an http or https URL without credentials.",
      VALID_URL,
    );
  }
  if (parsed.username !== "" || parsed.password !== "") {
    return issue(
      path,
      "Do not put credentials in the URL.",
      "Set a bearer token variable instead.",
    );
  }
  if (parsed.hostname === "") {
    return issue(path, "Name a host in the URL.", VALID_URL);
  }
  if (
    hasCredentials &&
    parsed.protocol === "http:" &&
    !isLiteralLoopbackHost(parsed.hostname)
  ) {
    return issue(
      path,
      "Credentialed URLs use https unless they name a literal loopback address.",
      "Use an https URL, or a loopback http URL such as http://127.0.0.1:8080/mcp.",
    );
  }
  return null;
}

function isLiteralLoopbackHost(host: string): boolean {
  const bare =
    host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
  if (bare === "::1" || bare === "0:0:0:0:0:0:0:1") return true;
  const ipv4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(bare);
  if (ipv4 === null) return false;
  const octets = ipv4.slice(1).map(Number);
  return octets.every((octet) => octet <= 255) && octets[0] === 127;
}

function validNamespace(name: string): boolean {
  return (
    name.length > 0 &&
    name.length <= MAX_NAMESPACE_BYTES &&
    /^[A-Za-z0-9_-]+$/.test(name)
  );
}

function validGatewayEndpoint(slug: string): boolean {
  return slug.length > 0 && slug.length <= 127 && /^[A-Za-z0-9_-]+$/.test(slug);
}

function unique(values: string[]): string[] {
  return [...new Set(values)];
}

function namespaceIssue(path: string, name: string): string {
  const rule =
    "Name a server with 1–32 ASCII letters, numbers, underscores, or hyphens.";
  if (name.length === 0) {
    return issue(path, `${rule} The name is empty.`, VALID_NAMESPACE);
  }
  if (name.length > MAX_NAMESPACE_BYTES) {
    return issue(
      path,
      `${rule} This name is longer than ${MAX_NAMESPACE_BYTES} characters.`,
      VALID_NAMESPACE,
    );
  }
  const match = name.match(/[^A-Za-z0-9_-]/);
  if (match) {
    const char = match[0];
    const label =
      char === " "
        ? "a space"
        : char === "/"
          ? "a slash"
          : char === "."
            ? "a period"
            : JSON.stringify(char);
    return issue(
      path,
      `${rule} ${JSON.stringify(clip(name))} contains ${label}.`,
      VALID_NAMESPACE,
    );
  }
  return issue(path, rule, VALID_NAMESPACE);
}

function issue(path: string, rule: string, expected: string): string {
  const prefix = path === "" ? "" : `${path}: `;
  return `${prefix}${rule} ${expected}`;
}

export function mcpJsonPath(parts: Array<string | number>): string {
  let result = "";
  for (const part of parts) {
    if (typeof part === "number") {
      result += `[${part}]`;
      continue;
    }
    const ident = /^[A-Za-z_][A-Za-z0-9_]*$/.test(part);
    if (result === "") {
      result = ident ? part : `[${JSON.stringify(part)}]`;
    } else if (ident) {
      result += `.${part}`;
    } else {
      result += `[${JSON.stringify(part)}]`;
    }
  }
  return result;
}

function joinPath(base: string, field: string): string {
  if (base === "") return field;
  return /^[A-Za-z_][A-Za-z0-9_]*$/.test(field)
    ? `${base}.${field}`
    : `${base}[${JSON.stringify(field)}]`;
}

function clip(value: string): string {
  return value.length > 40 ? `${value.slice(0, 40)}…` : value;
}

function jsonSyntaxMessage(text: string, error: unknown): string {
  const scanned = scanNonStrictJson(text);
  if (scanned?.kind === "comment") {
    return `JSON at character ${scanned.index}: this file uses a comment. Use strict JSON without comments or trailing commas.`;
  }
  if (scanned?.kind === "trailing-comma") {
    return `JSON at character ${scanned.index}: this file uses a trailing comma. Use strict JSON without comments or trailing commas.`;
  }
  const position = jsonErrorPosition(error);
  if (position !== null) {
    return `JSON at character ${position}: this file is not valid JSON. Use strict JSON without comments or trailing commas.`;
  }
  return "This file is not valid JSON. Use strict JSON without comments or trailing commas.";
}

function jsonErrorPosition(error: unknown): number | null {
  if (!(error instanceof Error)) return null;
  const match = error.message.match(/position\s+(\d+)/i);
  return match ? Number(match[1]) : null;
}

function scanNonStrictJson(
  text: string,
): { kind: "comment" | "trailing-comma"; index: number } | null {
  let inString = false;
  let escape = false;
  for (let index = 0; index < text.length; index += 1) {
    const char = text[index];
    if (inString) {
      if (escape) {
        escape = false;
        continue;
      }
      if (char === "\\") {
        escape = true;
        continue;
      }
      if (char === '"') inString = false;
      continue;
    }
    if (char === '"') {
      inString = true;
      continue;
    }
    if (char === "/" && (text[index + 1] === "/" || text[index + 1] === "*")) {
      return { kind: "comment", index };
    }
    if (char === ",") {
      let cursor = index + 1;
      while (cursor < text.length && /[ \t\r\n]/.test(text[cursor] ?? "")) {
        cursor += 1;
      }
      if (text[cursor] === "}" || text[cursor] === "]") {
        return { kind: "trailing-comma", index };
      }
    }
  }
  return null;
}
