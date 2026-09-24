import { createServer } from "node:http";

import { listen } from "./process";

/**
 * A stand-in OpenAI-compatible server that only lists one model.
 *
 * First run connects it the way a person connects LM Studio or vLLM, so the
 * connection test and "Find models" read a real answer from loopback. Chat
 * completions never reach it: the scripted provider answers every turn.
 */
export async function serveModelList(
  model: string,
): Promise<{ baseUrl: string; close(): Promise<void> }> {
  const server = createServer((request, response) => {
    if (
      request.method === "GET" &&
      request.url?.split("?")[0] === "/v1/models"
    ) {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          object: "list",
          data: [{ id: model, object: "model" }],
        }),
      );
      return;
    }
    response.writeHead(404).end();
  });
  const port = await listen(server);
  return {
    baseUrl: `http://127.0.0.1:${port}/v1`,
    close: () => new Promise((resolve) => server.close(() => resolve())),
  };
}
