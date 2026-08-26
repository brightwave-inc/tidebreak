let clientGenerations = new WeakMap<object, number>();
let nextClientGeneration = 0;
let activeClientGeneration: number | null = null;

/** A stable identity for one ApiClient object. */
export function codeClientGeneration(client: object): number {
  const existing = clientGenerations.get(client);
  if (existing !== undefined) return existing;
  nextClientGeneration += 1;
  clientGenerations.set(client, nextClientGeneration);
  return nextClientGeneration;
}

/** Mark one ApiClient generation as the only Code authority allowed to write. */
export function activateCodeClientGeneration(client: object): {
  generation: number;
  changed: boolean;
} {
  const generation = codeClientGeneration(client);
  const changed = activeClientGeneration !== generation;
  activeClientGeneration = generation;
  return { generation, changed };
}

/** Standalone stores accept calls before AppShell establishes an authority. */
export function isCodeClientGenerationActive(generation: number): boolean {
  return (
    activeClientGeneration === null || activeClientGeneration === generation
  );
}

/** Test-only: restore the module to its pre-AppShell state. */
export function resetCodeClientGenerationForTests(): void {
  clientGenerations = new WeakMap<object, number>();
  nextClientGeneration = 0;
  activeClientGeneration = null;
}
