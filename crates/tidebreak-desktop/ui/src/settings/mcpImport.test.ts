import { describe, expect, it } from "vitest";
import type { McpServerInfo } from "../api";
import { parseMcpImport } from "./mcpImport";

function existing(name: string): McpServerInfo {
  return {
    name,
    command: "/opt/mcp/server",
    args: [],
    env: [],
    env_from: [],
    cwd: null,
    url: null,
    bearer_token_env: null,
    gateway_endpoint: null,
    request_timeout_ms: 60_000,
    enabled: true,
    plugin: null,
    health: "healthy",
    tool_count: 1,
    diagnostic: null,
    curated: null,
  };
}

describe("parseMcpImport", () => {
  it("imports Claude and Cursor servers without retaining environment values", () => {
    const result = parseMcpImport(
      {
        mcpServers: {
          docs: {
            command: "npx",
            args: ["-y", "@example/docs-mcp"],
            env: { DOCS_TOKEN: "do-not-retain", LOG_LEVEL: "debug" },
            cwd: "/Users/alex/Code/docs",
          },
          remote: {
            url: "https://mcp.example.test/tools",
            headers: { Authorization: "Bearer ${REMOTE_TOKEN}" },
          },
        },
      },
      [],
    );

    expect(result.skipped).toEqual([]);
    expect(result.servers).toEqual([
      expect.objectContaining({
        name: "docs",
        command: "npx",
        args: ["-y", "@example/docs-mcp"],
        env: ["DOCS_TOKEN", "LOG_LEVEL"],
        cwd: "/Users/alex/Code/docs",
        url: null,
      }),
      expect.objectContaining({
        name: "remote",
        command: null,
        args: [],
        env: [],
        cwd: null,
        url: "https://mcp.example.test/tools",
        bearer_token_env: "REMOTE_TOKEN",
      }),
    ]);
    expect(result.secrets).toEqual([
      { server: "docs", name: "DOCS_TOKEN" },
      { server: "docs", name: "LOG_LEVEL" },
    ]);
    expect(JSON.stringify(result)).not.toContain("do-not-retain");
    expect(JSON.stringify(result)).not.toContain("debug");
  });

  it("imports Tidebreak fields and drops inbound secret values", () => {
    const result = parseMcpImport(
      {
        servers: [
          {
            name: "private_docs",
            command: "/opt/mcp/docs",
            args: ["--stdio"],
            env: ["DOCS_TOKEN"],
            env_values: {
              DOCS_TOKEN: "do-not-retain",
              SEARCH_TOKEN: "also-do-not-retain",
            },
            env_from: ["LOG_LEVEL"],
            cwd: "/tmp/docs",
            request_timeout_ms: 90_000,
            enabled: false,
          },
        ],
      },
      [],
    );

    expect(result.servers).toEqual([
      expect.objectContaining({
        name: "private_docs",
        command: "/opt/mcp/docs",
        args: ["--stdio"],
        env: ["DOCS_TOKEN", "SEARCH_TOKEN"],
        env_from: ["LOG_LEVEL"],
        cwd: "/tmp/docs",
        request_timeout_ms: 90_000,
        enabled: false,
      }),
    ]);
    expect(JSON.stringify(result)).not.toContain("do-not-retain");
    expect(JSON.stringify(result)).not.toContain("also-do-not-retain");
  });

  it("skips existing, repeated, invalid, and malformed namespaces", () => {
    const result = parseMcpImport(
      {
        mcpServers: {
          existing: { command: "existing-server" },
          "bad.name": { command: "bad-server" },
          valid: { command: "first-server" },
          broken: { command: "", url: "https://mcp.example.test" },
        },
      },
      [existing("existing")],
    );

    expect(result.servers.map((server) => server.name)).toEqual(["valid"]);
    expect(result.skipped).toEqual([
      { name: "existing", reason: "Namespace already exists." },
      {
        name: "bad.name",
        reason:
          "Namespace must use 1–32 ASCII letters, numbers, underscores, or hyphens.",
      },
      { name: "broken", reason: "Configure exactly one command or URL." },
    ]);
  });

  it("imports the first Tidebreak server when a namespace repeats", () => {
    const result = parseMcpImport(
      {
        servers: [
          { name: "calendar", command: "first-server" },
          { name: "calendar", command: "second-server" },
        ],
      },
      [],
    );

    expect(result.servers).toEqual([
      expect.objectContaining({ name: "calendar", command: "first-server" }),
    ]);
    expect(result.skipped).toEqual([
      {
        name: "calendar",
        reason: "Namespace appears more than once in this file.",
      },
    ]);
  });

  it("rejects custom or literal HTTP headers without retaining their values", () => {
    const result = parseMcpImport(
      {
        mcpServers: {
          literal: {
            url: "https://mcp.example.test/literal",
            headers: { Authorization: "Bearer do-not-retain" },
          },
          custom: {
            url: "https://mcp.example.test/custom",
            headers: { "X-API-Key": "also-do-not-retain" },
          },
        },
      },
      [],
    );

    expect(result.servers).toEqual([]);
    expect(result.skipped.map((item) => item.name)).toEqual([
      "literal",
      "custom",
    ]);
    expect(JSON.stringify(result)).not.toContain("do-not-retain");
    expect(JSON.stringify(result)).not.toContain("also-do-not-retain");
  });

  it("rejects files without a supported top-level shape", () => {
    expect(() => parseMcpImport({ tools: [] }, [])).toThrow(
      /Tidebreak.*Claude\/Cursor/,
    );
  });
});
