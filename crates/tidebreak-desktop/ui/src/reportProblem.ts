import { getVersion } from "@tauri-apps/api/app";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { create } from "zustand";

/** Where a new Tidebreak issue starts. */
export const NEW_ISSUE_URL =
  "https://github.com/naingthet/tidebreak/issues/new";

/** The issue form a report fills in (`.github/ISSUE_TEMPLATE`). */
export const BUG_REPORT_TEMPLATE = "bug-report.yml";

/**
 * What a new issue is prefilled with: the build and the machine, and
 * nothing else. The native shell reads the operating system and the
 * architecture, which a webview cannot report truthfully.
 */
export type ProblemReportFacts = {
  version: string | null;
  os: string | null;
  arch: string | null;
};

const NO_FACTS: ProblemReportFacts = { version: null, os: null, arch: null };

/**
 * The address of a new bug report with the version, operating system, and
 * architecture filled in.
 *
 * The address reaches GitHub the moment the browser opens it, before the
 * person has read a word of it, so it carries exactly the three facts the
 * form asks for by field id. Logs, errors, and anything from a conversation
 * stay in the diagnostics report, which the person attaches themselves.
 */
export function problemReportIssueUrl(facts: ProblemReportFacts): string {
  const url = new URL(NEW_ISSUE_URL);
  url.searchParams.set("template", BUG_REPORT_TEMPLATE);
  const fields: [keyof ProblemReportFacts, string | null][] = [
    ["version", facts.version],
    ["os", facts.os],
    ["arch", facts.arch],
  ];
  for (const [field, value] of fields) {
    if (value?.trim()) url.searchParams.set(field, value.trim());
  }
  return url.toString();
}

/** The facts for a bug report, from the native shell when there is one. */
export async function problemReportFacts(): Promise<ProblemReportFacts> {
  if (!isTauri()) return NO_FACTS;
  try {
    return await invoke<ProblemReportFacts>("problem_report_facts");
  } catch {
    return { ...NO_FACTS, version: await getVersion().catch(() => null) };
  }
}

/**
 * Save the diagnostics report to a file the person picks. Resolves `false`
 * when they close the save dialog. The shell builds the report even when
 * the server never started, which is when a report matters most.
 */
export function saveDiagnosticsReport(): Promise<boolean> {
  return invoke<boolean>("save_diagnostics_report");
}

/** Open this computer's Tidebreak logs folder in its file manager. */
export function revealLogsDirectory(): Promise<void> {
  return invoke("reveal_logs_directory");
}

/** Where a report was started from, and what that place adds to it. */
export type ReportProblemRequest = {
  /**
   * Start an agent that files the report with this session's debug details,
   * as decision 81 describes. Only a code workspace with a session has them.
   */
  askAgent?: () => void;
};

type ReportProblemState = {
  /** The open dialog's request, or `null` while it is closed. */
  request: ReportProblemRequest | null;
  open: (request?: ReportProblemRequest) => void;
  close: () => void;
};

/**
 * The one Report a problem dialog. Every way in, from the Help menu, the
 * boot screen, a crash, Settings, or a workspace, opens it here.
 */
export const useReportProblem = create<ReportProblemState>()((set) => ({
  request: null,
  open: (request = {}) => set({ request }),
  close: () => set({ request: null }),
}));

/** Open the Report a problem dialog. */
export function openReportProblem(request?: ReportProblemRequest): void {
  useReportProblem.getState().open(request);
}
