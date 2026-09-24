import type { Machine } from "./machine";

/**
 * Arrange-step calls against the machine's own API, as the administrator.
 *
 * A flow uses these only to reach its starting state. Everything the flow is
 * about happens through the page.
 */
async function call<T>(
  machine: Machine,
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const response = await fetch(`${machine.url}${path}`, {
    method,
    headers: {
      authorization: `Bearer ${machine.token}`,
      accept: "application/json",
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(
      `${method} ${path} answered ${response.status}: ${await response.text()}`,
    );
  }
  return (await response.json()) as T;
}

/**
 * Give the machine a model the Work composer can pick.
 *
 * The model is a custom one on an OpenAI-compatible endpoint that nothing
 * listens on: the scripted provider answers every turn, so the endpoint is
 * never asked for a completion. The model only has to exist for the
 * composer to let a message go.
 */
export async function connectScriptedModel(machine: Machine): Promise<void> {
  await call(machine, "PUT", "/providers/openai_compatible", {
    enabled: true,
    base_url: "http://127.0.0.1:9/v1",
    models: [{ id: "e2e-model", display_name: "Scripted model" }],
  });
}

/** The repositories the administrator has on the machine. */
export async function listRepositories(
  machine: Machine,
): Promise<{ id: string; display_name: string; root_path: string }[]> {
  return await call(machine, "GET", "/code/repos");
}

/** Register a checkout on the machine's own disk, the way a desktop registers a local folder. */
export async function registerRepository(
  machine: Machine,
  path: string,
): Promise<{ id: string; display_name: string }> {
  return await call(machine, "POST", "/code/repos", { path });
}
