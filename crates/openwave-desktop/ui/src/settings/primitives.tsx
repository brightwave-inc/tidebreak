import type { ReactNode } from "react";

import { Card } from "@/components/ui/card";

/**
 * A whole settings surface: the page title, an optional description, and the
 * sections below it.
 *
 * Panels only name their sections and fields; the composition — a large page
 * title, a bounded reading column, and the rhythm between sections — is owned
 * here, so bringing the surface in line does not mean editing every panel.
 * The surrounding route owns the rail and the window; this owns the scroll.
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
    <div className="h-full min-h-0 overflow-y-auto" aria-busy={busy}>
      <div className="mx-auto flex w-full max-w-4xl flex-col gap-10 px-6 py-8 md:px-10">
        <div className="flex flex-col gap-1">
          <h1 className="text-2xl font-medium tracking-tight">{title}</h1>
          {description && (
            <p className="text-sm text-muted-foreground">{description}</p>
          )}
        </div>
        {children}
      </div>
    </div>
  );
}

/**
 * A field within a section: a label and its control, kept in one `label` so the
 * control takes the label's name whether or not it carries its own. Full width,
 * because the controls that live here — selects, text inputs, editors — read
 * badly squeezed against the right edge.
 */
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
      <span className="font-bold">{label}</span>
      {children}
      {hint && <span className="text-sm text-muted-foreground">{hint}</span>}
    </label>
  );
}

/**
 * A group of related fields, with an optional heading and description above a
 * transparent bordered card. A panel is a stack of these; a section with no
 * heading is just the card, for panels that carry a single unnamed group.
 */
export function SettingsSection({
  title,
  description,
  children,
}: {
  title?: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <section className="flex flex-col gap-4">
      {(title || description) && (
        <div className="flex flex-col gap-1">
          {title && <h2 className="text-lg font-semibold">{title}</h2>}
          {description && (
            <p className="text-sm text-muted-foreground">{description}</p>
          )}
        </div>
      )}
      <Card className="gap-4 border bg-transparent p-4">{children}</Card>
    </section>
  );
}

/**
 * The readiness line a settings surface leads with: a short verdict and the one
 * sentence that says what to do about it. The tone carries the only colour —
 * green for ready, red for something the user still has to supply — so a panel
 * never has to reach for the class itself.
 */
export function SettingsStatus({
  tone,
  label,
  description,
}: {
  tone: "ready" | "not-configured" | "disabled";
  label: string;
  description: ReactNode;
}) {
  return (
    <div className={`settings-status is-${tone}`} role="status">
      <strong>{label}</strong>
      <span>{description}</span>
    </div>
  );
}

export function SettingsError({ children }: { children: ReactNode }) {
  return (
    <p className="text-sm text-destructive" role="alert">
      {children}
    </p>
  );
}

/**
 * The one line naming how many local mini-apps bind a capability — a
 * connected-app record or a gateway app alike. Shared so the two pages that
 * carry the count cannot drift into two spellings of the same sentence.
 */
export function usedByLabel(count: number): string {
  return `Used by ${count} local app${count === 1 ? "" : "s"}`;
}
