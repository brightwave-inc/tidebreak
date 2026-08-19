import { BarChart3, FileText, Package, Sparkles, Table2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

import type { PluginCategory } from "@/api";
import { cn } from "@/lib/utils";

/**
 * The library's signature mark: a compact, colored instrument mark.
 *
 * A plugin is something you pick up and hand to the agent — the glyph has to
 * read as a tool at a glance, not as a muted list bullet. Color belongs to the
 * mark itself rather than a colored tile behind it; known bundles get a fixed
 * tone and purpose-built mark, while user bundles fall back to their category.
 */

type GlyphTone = {
  /** Ink for the lucide / path mark. */
  ink: string;
};

const CATEGORY_TONES: Record<PluginCategory, GlyphTone> = {
  documents: { ink: "text-icon-blue" },
  data: { ink: "text-icon-green" },
  visualization: { ink: "text-icon-violet" },
  other: { ink: "text-icon-cyan" },
};

const SKILL_TONE: GlyphTone = {
  ink: "text-icon-amber",
};

const CATEGORY_FALLBACK: Record<PluginCategory, LucideIcon> = {
  documents: FileText,
  data: Table2,
  visualization: BarChart3,
  other: Package,
};

/** Known built-in bundles: name wins over category so each keeps its own face. */
const BUNDLE_TONES: Record<string, GlyphTone> = {
  documents: CATEGORY_TONES.documents,
  spreadsheets: CATEGORY_TONES.data,
  charts: CATEGORY_TONES.visualization,
};

type GlyphSize = "sm" | "md" | "lg";

const SIZE_CLASS: Record<GlyphSize, string> = {
  sm: "size-8 rounded-lg [&_svg]:size-4",
  md: "size-10 rounded-[0.7rem] [&_svg]:size-5",
  lg: "size-14 rounded-2xl [&_svg]:size-7",
};

export function PluginGlyph({
  pluginName,
  category,
  size = "md",
  className,
}: {
  /** Bundle slug when known; drives the built-in marks. */
  pluginName?: string;
  category: PluginCategory;
  size?: GlyphSize;
  className?: string;
}) {
  const tone = (pluginName && BUNDLE_TONES[pluginName]) || CATEGORY_TONES[category];
  const mark = pluginName ? bundleMark(pluginName) : null;
  const Fallback = CATEGORY_FALLBACK[category];

  return (
    <span
      className={cn(
        "grid shrink-0 place-items-center",
        tone.ink,
        SIZE_CLASS[size],
        className,
      )}
      aria-hidden="true"
    >
      {mark ?? <Fallback strokeWidth={1.75} />}
    </span>
  );
}

/** A standalone skill's tile — warmer than a bundle so the two kinds separate. */
export function SkillGlyph({
  size = "md",
  className,
}: {
  size?: GlyphSize;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "grid shrink-0 place-items-center",
        SKILL_TONE.ink,
        SIZE_CLASS[size],
        className,
      )}
      aria-hidden="true"
    >
      <Sparkles strokeWidth={1.75} />
    </span>
  );
}

/**
 * Purpose-built marks for the three shipped bundles. Drawn as simple geometry
 * so they stay sharp at 16–28px and never lean on a third-party brand lockup.
 */
function bundleMark(name: string): ReactNode | null {
  switch (name) {
    case "documents":
      return <DocumentsMark />;
    case "spreadsheets":
      return <SpreadsheetsMark />;
    case "charts":
      return <ChartsMark />;
    default:
      return null;
  }
}

function DocumentsMark() {
  return (
    <svg viewBox="0 0 24 24" fill="none" className="size-[1.15em]">
      <path
        d="M7 3.75h7.2L18.5 8v12.25a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4.75a1 1 0 0 1 1-1Z"
        fill="currentColor"
        opacity={0.18}
      />
      <path
        d="M14.1 3.75V7.2a.8.8 0 0 0 .8.8h3.6"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinejoin="round"
      />
      <path
        d="M7 3.75h7.2L18.5 8v12.25a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V4.75a1 1 0 0 1 1-1Z"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinejoin="round"
      />
      <path
        d="M9.25 12.25h5.5M9.25 15.25h3.75"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
      />
    </svg>
  );
}

function SpreadsheetsMark() {
  return (
    <svg viewBox="0 0 24 24" fill="none" className="size-[1.15em]">
      <rect
        x={4.5}
        y={4.5}
        width={15}
        height={15}
        rx={2}
        fill="currentColor"
        opacity={0.18}
      />
      <rect
        x={4.5}
        y={4.5}
        width={15}
        height={15}
        rx={2}
        stroke="currentColor"
        strokeWidth={1.5}
      />
      <path
        d="M4.5 9.25h15M4.5 14.75h15M9.25 4.5v15M14.75 4.5v15"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
      />
    </svg>
  );
}

function ChartsMark() {
  return (
    <svg viewBox="0 0 24 24" fill="none" className="size-[1.15em]">
      <path
        d="M5 18.5V14M10 18.5V9.5M15 18.5V12M20 18.5V6.5"
        stroke="currentColor"
        strokeWidth={2}
        strokeLinecap="round"
      />
      <path
        d="M4 19.25h16.5"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
        opacity={0.45}
      />
    </svg>
  );
}
