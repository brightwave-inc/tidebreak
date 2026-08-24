import { useId, type ReactNode } from "react";
import { CircleAlert, CircleCheck, CircleMinus } from "lucide-react";

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
    <div className="settings-panel" aria-busy={busy}>
      <div className="settings-panel-inner">
        <header className="settings-panel-header">
          <h1 className="settings-panel-title">{title}</h1>
          {description && (
            <p className="settings-panel-description">{description}</p>
          )}
        </header>
        <div className="settings-panel-content">{children}</div>
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
    <label className="settings-field">
      <span className="settings-field-label">{label}</span>
      {children}
      {hint && <span className="settings-field-hint">{hint}</span>}
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
  const headingId = useId();
  const hasHeading = Boolean(title || description);
  return (
    <section
      className="settings-section"
      data-has-heading={hasHeading || undefined}
      aria-labelledby={title ? headingId : undefined}
    >
      {hasHeading && (
        <header className="settings-section-header">
          {title && (
            <h2 id={headingId} className="settings-section-title">
              {title}
            </h2>
          )}
          {description && (
            <p className="settings-section-description">{description}</p>
          )}
        </header>
      )}
      <Card className="settings-section-card gap-4 rounded-none bg-transparent p-0 ring-0">
        {children}
      </Card>
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
  const Icon =
    tone === "ready"
      ? CircleCheck
      : tone === "not-configured"
        ? CircleAlert
        : CircleMinus;
  return (
    <div className={`settings-status is-${tone}`} role="status">
      <Icon className="settings-status-icon" aria-hidden="true" />
      <span className="settings-status-copy">
        <strong>{label}</strong>
        <span>{description}</span>
      </span>
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
