import type { CodeCloneRequest } from "./CodeUpdatesStore";

/**
 * What one typed line in the inline add-repo field asks for.
 *
 * The add-repo palette asks the source first and then shows the matching
 * form. The new-workspace composer has room for one field, so the field
 * reads the answer out of the value instead: a path registers a checkout, a
 * URL or an `owner/repo` clones one.
 */
export type AddRepoInputKind = "empty" | "path" | "url" | "github";

/** A scheme-qualified remote: `https://`, `ssh://`, `git://`, `file://`. */
const SCHEME = /^[a-z][a-z0-9+.-]*:\/\//i;
/** Git's scp-like remote, `git@github.com:acme/app.git`. */
const SCP_REMOTE = /^[^\s/@]+@[^\s/:]+:/;
/** A path this machine would resolve: absolute, home-relative, or explicit. */
const PATH_PREFIX = /^(?:[/~]|\.{1,2}\/)/;
/** A Windows path, so a drive letter never reads as an scp remote. */
const WINDOWS_PATH = /^[A-Za-z]:[\\/]/;
/** `owner/repo`, the shorthand `gh` takes. */
const GITHUB_SLUG = /^[\w.-]+\/[\w.-]+$/;

/**
 * Read what a value asks for, without asking the machine.
 *
 * A value that matches nothing is treated as a path: `createCodeRepo` answers
 * with the reason that path is not a repository, which is a better error than
 * anything this function could guess.
 */
export function classifyAddRepoInput(value: string): AddRepoInputKind {
  const trimmed = value.trim();
  if (!trimmed) return "empty";
  if (WINDOWS_PATH.test(trimmed) || PATH_PREFIX.test(trimmed)) return "path";
  if (SCHEME.test(trimmed) || SCP_REMOTE.test(trimmed)) return "url";
  if (GITHUB_SLUG.test(trimmed)) return "github";
  return "path";
}

/** Whether adding this value clones, and so needs somewhere to clone into. */
export function addRepoInputClones(kind: AddRepoInputKind): boolean {
  return kind === "url" || kind === "github";
}

/**
 * The clone body for a value, or null when the destination is still missing.
 *
 * A field nobody was shown must not be sent, so `parent_dir` is omitted
 * whenever the machine places clones itself — the same rule the palette
 * follows.
 */
export function addRepoCloneRequest({
  value,
  parentDir,
  machineChoosesDestination,
}: {
  value: string;
  parentDir: string;
  machineChoosesDestination: boolean;
}): CodeCloneRequest | null {
  const kind = classifyAddRepoInput(value);
  if (!addRepoInputClones(kind)) return null;
  const trimmed = value.trim();
  const parent = machineChoosesDestination ? undefined : parentDir.trim();
  if (!machineChoosesDestination && !parent) return null;
  return {
    ...(kind === "github" ? { github: trimmed } : { url: trimmed }),
    parent_dir: parent || undefined,
  };
}
