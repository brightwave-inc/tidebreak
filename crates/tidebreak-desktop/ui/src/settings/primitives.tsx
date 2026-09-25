import { cloneElement, isValidElement, useId, type ReactNode } from "react";
import {
  CircleAlert,
  CircleCheck,
  CircleMinus,
  type LucideIcon,
} from "lucide-react";

import { Card } from "@/components/ui/card";
import {
  Notice,
  NoticeRetryButton,
  type NoticeTone,
} from "@/components/ui/notice";
import { PaneDragBand } from "@/WindowDragStrip";
import { cn } from "@/lib/utils";

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
 *
 * `error` says why the value cannot be saved. It sits directly under the
 * control in critical ink and marks the control invalid; a failure to load or
 * save the panel is a `SettingsError` instead.
 */
export function SettingsField({
  label,
  hint,
  status,
  error,
  children,
}: {
  label: string;
  hint?: ReactNode;
  status?: ReactNode;
  error?: ReactNode;
  children: ReactNode;
}) {
  const hintId = useId();
  const errorId = useId();
  const describedBy = [hint ? hintId : null, error ? errorId : null].filter(
    Boolean,
  );
  const control =
    describedBy.length > 0 &&
    isValidElement<{
      "aria-describedby"?: string;
      "aria-invalid"?: boolean;
      className?: string;
    }>(children)
      ? cloneElement(children, {
          "aria-describedby": [
            children.props["aria-describedby"],
            ...describedBy,
          ]
            .filter(Boolean)
            .join(" "),
          ...(error
            ? {
                "aria-invalid": true,
                className: cn(
                  children.props.className,
                  "aria-invalid:border-critical-border",
                ),
              }
            : {}),
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
      {error && <SettingsFieldError id={errorId}>{error}</SettingsFieldError>}
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
 * A group of related fields, with a heading and optional description above a
 * transparent bordered card. A panel is a stack of these. Every group takes a
 * title so the two-column layout stays aligned across the page.
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

const STATUS_NOTICE: Record<
  SettingsStatusTone,
  { tone: NoticeTone; icon: LucideIcon }
> = {
  ready: { tone: "success", icon: CircleCheck },
  neutral: { tone: "neutral", icon: CircleMinus },
  warning: { tone: "warning", icon: CircleAlert },
  critical: { tone: "critical", icon: CircleAlert },
};

/**
 * The readiness line a settings surface leads with: a short verdict and the one
 * sentence that says what to do about it. It is a `Notice`: a neutral surface
 * with the tone on its leading edge and icon.
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
  const notice = STATUS_NOTICE[tone];
  return (
    <Notice tone={notice.tone} icon={notice.icon} title={label} role="status">
      {description}
    </Notice>
  );
}

/**
 * Why a value on a settings surface cannot be saved, in critical ink beside
 * the field or form it belongs to. Validation is part of editing, not a
 * failure, so it never takes the notice shape and never offers a retry.
 */
export function SettingsFieldError({
  id,
  children,
}: {
  id?: string;
  children: ReactNode;
}) {
  return (
    <p id={id} className="text-sm text-critical break-words" role="alert">
      {children}
    </p>
  );
}

/**
 * A failure on a settings surface: a load or a save that did not go through,
 * as a critical `Notice`. When the failure is the panel's own load, pass
 * `onRetry` so the reader can run it again. The message arrives worded by
 * `friendlyErrorMessage`; this renders it as given. A value the reader can
 * fix is a `SettingsFieldError`, not this.
 */
export function SettingsError({
  children,
  title,
  onRetry,
  retrying = false,
  className,
}: {
  children: ReactNode;
  title?: ReactNode;
  onRetry?: () => void;
  /** The retry is running: its button waits instead of sending another. */
  retrying?: boolean;
  className?: string;
}) {
  return (
    <Notice
      tone="critical"
      title={title}
      className={className}
      action={
        onRetry && <NoticeRetryButton pending={retrying} onClick={onRetry} />
      }
    >
      {children}
    </Notice>
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
