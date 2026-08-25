import { invoke, isTauri } from "@tauri-apps/api/core";

export type SkillImportIssue = {
  name: string;
  reason: string;
};

export type SkillImportReport = {
  imported: string[];
  skipped: SkillImportIssue[];
  conflicts: SkillImportIssue[];
};

/** Open the native folder picker and import skills into this desktop profile. */
export async function importSkillsFromFolder(): Promise<SkillImportReport | null> {
  if (!isTauri()) {
    throw new Error("Skill import is available in the Tidebreak desktop app.");
  }
  const value = await invoke<unknown>("import_skills");
  if (value === null) return null;
  const report = parseSkillImportReport(value);
  if (!report)
    throw new Error("Tidebreak returned an invalid skill import result.");
  return report;
}

export function parseSkillImportReport(
  value: unknown,
): SkillImportReport | null {
  if (!isRecord(value)) return null;
  const imported = parseStrings(value.imported);
  const skipped = parseIssues(value.skipped);
  const conflicts = parseIssues(value.conflicts);
  if (!imported || !skipped || !conflicts) return null;
  return { imported, skipped, conflicts };
}

function parseStrings(value: unknown): string[] | null {
  if (!Array.isArray(value) || value.length > 64) return null;
  const strings = value.filter(
    (entry): entry is string =>
      typeof entry === "string" && entry.length > 0 && entry.length <= 64,
  );
  return strings.length === value.length ? strings : null;
}

function parseIssues(value: unknown): SkillImportIssue[] | null {
  if (!Array.isArray(value) || value.length > 256) return null;
  const issues: SkillImportIssue[] = [];
  for (const entry of value) {
    if (
      !isRecord(entry) ||
      typeof entry.name !== "string" ||
      entry.name.length === 0 ||
      entry.name.length > 255 ||
      typeof entry.reason !== "string" ||
      entry.reason.length === 0 ||
      entry.reason.length > 500
    ) {
      return null;
    }
    issues.push({ name: entry.name, reason: entry.reason });
  }
  return issues;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
