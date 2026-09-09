import type { ApiClient } from "../api/client";
import type {
  CodeSessionSnapshot,
  SessionAccessLevel,
  SessionAccessSnapshot,
  SessionAccessSummary,
  SessionAllowedAction,
  SessionVisibility,
} from "../api/types";
import { codeClientGeneration } from "./CodeClientGeneration";
import { useCallback, useEffect, useRef, useState } from "react";

/**
 * The server-resolved access summary for one code session (decision 0086).
 *
 * The renderer never infers rights from the snapshot: every allowed action
 * comes from `GET /sessions/{id}/access-summary`, and while that answer is
 * loading the surface fails closed (no composer, no decisions, no sharing).
 * Access changes on the updates channel replace the session list; this hook
 * refetches when the client generation or the session id changes.
 */
export type CodeSessionAccess = {
  /** `null` until the server answered; surfaces must fail closed meanwhile. */
  summary: SessionAccessSummary | null;
  /** Owner-only access rows; `null` for anyone who cannot manage access. */
  rows: SessionAccessSnapshot[] | null;
  error: string | null;
  reload: () => Promise<void>;
  can: (action: SessionAllowedAction) => boolean;
  setVisibility: (visibility: SessionVisibility) => Promise<CodeSessionSnapshot>;
  addAccess: (subject: string, level: SessionAccessLevel) => Promise<void>;
  revokeAccess: (subject: string) => Promise<void>;
};

export function canAccess(
  summary: SessionAccessSummary | null | undefined,
  action: SessionAllowedAction,
): boolean {
  return summary?.allowed_actions.includes(action) ?? false;
}

export function useCodeSessionAccess(
  client: ApiClient,
  sessionId: string,
): CodeSessionAccess {
  const [summary, setSummary] = useState<SessionAccessSummary | null>(null);
  const [rows, setRows] = useState<SessionAccessSnapshot[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const clientGenerationRef = useRef(codeClientGeneration(client));
  const [revision, setRevision] = useState(0);

  const load = useCallback(async () => {
    try {
      const next = await client.getCodeSessionAccessSummary(sessionId);
      setSummary(next);
      setError(null);
      // The access list is owner-only; only fetch it when the summary proves
      // this caller may manage access.
      if (next.allowed_actions.includes("manage_access")) {
        const listed = await client.listCodeSessionAccess(sessionId);
        setRows(listed);
      } else {
        setRows(null);
      }
    } catch (caught) {
      setError(String(caught));
      setSummary((current) =>
        current?.session.id === sessionId ? current : null,
      );
      setRows(null);
    }
  }, [client, sessionId]);

  useEffect(() => {
    const generation = codeClientGeneration(client);
    if (generation !== clientGenerationRef.current) {
      clientGenerationRef.current = generation;
      setSummary(null);
      setRows(null);
      setError(null);
      setRevision((value) => value + 1);
    }
  }, [client]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  const reload = useCallback(async () => {
    await load();
  }, [load]);

  const setVisibility = useCallback(
    async (visibility: SessionVisibility) => {
      const session = await client.setCodeSessionVisibility(sessionId, visibility);
      await load();
      return session;
    },
    [client, load, sessionId],
  );

  const addAccess = useCallback(
    async (subject: string, level: SessionAccessLevel) => {
      await client.addCodeSessionAccess(sessionId, subject, level);
      await load();
    },
    [client, load, sessionId],
  );

  const revokeAccess = useCallback(
    async (subject: string) => {
      await client.revokeCodeSessionAccess(sessionId, subject);
      await load();
    },
    [client, load, sessionId],
  );

  return {
    summary,
    rows,
    error,
    reload,
    can: (action) => canAccess(summary, action),
    setVisibility,
    addAccess,
    revokeAccess,
  };
}
