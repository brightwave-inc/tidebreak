import type { ReactNode } from "react";

/**
 * The shared shell for a transcript card that parks the turn on the person.
 *
 * It is the same chrome the approval card wears — a bordered surface with a
 * short title, an optional explanatory line, and a footer for the error a
 * decision can surface — so consent prompts, folder requests, and clarifying
 * questions read as one family rather than three hand-rolled panels. Callers
 * own the body between the title and the error: option rows, notes, actions.
 */
export function AttentionCard({
  title,
  titleId,
  subtitle,
  busy,
  error,
  children,
}: {
  title: ReactNode;
  titleId?: string;
  subtitle?: ReactNode;
  busy?: boolean;
  error?: string;
  children: ReactNode;
}) {
  return (
    <section
      className="bg-background flex max-w-prose flex-col gap-3 rounded-lg border p-4"
      aria-labelledby={titleId}
      aria-busy={busy}
    >
      <h3 id={titleId} className="font-medium break-words">
        {title}
      </h3>
      {subtitle != null && (
        <p className="text-muted-foreground text-sm break-words whitespace-pre-wrap">
          {subtitle}
        </p>
      )}
      {children}
      {error && (
        <p className="text-destructive text-xs break-words" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
