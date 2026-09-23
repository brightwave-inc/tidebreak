import { cloneElement, isValidElement, useId, type ReactNode } from "react";
import { CircleAlert, CircleCheck, CircleMinus } from "lucide-react";

import { Card } from "@/components/ui/card";
import { PaneDragBand } from "@/WindowDragStrip";

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
      <PaneDragBand />
      <div className="settings-panel-inner">
        <header className="settings-panel-header">
          <h1 className="settings-panel-title" tabIndex={-1}>
            {title}
          </h1>
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
 *
 * `status` is a short reading about the value, such as how much of a size
 * limit it uses. It sits at the right end of the hint's row, under the
 * control's right edge.
 */
export function SettingsField({
  label,
  hint,
  status,
  children,
}: {
  label: string;
  hint?: ReactNode;
  status?: ReactNode;
  children: ReactNode;
}) {
  const hintId = useId();
  const control =
    hint && isValidElement<{ "aria-describedby"?: string }>(children)
      ? cloneElement(children, {
          "aria-describedby": [children.props["aria-describedby"], hintId]
            .filter(Boolean)
            .join(" "),
        })
      : children;
  const hintElement = hint && (
    <span id={hintId} className="settings-field-hint">
      {hint}
    </span>
  );
  return (
    <div className="settings-field">
      <label>
        <span className="settings-field-label">{label}</span>
        {control}
      </label>
      {status ? (
        <div className="flex items-start justify-between gap-4">
          {hintElement}
          <span className="settings-field-hint ml-auto shrink-0 tabular-nums">
            {status}
          </span>
        </div>
      ) : (
        hintElement
      )}
    </div>
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
 * What a settings verdict means, in the status vocabulary from DESIGN.md:
 * `ready` works, `neutral` is off or not set up, `warning` needs a look, and
 * `critical` is a real failure. An optional feature nobody has set up is
 * neutral, not an error.
 */
export type SettingsStatusTone = "ready" | "neutral" | "warning" | "critical";

const STATUS_NOTICE: Record<SettingsStatusTone, string> = {
  ready: "notice-success",
  neutral: "",
  warning: "notice-warning",
  critical: "notice-critical",
};

/**
 * The readiness line a settings surface leads with: a short verdict and the one
 * sentence that says what to do about it. It reads as a notice: a neutral
 * surface with the tone on its leading edge and icon, so a panel never has to
 * reach for the class itself.
 */
export function SettingsStatus({
  tone,
  label,
  description,
}: {
  tone: SettingsStatusTone;
  label: string;
  description: ReactNode;
}) {
  const Icon =
    tone === "ready"
      ? CircleCheck
      : tone === "neutral"
        ? CircleMinus
        : CircleAlert;
  return (
    <div
      className={`settings-status ${STATUS_NOTICE[tone]}`.trim()}
      role="status"
    >
      <Icon className="settings-status-icon" aria-hidden="true" />
      <span className="settings-status-copy">
        <strong>{label}</strong>
        <span className="break-words">{description}</span>
      </span>
    </div>
  );
}

/**
 * An error line on a settings surface, in the critical ink that reads as text
 * on the page in both themes. `String(err)` puts the error's class name in
 * front of the message ("Error: …", "HttpError: …"); the reader needs only
 * the message.
 */
export function SettingsError({ children }: { children: ReactNode }) {
  return (
    <p className="text-sm text-critical break-words" role="alert">
      {typeof children === "string" ? withoutErrorName(children) : children}
    </p>
  );
}

function withoutErrorName(message: string): string {
  return message.replace(/^(?:[A-Z][A-Za-z]*)?Error:\s*/, "") || message;
}

/**
 * The one line naming how many local mini-apps bind a capability — a
 * connected-app record or a gateway app alike. Shared so the two pages that
 * carry the count cannot drift into two spellings of the same sentence.
 */
export function usedByLabel(count: number): string {
  return `Used by ${count} local app${count === 1 ? "" : "s"}`;
}
