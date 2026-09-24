import type { ReactNode, Ref } from "react";
import { Notice } from "@/components/ui/notice";

/**
 * The shared shell for a transcript card that parks the turn on the person.
 *
 * A bordered surface with a short title, an optional explanatory line, and a
 * footer for an error. Consent itself belongs on ApprovalCard. Callers own the
 * body between the title and the error: notes, actions, setup links.
 */
export function AttentionCard({
  title,
  titleId,
  subtitle,
  busy,
  error,
  headingRef,
  children,
}: {
  title: ReactNode;
  titleId?: string;
  subtitle?: ReactNode;
  busy?: boolean;
  error?: string;
  headingRef?: Ref<HTMLHeadingElement>;
  children: ReactNode;
}) {
  return (
    <section
      className="bg-background flex w-full min-w-0 flex-col gap-3 rounded-lg border p-4"
      aria-labelledby={titleId}
      aria-busy={busy}
    >
      <h3
        id={titleId}
        ref={headingRef}
        tabIndex={headingRef ? -1 : undefined}
        className="font-medium break-words outline-hidden"
      >
        {title}
      </h3>
      {subtitle != null && (
        <p className="text-muted-foreground text-sm break-words whitespace-pre-wrap">
          {subtitle}
        </p>
      )}
      {children}
      {error && (
        <Notice tone="critical" density="compact">
          {error}
        </Notice>
      )}
    </section>
  );
}
