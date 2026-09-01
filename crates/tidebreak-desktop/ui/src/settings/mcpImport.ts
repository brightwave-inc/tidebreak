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

type ImportShape = "tidebreak" | "common";

type SourceEntry = {
  fallbackName: string;
  raw: unknown;
  shape: ImportShape;
};

export type McpImportSkip = {
  name: string;
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
          : "";
    const label = rawName || `Server ${index + 1}`;

    if (!validNamespace(rawName)) {
      skipped.push({
        name: label,
        reason:
          "Namespace must use 1–32 ASCII letters, numbers, underscores, or hyphens.",
      });
      continue;
    }
    if (seen.has(rawName)) {
      skipped.push({
        name: rawName,
        reason: "Namespace appears more than once in this file.",
      });
      continue;
    }
    seen.add(rawName);
    if (taken.has(rawName)) {
      skipped.push({ name: rawName, reason: "Namespace already exists." });
      continue;
    }
    if (servers.length >= remaining) {
      skipped.push({
        name: rawName,
        reason: `Tidebreak supports up to ${MAX_SERVERS} configured servers.`,
      });
      continue;
    }

    const parsed = parseEntry(rawName, entry.raw, entry.shape);
    if ("error" in parsed) {
      skipped.push({ name: rawName, reason: parsed.error });
      continue;
    }
    servers.push(parsed.value.server);
    secrets.push(...parsed.value.secrets);
    taken.add(rawName);
  }

  return { servers, skipped, secrets };
}

function sourceEntries(value: unknown): SourceEntry[] {
  if (!isRecord(value)) {
    throw new Error("The file must contain a JSON object.");
  }
  if (Array.isArray(value.servers)) {
    return value.servers.map((raw, index) => ({
      fallbackName: `Server ${index + 1}`,
      raw,
      shape: "tidebreak",
    }));
  }
  if (isRecord(value.mcpServers)) {
    return Object.entries(value.mcpServers).map(([name, raw]) => ({
      fallbackName: name,
      raw,
      shape: "common",
    }));
  }
  throw new Error(
    'Use a Tidebreak {"servers": [...]} file or a Claude/Cursor {"mcpServers": {...}} file.',
  );
}

function parseEntry(
  name: string,
  raw: unknown,
  shape: ImportShape,
): FieldResult<ParsedEntry> {
  if (!isRecord(raw)) return { error: "Server definition must be an object." };

  const command = nullableString(raw, "command");
  if ("error" in command) return command;
  const url = nullableString(raw, "url");
  if ("error" in url) return url;
  const gateway =
    shape === "tidebreak"
      ? nullableString(raw, "gateway_endpoint")
      : { value: null };
  if ("error" in gateway) return gateway;

  const transports = [command.value, url.value, gateway.value].filter(
    (candidate) => candidate !== null,
  );
  if (transports.length !== 1) {
    return {
      error:
        shape === "tidebreak"
          ? "Configure exactly one command, URL, or gateway endpoint."
          : "Configure exactly one command or URL.",
    };
  }

  const args = stringArray(raw.args, "Arguments");
  if ("error" in args) return args;
  if (args.value.length > MAX_ARGS) {
    return { error: `Arguments must contain at most ${MAX_ARGS} items.` };
  }

  const directEnvironment = environmentNames(raw.env, "Environment");
  if ("error" in directEnvironment) return directEnvironment;
  const forwardedEnvironment = stringArray(
    raw.env_from,
    "Forwarded environment",
  );
  if ("error" in forwardedEnvironment) return forwardedEnvironment;

  let secretValueNames: string[] = [];
  if (raw.env_values !== undefined && raw.env_values !== null) {
    if (!isRecord(raw.env_values)) {
      return { error: "Environment values must be a JSON object." };
    }
    secretValueNames = Object.keys(raw.env_values);
  }
  const env = unique([...directEnvironment.value, ...secretValueNames]);
  const envFrom = forwardedEnvironment.value;
  if (env.length + envFrom.length > MAX_ENVIRONMENT_VARIABLES) {
    return {
      error: `Environment must contain at most ${MAX_ENVIRONMENT_VARIABLES} names.`,
    };
  }
  const environmentError = validateEnvironmentNames([...env, ...envFrom]);
  if (environmentError !== null) return { error: environmentError };
  if (new Set([...env, ...envFrom]).size !== env.length + envFrom.length) {
    return {
      error: "An environment variable name is configured more than once.",
    };
  }

  const cwd = nullableString(raw, "cwd");
  if ("error" in cwd) return cwd;
  const bearer = nullableString(raw, "bearer_token_env");
  if ("error" in bearer) return bearer;
  const headerBearer =
    shape === "common"
      ? bearerEnvironmentFromHeaders(raw.headers)
      : { value: null };
  if ("error" in headerBearer) return headerBearer;
  if (
    bearer.value !== null &&
    headerBearer.value !== null &&
    bearer.value !== headerBearer.value
  ) {
    return { error: "Bearer token variables are configured more than once." };
  }
  const bearerEnvironment = bearer.value ?? headerBearer.value;
  if (bearerEnvironment !== null) {
    const bearerError = validateEnvironmentNames([bearerEnvironment]);
    if (bearerError !== null) return { error: bearerError };
  }

  const enabled = booleanField(raw, "enabled", true);
  if ("error" in enabled) return enabled;
  const timeout = numberField(
    raw,
    "request_timeout_ms",
    DEFAULT_REQUEST_TIMEOUT_MS,
  );
  if ("error" in timeout) return timeout;
  if (
    !Number.isInteger(timeout.value) ||
    timeout.value < 1 ||
    timeout.value > MAX_REQUEST_TIMEOUT_MS
  ) {
    return {
      error: `Request timeout must be a whole number from 1 to ${MAX_REQUEST_TIMEOUT_MS.toLocaleString()}.`,
    };
  }

  if (command.value !== null) {
    if (command.value.length === 0) {
      return { error: "Command must not be empty." };
    }
    if (bearerEnvironment !== null) {
      return { error: "Bearer token variables apply only to URL servers." };
    }
  } else if (
    args.value.length > 0 ||
    env.length > 0 ||
    envFrom.length > 0 ||
    cwd.value !== null
  ) {
    return {
      error:
        "Arguments, environment, and working directory apply only to command servers.",
    };
  }

  if (url.value !== null) {
    const urlError = validateUrl(url.value);
    if (urlError !== null) return { error: urlError };
  } else if (bearerEnvironment !== null) {
    return { error: "Bearer token variables apply only to URL servers." };
  }

  if (gateway.value !== null && !validGatewayEndpoint(gateway.value)) {
    return {
      error:
        "Gateway endpoint must use 1–127 ASCII letters, numbers, underscores, or hyphens.",
    };
  }

  const processStrings = [
    command.value,
    url.value,
    cwd.value,
    ...args.value,
  ].filter((item): item is string => item !== null);
  if (
    processStrings.some(
      (item) => item.length > MAX_PROCESS_STRING_BYTES || item.includes("\0"),
    )
  ) {
    return {
      error:
        "Command, URL, arguments, and working directory must be valid text.",
    };
  }

  return {
    value: {
      server: {
        name,
        command: command.value,
        args: args.value,
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

function bearerEnvironmentFromHeaders(
  value: unknown,
): FieldResult<string | null> {
  if (value === undefined || value === null) return { value: null };
  if (!isRecord(value)) return { error: "HTTP headers must be a JSON object." };
  const entries = Object.entries(value);
  if (entries.length === 0) return { value: null };
  if (
    entries.length !== 1 ||
    entries[0]?.[0].toLowerCase() !== "authorization"
  ) {
    return {
      error: "Custom HTTP headers are not supported. Add this server manually.",
    };
  }
  const authorization = entries[0][1];
  if (typeof authorization !== "string") {
    return { error: "The Authorization header must be a string." };
  }
  const match = authorization.match(
    /^Bearer\s+(?:\$\{(?:env:)?([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*))$/,
  );
  const environment = match?.[1] ?? match?.[2];
  return environment
    ? { value: environment }
    : {
        error:
          "Authorization must use a bearer environment variable, not a saved token value.",
      };
}

function nullableString(
  raw: Record<string, unknown>,
  field: string,
): FieldResult<string | null> {
  const value = raw[field];
  if (value === undefined || value === null) return { value: null };
  return typeof value === "string"
    ? { value }
    : { error: `${field} must be a string.` };
}

function stringArray(value: unknown, label: string): FieldResult<string[]> {
  if (value === undefined || value === null) return { value: [] };
  if (
    !Array.isArray(value) ||
    !value.every((item) => typeof item === "string")
  ) {
    return { error: `${label} must be an array of strings.` };
  }
  return { value: value as string[] };
}

function environmentNames(
  value: unknown,
  label: string,
): FieldResult<string[]> {
  if (isRecord(value)) return { value: Object.keys(value) };
  return stringArray(value, label);
}

function booleanField(
  raw: Record<string, unknown>,
  field: string,
  fallback: boolean,
): FieldResult<boolean> {
  const value = raw[field];
  if (value === undefined || value === null) return { value: fallback };
  return typeof value === "boolean"
    ? { value }
    : { error: `${field} must be true or false.` };
}

function numberField(
  raw: Record<string, unknown>,
  field: string,
  fallback: number,
): FieldResult<number> {
  const value = raw[field];
  if (value === undefined || value === null) return { value: fallback };
  return typeof value === "number"
    ? { value }
    : { error: `${field} must be a number.` };
}

function validateEnvironmentNames(names: string[]): string | null {
  const invalid = names.find(
    (name) =>
      name.length === 0 ||
      name.length > MAX_ENVIRONMENT_NAME_BYTES ||
      name.includes("=") ||
      name.includes("\0"),
  );
  return invalid === undefined
    ? null
    : `Environment variable name ${JSON.stringify(invalid)} is invalid.`;
}

function validateUrl(value: string): string | null {
  try {
    const parsed = new URL(value);
    if (!["http:", "https:"].includes(parsed.protocol)) {
      return "URL must use http or https.";
    }
    if (parsed.username !== "" || parsed.password !== "") {
      return "URL must not contain credentials.";
    }
    if (parsed.hostname === "") return "URL must name a host.";
    return null;
  } catch {
    return "URL must be a valid HTTP or HTTPS URL.";
  }
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


