import { describe, expect, it } from "vitest";
import type { McpServerInfo } from "../api";
import { parseMcpImport, parseMcpImportText } from "./mcpImport";

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

function importJson(
  text: string,
  existingServers: readonly McpServerInfo[] = [],
) {
  return parseMcpImportText(text, existingServers);
}

function importedNames(text: string): string[] {
  return importJson(text).servers.map((server) => server.name);
}

describe("Claude Desktop mcpServers", () => {
  it.each([
    [
      "a stdio server with args and env names",
      `{
        "mcpServers": {
          "docs": {
            "command": "npx",
            "args": ["-y", "@example/docs-mcp"],
            "env": { "DOCS_TOKEN": "do-not-retain", "LOG_LEVEL": "debug" },
            "cwd": "/Users/alex/Code/docs"
          }
        }
      }`,
      {
        name: "docs",
        command: "npx",
        args: ["-y", "@example/docs-mcp"],
        env: ["DOCS_TOKEN", "LOG_LEVEL"],
        env_from: [],
        cwd: "/Users/alex/Code/docs",
        url: null,
        bearer_token_env: null,
      },
    ],
    [
      "an explicit stdio type",
      `{
        "mcpServers": {
          "fs": { "type": "stdio", "command": "npx", "args": ["-y", "fs"] }
        }
      }`,
      {
        name: "fs",
        command: "npx",
        args: ["-y", "fs"],
        url: null,
      },
    ],
    [
      "a command argv array",
      `{
        "mcpServers": {
          "docs": { "command": ["npx", "-y", "@example/docs-mcp"] }
        }
      }`,
      {
        name: "docs",
        command: "npx",
        args: ["-y", "@example/docs-mcp"],
      },
    ],
  ] as const)("imports %s", (_label, json, expected) => {
    const result = importJson(json);
    expect(result.skipped).toEqual([]);
    expect(result.servers[0]).toEqual(expect.objectContaining(expected));
    expect(JSON.stringify(result)).not.toContain("do-not-retain");
    expect(JSON.stringify(result)).not.toContain("debug");
  });
});

describe("Cursor and Windsurf mcpServers HTTP", () => {
  it.each([
    [
      "a url without a type",
      `{
        "mcpServers": {
          "remote": {
            "url": "https://mcp.example.test/tools",
            "headers": { "Authorization": "Bearer \${REMOTE_TOKEN}" }
          }
        }
      }`,
      {
        name: "remote",
        command: null,
        url: "https://mcp.example.test/tools",
        bearer_token_env: "REMOTE_TOKEN",
      },
    ],
    [
      "type http",
      `{
        "mcpServers": {
          "remote": {
            "type": "http",
            "url": "https://mcp.example.test/tools?x=1"
          }
        }
      }`,
      {
        name: "remote",
        url: "https://mcp.example.test/tools?x=1",
        bearer_token_env: null,
      },
    ],
    [
      "type sse",
      `{
        "mcpServers": {
          "remote": {
            "type": "sse",
            "url": "https://mcp.example.test/sse/"
          }
        }
      }`,
      {
        name: "remote",
        url: "https://mcp.example.test/sse/",
      },
    ],
    [
      "type streamable-http",
      `{
        "mcpServers": {
          "remote": {
            "type": "streamable-http",
            "url": "https://mcp.example.test/mcp/"
          }
        }
      }`,
      {
        name: "remote",
        url: "https://mcp.example.test/mcp/",
      },
    ],
    [
      "a serverUrl alias and env placeholder header",
      `{
        "mcpServers": {
          "remote": {
            "serverUrl": "https://mcp.example.test/tools",
            "headers": { "Authorization": "Bearer \${env:GATEWAY_TOKEN}" }
          }
        }
      }`,
      {
        name: "remote",
        url: "https://mcp.example.test/tools",
        bearer_token_env: "GATEWAY_TOKEN",
      },
    ],
  ] as const)("imports %s", (_label, json, expected) => {
    const result = importJson(json);
    expect(result.skipped).toEqual([]);
    expect(result.servers[0]).toEqual(expect.objectContaining(expected));
  });
});

describe("VS Code servers", () => {
  it.each([
    [
      "a stdio object with inputs",
      `{
        "inputs": [
          {
            "type": "promptString",
            "id": "docs-token",
            "description": "Docs token",
            "password": true
          }
        ],
        "servers": {
          "docs": {
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "@example/docs-mcp"],
            "env": { "DOCS_TOKEN": "\${input:docs-token}" }
          }
        }
      }`,
      {
        name: "docs",
        command: "npx",
        args: ["-y", "@example/docs-mcp"],
        env: ["DOCS_TOKEN"],
        url: null,
      },
    ],
    [
      "an http object",
      `{
        "servers": {
          "remote": {
            "type": "http",
            "url": "https://mcp.example.test/mcp"
          }
        }
      }`,
      {
        name: "remote",
        command: null,
        url: "https://mcp.example.test/mcp",
      },
    ],
    [
      "settings.json nested under mcp.servers",
      `{
        "mcp": {
          "servers": {
            "docs": { "command": "npx", "args": ["-y", "docs"] }
          }
        }
      }`,
      {
        name: "docs",
        command: "npx",
        args: ["-y", "docs"],
      },
    ],
  ] as const)("imports %s", (_label, json, expected) => {
    const result = importJson(json);
    expect(result.skipped).toEqual([]);
    expect(result.servers[0]).toEqual(expect.objectContaining(expected));
  });
});

describe("bare servers and arrays", () => {
  it.each([
    [
      "a single stdio object",
      `{ "name": "docs", "command": "npx", "args": ["-y", "docs"] }`,
      ["docs"],
    ],
    [
      "a single HTTP object without a name",
      `{ "url": "https://mcp.example.test/mcp" }`,
      ["imported"],
    ],
    [
      "a top-level array of stdio and HTTP servers",
      `[
        { "command": "npx", "args": ["-y", "docs"] },
        { "url": "https://mcp.example.test/mcp" }
      ]`,
      ["imported_1", "imported_2"],
    ],
    [
      "a Tidebreak servers array",
      `{
        "servers": [
          { "name": "docs", "command": "/opt/mcp/docs" },
          { "name": "remote", "url": "https://mcp.example.test/mcp" }
        ]
      }`,
      ["docs", "remote"],
    ],
  ] as const)("imports %s", (_label, json, names) => {
    expect(importedNames(json)).toEqual([...names]);
  });
});

describe("placeholders and env forwarding", () => {
  it.each([
    [
      "${env:VAR} matching the env key",
      `{
        "mcpServers": {
          "docs": {
            "command": "npx",
            "env": { "DOCS_TOKEN": "\${env:DOCS_TOKEN}" }
          }
        }
      }`,
      { env: [], env_from: ["DOCS_TOKEN"] },
    ],
    [
      "${VAR} matching the env key",
      `{
        "mcpServers": {
          "docs": {
            "command": "npx",
            "env": { "DOCS_TOKEN": "\${DOCS_TOKEN}" }
          }
        }
      }`,
      { env: [], env_from: ["DOCS_TOKEN"] },
    ],
    [
      "$VAR matching the env key",
      `{
        "mcpServers": {
          "docs": {
            "command": "npx",
            "env": { "DOCS_TOKEN": "$DOCS_TOKEN" }
          }
        }
      }`,
      { env: [], env_from: ["DOCS_TOKEN"] },
    ],
  ] as const)("forwards %s", (_label, json, expected) => {
    const result = importJson(json);
    expect(result.skipped).toEqual([]);
    expect(result.servers[0]).toEqual(expect.objectContaining(expected));
    expect(result.secrets).toEqual([]);
  });
});

describe("parseMcpImport", () => {
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
    expect(result.skipped.map((item) => item.name)).toEqual([
      "existing",
      "bad.name",
      "broken",
    ]);
    expect(result.skipped[0]).toEqual({
      name: "existing",
      path: "mcpServers.existing",
      reason:
        "mcpServers.existing: Namespace already exists. Choose a different name.",
    });
    expect(result.skipped[1]?.reason).toContain('mcpServers["bad.name"]');
    expect(result.skipped[1]?.reason).toContain("a period");
    expect(result.skipped[2]?.reason).toContain("mcpServers.broken");
    expect(result.skipped[2]?.reason).toContain(
      "Configure exactly one command or URL.",
    );
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
        path: "servers[1].name",
        reason:
          "servers[1].name: Namespace appears more than once in this file. Choose a different name.",
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
    expect(result.skipped[0]?.reason).toContain(
      "mcpServers.literal.headers.Authorization",
    );
    expect(result.skipped[1]?.reason).toContain("mcpServers.custom.headers");
    expect(JSON.stringify(result)).not.toContain("do-not-retain");
    expect(JSON.stringify(result)).not.toContain("also-do-not-retain");
  });

  it("rejects files without a supported top-level shape", () => {
    expect(() => parseMcpImport({ tools: [] }, [])).toThrow(/mcpServers/);
    expect(() => parseMcpImport({ mcpServers: {} }, [])).toThrow(
      /contains no servers/,
    );
  });
});

describe("rejected JSON names the path and the rule", () => {
  it.each([
    [
      "JSON with a comment",
      `{
        // claude
        "mcpServers": { "docs": { "command": "npx" } }
      }`,
      /JSON at character \d+: this JSON uses a comment/,
    ],
    [
      "JSON with a trailing comma",
      `{
        "mcpServers": {
          "docs": { "command": "npx", }
        }
      }`,
      /JSON at character \d+: this JSON uses a trailing comma/,
    ],
    [
      "a non-string env value",
      `{
        "mcpServers": {
          "docs": { "command": "npx", "env": { "LOG_LEVEL": 1 } }
        }
      }`,
      /mcpServers\.docs\.env\.LOG_LEVEL: Environment values are strings/,
    ],
    [
      "a name with spaces",
      `{
        "mcpServers": {
          "my server": { "command": "npx" }
        }
      }`,
      /contains a space/,
    ],
    [
      "a name with slashes",
      `{
        "mcpServers": {
          "org\/docs": { "command": "npx" }
        }
      }`,
      /contains a slash/,
    ],
    [
      "stdio with a url",
      `{
        "mcpServers": {
          "docs": { "type": "stdio", "url": "https://mcp.example.test/mcp" }
        }
      }`,
      /mcpServers\.docs\.url: stdio servers set command, not url/,
    ],
    [
      "http without a url",
      `{
        "mcpServers": {
          "remote": { "type": "http", "command": "npx" }
        }
      }`,
      /mcpServers\.remote\.command: http and sse servers set url, not command/,
    ],
  ])("rejects %s", (_label, json, pattern) => {
    expect(() => {
      const result = importJson(json);
      if (result.servers.length === 0 && result.skipped.length > 0) {
        throw new Error(result.skipped[0]?.reason ?? "skipped");
      }
    }).toThrow(pattern);
  });

  it("keeps valid siblings when one entry fails", () => {
    const result = importJson(`{
      "mcpServers": {
        "docs": { "command": "npx" },
        "bad name": { "command": "npx" },
        "remote": { "url": "https://mcp.example.test/mcp" }
      }
    }`);
    expect(result.servers.map((server) => server.name)).toEqual([
      "docs",
      "remote",
    ]);
    expect(result.skipped).toHaveLength(1);
    expect(result.skipped[0]?.reason).toContain("a space");
    expect(result.skipped[0]?.path).toContain("bad name");
  });

  it("strips a leading BOM", () => {
    const result = importJson(
      `\ufeff{"mcpServers":{"docs":{"command":"npx"}}}`,
    );
    expect(result.servers.map((server) => server.name)).toEqual(["docs"]);
  });
});
