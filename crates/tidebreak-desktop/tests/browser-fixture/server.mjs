import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const fixtureRoot = dirname(fileURLToPath(import.meta.url));
const maxBodyBytes = 1024 * 1024;

function html(response, body, status = 200, headers = {}) {
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-type": "text/html; charset=utf-8",
    "x-content-type-options": "nosniff",
    ...headers,
  });
  response.end(body);
}

function json(response, value, status = 200, headers = {}) {
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-type": "application/json; charset=utf-8",
    "x-content-type-options": "nosniff",
    ...headers,
  });
  response.end(JSON.stringify(value));
}

function text(response, value, status = 200, headers = {}) {
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-type": "text/plain; charset=utf-8",
    "x-content-type-options": "nosniff",
    ...headers,
  });
  response.end(value);
}

async function requestBody(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > maxBodyBytes) {
      const error = new Error("request body is too large");
      error.statusCode = 413;
      throw error;
    }
    chunks.push(chunk);
  }
  return Buffer.concat(chunks);
}

function listen(server, host, port) {
  return new Promise((resolveListen, reject) => {
    server.once("error", reject);
    server.listen(port, host, () => {
      server.off("error", reject);
      resolveListen(server.address());
    });
  });
}

function close(server) {
  return new Promise((resolveClose, reject) => {
    server.close((error) => (error ? reject(error) : resolveClose()));
  });
}

async function fixtureFile(name) {
  return readFile(join(fixtureRoot, name), "utf8");
}

function fixtureDelay(url) {
  switch (url.searchParams.get("ms")) {
    case "0":
      return 0;
    case "5":
      return 5;
    case "25":
      return 25;
    case "100":
      return 100;
    case "500":
      return 500;
    case "1000":
      return 1_000;
    case "2000":
      return 2_000;
    default:
      return 250;
  }
}

function parseJsonBody(body) {
  try {
    return JSON.parse(body.toString("utf8"));
  } catch {
    return null;
  }
}

export async function startBrowserFixture({
  host = "127.0.0.1",
  port = 41_781,
  crossOriginPort = 41_782,
} = {}) {
  let items = [{ id: 1, label: "Inspect the preview" }];
  let nextItemId = 2;

  const crossOriginServer = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", "http://fixture.invalid");
    if (request.method === "GET" && url.pathname === "/cross-frame") {
      html(response, await fixtureFile("cross-frame.html"));
      return;
    }
    json(response, { error: "not_found" }, 404);
  });

  const crossAddress = await listen(crossOriginServer, host, crossOriginPort);
  if (!crossAddress || typeof crossAddress === "string") {
    await close(crossOriginServer);
    throw new Error("cross-origin fixture did not bind a TCP address");
  }
  const crossOrigin = `http://${host}:${crossAddress.port}`;

  const primaryServer = createServer(async (request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://fixture.invalid");

      if (request.method === "GET" && url.pathname === "/") {
        const source = await fixtureFile("index.html");
        html(response, source.replaceAll("__CROSS_ORIGIN__", crossOrigin));
        return;
      }
      if (request.method === "GET" && url.pathname === "/same-frame") {
        html(response, await fixtureFile("same-frame.html"));
        return;
      }
      if (request.method === "GET" && url.pathname === "/popup-target") {
        html(
          response,
          "<!doctype html><title>Popup target</title><main><h1>Popup target</h1><button id=popup-confirm>Confirm popup</button></main>",
        );
        return;
      }
      if (request.method === "GET" && url.pathname === "/redirect") {
        response.writeHead(302, {
          "cache-control": "no-store",
          location: "/redirected?from=redirect",
        });
        response.end();
        return;
      }
      if (request.method === "GET" && url.pathname === "/redirected") {
        html(
          response,
          "<!doctype html><title>Redirect complete</title><main><h1>Redirect complete</h1><p>Source: redirect</p></main>",
        );
        return;
      }
      if (request.method === "GET" && url.pathname === "/slow") {
        const waitedMs = fixtureDelay(url);
        await new Promise((resolveDelay) => setTimeout(resolveDelay, waitedMs));
        json(response, { status: "ready", waitedMs });
        return;
      }
      if (request.method === "GET" && url.pathname === "/api/items") {
        json(response, { items });
        return;
      }
      if (request.method === "POST" && url.pathname === "/api/items") {
        const body = parseJsonBody(await requestBody(request));
        const label = typeof body?.label === "string" ? body.label.trim() : "";
        if (!label) {
          json(response, { error: "label_required" }, 400);
          return;
        }
        const item = { id: nextItemId++, label: label.slice(0, 120) };
        items = [...items, item];
        json(response, { item }, 201);
        return;
      }
      if (request.method === "POST" && url.pathname === "/api/submit") {
        const body = await requestBody(request);
        const fields = Object.fromEntries(
          new URLSearchParams(body.toString("utf8")).entries(),
        );
        json(response, { status: "submitted", fields });
        return;
      }
      if (request.method === "POST" && url.pathname === "/upload") {
        const body = await requestBody(request);
        json(response, {
          status: "uploaded",
          bytes: body.length,
          contentType: request.headers["content-type"] ?? null,
        });
        return;
      }
      if (request.method === "POST" && url.pathname === "/reset") {
        items = [{ id: 1, label: "Inspect the preview" }];
        nextItemId = 2;
        json(response, { status: "reset" });
        return;
      }
      if (request.method === "GET" && url.pathname === "/download") {
        text(response, "browser fixture download\n", 200, {
          "content-disposition": 'attachment; filename="fixture-download.txt"',
        });
        return;
      }

      json(response, { error: "not_found" }, 404);
    } catch (error) {
      const status = Number.isInteger(error?.statusCode) ? error.statusCode : 500;
      json(response, { error: status === 413 ? "body_too_large" : "fixture_error" }, status);
    }
  });

  try {
    const primaryAddress = await listen(primaryServer, host, port);
    if (!primaryAddress || typeof primaryAddress === "string") {
      throw new Error("primary fixture did not bind a TCP address");
    }
    const origin = `http://${host}:${primaryAddress.port}`;
    return {
      origin,
      crossOrigin,
      async close() {
        await Promise.all([close(primaryServer), close(crossOriginServer)]);
      },
    };
  } catch (error) {
    await close(crossOriginServer);
    throw error;
  }
}

function fixtureOptions(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--port" || argument === "--cross-origin-port") {
      const value = Number(argv[index + 1]);
      if (!Number.isInteger(value) || value < 0 || value > 65_535) {
        throw new Error(`${argument} must be an integer from 0 through 65535`);
      }
      if (argument === "--port") options.port = value;
      else options.crossOriginPort = value;
      index += 1;
      continue;
    }
    throw new Error(`unknown fixture argument: ${argument}`);
  }
  return options;
}

const invokedPath = process.argv[1]
  ? pathToFileURL(resolve(process.argv[1])).href
  : null;

if (invokedPath === import.meta.url) {
  const fixture = await startBrowserFixture(fixtureOptions(process.argv.slice(2)));
  process.stdout.write(
    `${JSON.stringify({ origin: fixture.origin, crossOrigin: fixture.crossOrigin })}\n`,
  );

  let closing = false;
  const shutdown = async () => {
    if (closing) return;
    closing = true;
    await fixture.close();
  };
  process.once("SIGINT", () => void shutdown());
  process.once("SIGTERM", () => void shutdown());
}
