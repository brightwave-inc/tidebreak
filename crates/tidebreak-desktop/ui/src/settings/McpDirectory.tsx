import { useId, useState, type ReactNode } from "react";
import { Check } from "lucide-react";

import type { McpDirectoryEntry, McpServerInfo } from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { McpTierChip } from "./McpTierChip";
import { SettingsError, SettingsSection } from "./primitives";

/**
 * A URL reduced to the parts that name an endpoint, so a configured server
 * whose address differs only by a trailing slash still reads as added. The
 * server applies the same rule when it refuses to add an endpoint twice.
 */
export function endpointKey(url: string): string {
  try {
    const parsed = new URL(url);
    const path = parsed.pathname.replace(/\/+$/, "");
    return `${parsed.protocol}//${parsed.host}${path}${parsed.search}`;
  } catch {
    return url;
  }
}

/** The host a directory server connects to. */
function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * How a directory row says the server signs in, and where it connects. A
 * token row says what the first connect sends and to which host, in the
 * words the import flow uses.
 */
function signInLabel(entry: McpDirectoryEntry): ReactNode {
  const host = <span className="font-mono">{hostOf(entry.url)}</span>;
  switch (entry.sign_in.kind) {
    case "oauth":
      return <>Sign in with your browser · {host}</>;
    case "token":
      return (
        <>
          Sends <span className="font-mono">{entry.sign_in.variable}</span> to{" "}
          {host}.
        </>
      );
    case "none":
      return <>No sign-in · {host}</>;
  }
}

function matches(entry: McpDirectoryEntry, needle: string): boolean {
  return [entry.name, entry.description, hostOf(entry.url)].some((text) =>
    text.toLowerCase().includes(needle),
  );
}

/**
 * The directory of remote MCP servers, as a list the person can search.
 *
 * Each row names the server, what it lets you do, how it signs in, and the
 * host it connects to, with one Add action. A server that reads a token asks
 * first, with a switch that stays off until the person turns it on: its first
 * connect sends the token to the vendor's host, so an add with the switch off
 * saves the server turned off. A server already configured at the same
 * address reads as added. The tier chip appears only when the curated list
 * vouches for the server; the directory claims no tier itself.
 *
 * Presentational: the panel owns the directory read, the add, and the
 * sign-in that follows it.
 */
export function McpDirectoryList({
  servers,
  configured,
  adding,
  disabled,
  loadError,
  addError,
  initialQuery = "",
  onAdd,
}: {
  /** The directory, or `null` while it loads. */
  servers: McpDirectoryEntry[] | null;
  /** The configured servers, so an entry already added says so. */
  configured: McpServerInfo[];
  /** The id of the entry being added, if any. */
  adding: string | null;
  /** Whether other work on the page holds every Add. */
  disabled: boolean;
  loadError: string | null;
  /** Why the last add failed. */
  addError: string | null;
  initialQuery?: string;
  /** Add one entry. `start` says whether to connect it once it is saved. */
  onAdd: (entry: McpDirectoryEntry, start: boolean) => void;
}) {
  const [query, setQuery] = useState(initialQuery);
  const added = new Set(
    configured.flatMap((server) =>
      server.url !== null && server.plugin === null
        ? [endpointKey(server.url)]
        : [],
    ),
  );
  const needle = query.trim().toLowerCase();
  const shown =
    servers?.filter((entry) => needle === "" || matches(entry, needle)) ?? [];

  let body: ReactNode;
  if (loadError !== null) {
    body = (
      <SettingsError>Could not load the directory: {loadError}</SettingsError>
    );
  } else if (servers === null) {
    body = (
      <p className="text-sm text-muted-foreground">Loading the directory…</p>
    );
  } else if (shown.length === 0) {
    body = (
      <p className="text-sm text-muted-foreground" role="status">
        No server in the directory matches “{query.trim()}”. To add a server
        that is not listed, select Add server below.
      </p>
    );
  } else {
    body = (
      <ul
        aria-label="MCP directory"
        className="max-h-[30rem] divide-y divide-border-subtle overflow-y-auto rounded-lg border"
      >
        {shown.map((entry) => (
          <DirectoryRow
            key={entry.id}
            entry={entry}
            added={added.has(endpointKey(entry.url))}
            adding={adding === entry.id}
            disabled={disabled}
            onAdd={(start) => onAdd(entry, start)}
          />
        ))}
      </ul>
    );
  }

  return (
    <SettingsSection
      title="Directory"
      description="Remote MCP servers their vendors host, at the address each vendor publishes. Add one, and sign in if it asks you to."
    >
      <Input
        type="search"
        value={query}
        placeholder="Search servers"
        aria-label="Search the MCP directory"
        autoComplete="off"
        spellCheck={false}
        disabled={servers === null}
        onChange={(event) => setQuery(event.target.value)}
      />
      {body}
      {addError !== null && <SettingsError>{addError}</SettingsError>}
    </SettingsSection>
  );
}

function DirectoryRow({
  entry,
  added,
  adding,
  disabled,
  onAdd,
}: {
  entry: McpDirectoryEntry;
  added: boolean;
  adding: boolean;
  disabled: boolean;
  onAdd: (start: boolean) => void;
}) {
  const startId = useId();
  const [start, setStart] = useState(false);
  // Only a token server sends something of the person's on its first
  // connect, so only its row asks. Every other row connects on Add.
  const asks = entry.sign_in.kind === "token";
  return (
    <li className="flex items-start gap-3 px-3 py-2.5">
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <p className="text-sm font-medium">{entry.name}</p>
          {entry.curated !== null && <McpTierChip curated={entry.curated} />}
        </div>
        <p className="text-xs text-muted-foreground">{entry.description}</p>
        <p className="text-xs break-words text-muted-foreground">
          {signInLabel(entry)}
        </p>
        {asks && !added && (
          <div className="mt-2 flex items-start gap-2">
            <Switch
              id={startId}
              checked={start}
              onCheckedChange={setStart}
              disabled={disabled || adding}
              aria-label={`Start ${entry.name} after adding`}
              aria-describedby={`${startId}-hint`}
            />
            <div className="flex min-w-0 flex-col">
              {/* The label's line box matches the switch's height, so its
                  text sits on the switch's center line. */}
              <label
                htmlFor={startId}
                className="text-xs leading-6 font-medium"
              >
                Start after adding
              </label>
              <p
                id={`${startId}-hint`}
                className="text-xs text-muted-foreground"
              >
                {start
                  ? "Connects when you add it."
                  : "Adds it turned off. You can turn it on later in Connected apps."}
              </p>
            </div>
          </div>
        )}
      </div>
      <div className="flex h-control-sm shrink-0 items-center">
        {added ? (
          <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
            <Check aria-hidden className="size-3.5 text-success" />
            Added
          </span>
        ) : (
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label={`Add ${entry.name}`}
            disabled={disabled || adding}
            onClick={() => onAdd(asks ? start : true)}
          >
            {adding && <Spinner aria-hidden className="size-3.5" />}
            {adding ? "Adding…" : "Add"}
          </Button>
        )}
      </div>
    </li>
  );
}
