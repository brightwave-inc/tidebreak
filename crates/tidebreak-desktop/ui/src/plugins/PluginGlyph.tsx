import { BarChart3, FileText, Package, Sparkles, Table2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

import type { PluginCategory } from "@/api";
import { cn } from "@/lib/utils";

/**
 * The library's signature mark: a small instrument tile.
 *
 * A plugin is something you pick up and hand to the agent — the glyph has to
 * read as a tool at a glance, not as a muted list bullet. Known bundles get a
 * fixed wash and a purpose-built mark; everything else falls back to its
 * category so a user-written bundle still has a face.
 */

type GlyphTone = {
  /** Soft wash behind the mark. */
  wash: string;
  /** Ink for the lucide / path mark. */
  ink: string;
};

const CATEGORY_TONES: Record<PluginCategory, GlyphTone> = {
  documents: {
    wash: "bg-sky-100 dark:bg-sky-400/15",
    ink: "text-sky-700 dark:text-sky-300",
  },
  data: {
    wash: "bg-emerald-100 dark:bg-emerald-400/15",
    ink: "text-emerald-700 dark:text-emerald-300",
  },
  visualization: {
    wash: "bg-violet-100 dark:bg-violet-400/15",
    ink: "text-violet-700 dark:text-violet-300",
  },
  other: {
    wash: "bg-zinc-100 dark:bg-zinc-400/12",
    ink: "text-zinc-600 dark:text-zinc-300",
  },
};

const SKILL_TONE: GlyphTone = {
  wash: "bg-amber-100 dark:bg-amber-400/15",
  ink: "text-amber-700 dark:text-amber-300",
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
        "grid shrink-0 place-items-center ring-1 ring-inset ring-black/5 dark:ring-white/8",
        tone.wash,
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
        "grid shrink-0 place-items-center ring-1 ring-inset ring-black/5 dark:ring-white/8",
        SKILL_TONE.wash,
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
