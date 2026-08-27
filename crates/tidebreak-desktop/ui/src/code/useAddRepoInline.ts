import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { CodeRepoSnapshot, CodeRepoSources } from "../api/types";
import { useApp } from "@/AppContext";
import {
  attachedRemotely,
  hasLocalHostAuthority,
  pickCodeDirectory,
} from "@/host";
import { friendlyErrorMessage } from "@/lib/utils";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import {
  activateCodeCloneClient,
  forgetCodeClone,
  reconcileCodeClone,
  setCodeCloneBackground,
  trackCodeClone,
  useCodeUpdatesStore,
} from "./CodeUpdatesStore";
import {
  addRepoCloneRequest,
  addRepoInputClones,
  classifyAddRepoInput,
  type AddRepoInputKind,
} from "./addRepoInput";

const CLONE_POLL_INTERVAL_MS = 1_500;

/** What the inline add-repo field is doing, and what it needs to finish. */
export type AddRepoInlineState = {
  value: string;
  setValue: (next: string) => void;
  parentDir: string;
  setParentDir: (next: string) => void;
  kind: AddRepoInputKind;
  /** Whether this value clones, and so needs somewhere to clone into. */
  needsDestination: boolean;
  machineChoosesDestination: boolean;
  canBrowse: boolean;
  attached: boolean;
  /** Why this value cannot be added from this window, when it cannot. */
  blocked: string | null;
  busy: boolean;
  error: string | null;
  /** The clone's own phase while one is running. */
  phase: string | null;
  percent: number | null;
  defaultsProbeFailed: boolean;
  defaultsBusy: boolean;
  canSubmit: boolean;
  submit: () => void;
  browse: () => void;
  browseDestination: () => void;
  retryDefaults: () => void;
};

/**
 * Register or clone a repository from inside the new-workspace composer.
 *
 * The state lives in the dialog rather than in the popover, because a clone
 * takes minutes and the field it started from is one click from closing. A
 * clone still running when the dialog closes is handed to the background, the
 * same way the add-repo palette hands one over, so the resume path already in
 * `CodeUpdatesStore` picks it up.
 */
export function useAddRepoInline({
  open,
  active,
  onAdded,
}: {
  /** The dialog is open. Closing it clears what was typed. */
  open: boolean;
  /** The field is on screen. The machine is probed once it is. */
  active: boolean;
  onAdded: (repo: CodeRepoSnapshot) => void;
}): AddRepoInlineState {
  const { client } = useApp();
  const upsertRepo = useCodeCatalogStore((state) => state.upsertRepo);
  const [value, setValue] = useState("");
  const [parentDir, setParentDir] = useState("");
  const [sources, setSources] = useState<CodeRepoSources | null>(null);
  const [seededDestination, setSeededDestination] = useState(false);
  const [defaultsProbeFailed, setDefaultsProbeFailed] = useState(false);
  const [defaultsBusy, setDefaultsBusy] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [jobId, setJobId] = useState<string | null>(null);
  const live = useRef(true);
  const probed = useRef(false);
  const jobIdRef = useRef<string | null>(null);
  const handedOff = useRef<string | null>(null);
  const addedRef = useRef(onAdded);
  addedRef.current = onAdded;
  // A destination the reader typed is never overwritten by a probe that
  // answers later.
  const destinationEdited = useRef(false);

  const job = useCodeUpdatesStore((state) =>
    jobId ? state.cloneJobs[jobId] : undefined,
  );

  const canBrowse = hasLocalHostAuthority();
  const attached = attachedRemotely();
  const machineChoosesDestination = sources?.chooses_destination === true;
  const kind = classifyAddRepoInput(value);
  const needsDestination =
    addRepoInputClones(kind) && !machineChoosesDestination;

  const sourceState = useCallback(
    (wanted: string) =>
      sources?.sources.find((source) => source.kind === wanted),
    [sources],
  );

  // Why the machine would refuse this value before it is sent. A window
  // attached to another machine cannot name a path that machine resolves,
  // which is why the palette hides its local source outright.
  const blocked = useMemo(() => {
    if (kind === "path") {
      if (attached) {
        return "This window is attached to another machine. Clone from a URL instead.";
      }
      const local = sourceState("local");
      if (local && !local.available) {
        return (
          local.remediation || "This machine cannot register a local folder."
        );
      }
      return null;
    }
    if (kind === "url" || kind === "github") {
      const source = sourceState(kind === "github" ? "github" : "git_url");
      if (source && !source.available) {
        return source.remediation || "This machine cannot clone that source.";
      }
      if (attached && !machineChoosesDestination) {
        return "This machine has no clone destination configured. An administrator sets one on the machine.";
      }
    }
    return null;
  }, [attached, kind, machineChoosesDestination, sourceState]);

  const loadDefaults = useCallback(
    async (signal: AbortSignal, replaceDestination: boolean) => {
      if (replaceDestination) setDefaultsBusy(true);
      try {
        const next = await client.getCodeCloneDefaults(signal);
        if (!live.current || signal.aborted) return;
        setDefaultsProbeFailed(false);
        if (replaceDestination || !destinationEdited.current) {
          setParentDir(next.parent_dir ?? "");
          setSeededDestination(true);
        }
      } catch {
        if (!live.current || signal.aborted) return;
        // Administrator-only. A member on a shared machine is refused here,
        // and the machine fills the destination in for itself.
        setDefaultsProbeFailed(replaceDestination);
      } finally {
        if (live.current && !signal.aborted) setDefaultsBusy(false);
      }
    },
    [client],
  );

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  // Clearing on close, not on the field closing: a failed register keeps what
  // was typed so the fix is an edit rather than a retype.
  useEffect(() => {
    if (open) return;
    probed.current = false;
    destinationEdited.current = false;
    setValue("");
    setParentDir("");
    setSources(null);
    setSeededDestination(false);
    setDefaultsProbeFailed(false);
    setDefaultsBusy(false);
    setBusy(false);
    setError(null);
    setJobId(null);
    jobIdRef.current = null;
  }, [open]);

  // Probe once the field is actually on screen. Every Cmd+N would otherwise
  // spend two requests on a picker most creates never open.
  useEffect(() => {
    if (!open || !active || probed.current) return;
    probed.current = true;
    activateCodeCloneClient(client);
    const controller = new AbortController();
    void client
      .getCodeRepoSources(controller.signal)
      .then((next) => {
        if (live.current && !controller.signal.aborted) setSources(next);
      })
      .catch(() => {});
    void loadDefaults(controller.signal, false);
    return () => controller.abort();
  }, [active, client, loadDefaults, open]);

  // The clone's durable state is the server's. Poll it the way the palette
  // does, so a dropped update stream still finishes the handoff.
  useEffect(() => {
    if (!jobId || job?.done) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const poll = async () => {
      const next = await reconcileCodeClone(client, jobId);
      if (cancelled || !next || next.done) return;
      timer = setTimeout(() => void poll(), CLONE_POLL_INTERVAL_MS);
    };
    void poll();
    return () => {
      cancelled = true;
      if (timer !== null) clearTimeout(timer);
    };
  }, [client, job?.done, jobId]);

  // A clone still running when the dialog closes keeps running. Backgrounding
  // it is what makes it resumable from the palette.
  useEffect(() => {
    return () => {
      const running = jobIdRef.current;
      if (running) setCodeCloneBackground(client, running, true);
    };
  }, [client]);

  useEffect(() => {
    if (!jobId || !job?.done || handedOff.current === jobId) return;
    handedOff.current = jobId;
    if (job.error || !job.repo_id) {
      forgetCodeClone(client, jobId);
      jobIdRef.current = null;
      setJobId(null);
      setBusy(false);
      setError(job.error || "The clone finished without a repository.");
      return;
    }
    const repoId = job.repo_id;
    const finishedJobId = jobId;
    void client
      .getCodeRepo(repoId)
      .then((repo) => {
        if (!live.current) return;
        upsertRepo(repo);
        forgetCodeClone(client, finishedJobId);
        jobIdRef.current = null;
        setJobId(null);
        setBusy(false);
        addedRef.current(repo);
      })
      .catch((caught: unknown) => {
        if (!live.current) return;
        jobIdRef.current = null;
        setJobId(null);
        setBusy(false);
        setError(
          friendlyErrorMessage(
            caught,
            "Cloned, but could not read the repository",
          ),
        );
      });
  }, [client, job, jobId, upsertRepo]);

  const canSubmit =
    !busy &&
    kind !== "empty" &&
    !blocked &&
    (!needsDestination || parentDir.trim().length > 0);

  const submit = useCallback(() => {
    if (busy || blocked) return;
    const inputKind = classifyAddRepoInput(value);
    if (inputKind === "empty") return;
    setError(null);
    if (inputKind === "path") {
      setBusy(true);
      void client
        .createCodeRepo({ path: value.trim() })
        .then((repo) => {
          if (!live.current) return;
          upsertRepo(repo);
          setBusy(false);
          addedRef.current(repo);
        })
        .catch((caught: unknown) => {
          if (!live.current) return;
          setBusy(false);
          setError(
            friendlyErrorMessage(caught, "Could not register that repository"),
          );
        });
      return;
    }
    const request = addRepoCloneRequest({
      value,
      parentDir,
      machineChoosesDestination,
    });
    if (!request) return;
    setBusy(true);
    void client
      .startCodeClone(request)
      .then((started) => {
        if (!live.current) return;
        if (!trackCodeClone(client, started, request)) {
          setBusy(false);
          return;
        }
        handedOff.current = null;
        jobIdRef.current = started.id;
        setJobId(started.id);
      })
      .catch((caught: unknown) => {
        if (!live.current) return;
        setBusy(false);
        setError(friendlyErrorMessage(caught, "Could not start the clone"));
      });
  }, [
    blocked,
    busy,
    client,
    machineChoosesDestination,
    parentDir,
    upsertRepo,
    value,
  ]);

  const editParentDir = useCallback((next: string) => {
    destinationEdited.current = true;
    setParentDir(next);
  }, []);

  const browseInto = useCallback(
    async (into: "value" | "destination") => {
      const picked = await pickCodeDirectory();
      if (!picked || !live.current) return;
      if (into === "value") setValue(picked);
      else editParentDir(picked);
    },
    [editParentDir],
  );

  const retryDefaults = useCallback(() => {
    const controller = new AbortController();
    void loadDefaults(controller.signal, true);
  }, [loadDefaults]);

  return {
    value,
    setValue,
    parentDir,
    setParentDir: editParentDir,
    kind,
    needsDestination,
    machineChoosesDestination,
    canBrowse,
    attached,
    blocked,
    busy,
    error,
    phase: jobId ? (job?.phase ?? "Starting") : null,
    percent: jobId ? (job?.percent ?? null) : null,
    defaultsProbeFailed: defaultsProbeFailed && !seededDestination,
    defaultsBusy,
    canSubmit,
    submit,
    browse: () => void browseInto("value"),
    browseDestination: () => void browseInto("destination"),
    retryDefaults,
  };
}
