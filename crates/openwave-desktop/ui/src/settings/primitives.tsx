import type { ReactNode } from "react";

/** Shared styling for the native <select> elements used across settings. */
export const SETTINGS_SELECT_CLASS =
  "flex h-10 w-full rounded-md border border-border bg-background px-3 text-sm ring-offset-background focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50";

/**
 * A single settings surface: a titled, optionally described column of fields.
 * The surrounding container owns scrolling and chrome, so this stays layout-only
 * and works both as a full-page section and as a docked side panel.
 */
export function SettingsPanel({
  title,
  description,
  busy,
  children,
}: {
  title: string;
  description?: string;
  busy?: boolean;
  children: ReactNode;
}) {
  return (
    <div className="settings-panel" aria-busy={busy}>
      <div className="flex flex-col gap-1">
        <h2 className="text-base font-semibold tracking-tight">{title}</h2>
        {description && (
          <p className="text-sm text-muted-foreground">{description}</p>
        )}
      </div>
      {children}
    </div>
  );
}

export function SettingsField({
  label,
  hint,
  children,
}: {
  label: string;
  hint?: ReactNode;
  children: ReactNode;
}) {
  return (
    <label className="flex flex-col gap-1.5">
      <span className="text-sm font-medium">{label}</span>
      {children}
      {hint && <span className="text-xs text-muted-foreground">{hint}</span>}
    </label>
  );
}

export function SettingsSection({
  title,
  children,
}: {
  title?: string;
  children: ReactNode;
}) {
  return (
    <section className="flex flex-col gap-3 rounded-lg border border-border p-4">
      {title && <h3 className="text-sm font-semibold capitalize">{title}</h3>}
      {children}
    </section>
  );
}

export function SettingsError({ children }: { children: ReactNode }) {
  return (
    <p className="text-sm text-destructive" role="alert">
      {children}
    </p>
  );
}
