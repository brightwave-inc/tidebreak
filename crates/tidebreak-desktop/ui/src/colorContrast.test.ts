import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { VariantProps } from "class-variance-authority";
import { describe, expect, it } from "vitest";

import {
  STATUS_CHIP,
  STATUS_MARK,
  STATUS_TEXT,
  STATUS_TEXT_MUTED,
} from "./code/statusTone";
import { badgeVariants } from "./components/ui/badge";
import { buttonVariants } from "./components/ui/button";
import { cn } from "./lib/utils";

/**
 * Color contrast, computed from the tokens in styles.css.
 *
 * DESIGN.md sets the palette; this test holds it to WCAG 2 contrast in both
 * themes. It reads `:root` and `.dark`, resolves each token to an sRGB color,
 * lays any alpha over the ground it sits on, and checks the ratio: 4.5:1 for
 * text, 3:1 for marks and focus rings.
 *
 * It also reads the maps components paint with — `STATUS_TEXT`, `STATUS_MARK`,
 * `STATUS_CHIP`, and the Badge and Button variants — so a chip or a button
 * cannot drift onto a failing pair while every token still passes alone.
 *
 * If a pair fails, fix the token or the pairing. Do not drop the pair.
 */

const CSS = readFileSync(join(import.meta.dirname, "styles.css"), "utf8");

type Theme = "light" | "dark";
const THEMES: Theme[] = ["light", "dark"];

/** Text needs 4.5:1; marks, dots, and focus indicators need 3:1. */
const TEXT = 4.5;
const MARK = 3;

/** Where text and marks rest: the canvas and the reading surfaces. */
const SURFACES = ["page-background", "background", "card", "popover"];
/** Recessed regions and hovered rows. Text on them has to stay readable. */
const ROWS = ["muted", "accent"];
const GROUNDS = [...SURFACES, ...ROWS];

const TONES = ["success", "warning", "critical", "info", "merged", "live"];

type BadgeVariant = NonNullable<VariantProps<typeof badgeVariants>["variant"]>;
const BADGE_VARIANTS: BadgeVariant[] = [
  "default",
  "secondary",
  "destructive",
  "outline",
  "success",
  "warning",
  "critical",
  "info",
  "merged",
  "live",
];

type ButtonVariant = NonNullable<
  VariantProps<typeof buttonVariants>["variant"]
>;
const BUTTON_VARIANTS: ButtonVariant[] = [
  "default",
  "destructive",
  "ghost-destructive",
  "outline",
  "secondary",
  "ghost",
  "link",
];

// ---------------------------------------------------------------------------
// Reading the tokens

function block(selector: string): string {
  const start = CSS.indexOf(`\n${selector} {`);
  if (start < 0) throw new Error(`styles.css has no top-level ${selector}`);
  const open = CSS.indexOf("{", start);
  let depth = 0;
  for (let index = open; index < CSS.length; index += 1) {
    if (CSS[index] === "{") depth += 1;
    if (CSS[index] === "}") depth -= 1;
    if (depth === 0) return CSS.slice(open + 1, index);
  }
  throw new Error(`unterminated ${selector} block`);
}

function declarations(body: string): Map<string, string> {
  const tokens = new Map<string, string>();
  const source = body.replace(/\/\*[\s\S]*?\*\//g, "");
  for (const match of source.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    tokens.set(match[1], match[2].replace(/\s+/g, " ").trim());
  }
  return tokens;
}

const ROOT = declarations(block(":root"));
/** `.dark` sits on the root element with `:root`, so it inherits the rest. */
const TOKENS: Record<Theme, Map<string, string>> = {
  light: ROOT,
  dark: new Map([...ROOT, ...declarations(block(".dark"))]),
};

// ---------------------------------------------------------------------------
// Color math

/** oklch with alpha. Hue is null when the color has none (chroma 0). */
type Paint = { l: number; c: number; h: number | null; alpha: number };
type Rgb = [number, number, number];

function splitArgs(text: string): string[] {
  const args: string[] = [];
  let depth = 0;
  let current = "";
  for (const char of text) {
    if (char === "(") depth += 1;
    if (char === ")") depth -= 1;
    if (char === "," && depth === 0) {
      args.push(current.trim());
      current = "";
    } else {
      current += char;
    }
  }
  args.push(current.trim());
  return args;
}

function number(text: string): number {
  return text.endsWith("%") ? Number(text.slice(0, -1)) / 100 : Number(text);
}

function resolve(theme: Theme, value: string, depth = 0): Paint {
  if (depth > 20) throw new Error(`token cycle at ${value}`);
  const text = value.trim();
  if (text === "transparent") return { l: 0, c: 0, h: null, alpha: 0 };

  const reference = /^var\((--[\w-]+)\)$/.exec(text);
  if (reference) {
    const token = TOKENS[theme].get(reference[1]);
    if (token === undefined) {
      throw new Error(`${reference[1]} is not defined in the ${theme} theme`);
    }
    return resolve(theme, token, depth + 1);
  }

  const oklch = /^oklch\(\s*(\S+)\s+(\S+)\s+(\S+)\s*(?:\/\s*(\S+)\s*)?\)$/.exec(
    text,
  );
  if (oklch) {
    const c = number(oklch[2]);
    return {
      l: number(oklch[1]),
      c,
      h: c < 1e-4 ? null : Number(oklch[3]),
      alpha: oklch[4] === undefined ? 1 : number(oklch[4]),
    };
  }

  const mix = /^color-mix\(\s*in\s+ok(?:lch|lab)\s*,(.*)\)$/.exec(text);
  if (mix) {
    const [first, second] = splitArgs(mix[1]).map((arg) => {
      const weighted = /^(.*?)\s+([\d.]+%)$/.exec(arg);
      return weighted
        ? {
            paint: resolve(theme, weighted[1], depth + 1),
            weight: number(weighted[2]),
          }
        : { paint: resolve(theme, arg, depth + 1), weight: null };
    });
    const p1 =
      first.weight ?? (second.weight === null ? 0.5 : 1 - second.weight);
    const p2 = second.weight ?? 1 - p1;
    return mixPaints(first.paint, p1, second.paint, p2);
  }

  throw new Error(`cannot read ${text} (${theme})`);
}

/** CSS Color 4 `color-mix` in oklch, with premultiplied alpha. */
function mixPaints(a: Paint, pa: number, b: Paint, pb: number): Paint {
  const scale = pa + pb;
  const wa = pa / scale;
  const wb = pb / scale;
  const alpha = a.alpha * wa + b.alpha * wb;
  if (alpha === 0) return { l: 0, c: 0, h: null, alpha: 0 };
  const premultiplied = (x: number, y: number) =>
    (x * a.alpha * wa + y * b.alpha * wb) / alpha;
  let h: number | null;
  if (a.h === null || b.h === null) {
    h = a.h ?? b.h;
  } else {
    let delta = b.h - a.h;
    if (delta > 180) delta -= 360;
    if (delta < -180) delta += 360;
    h = (a.h + delta * wb + 360) % 360;
  }
  return { l: premultiplied(a.l, b.l), c: premultiplied(a.c, b.c), h, alpha };
}

function withAlpha(paint: Paint, alpha: number): Paint {
  return { ...paint, alpha: paint.alpha * alpha };
}

/**
 * oklch to gamma-encoded sRGB. A color outside sRGB is clipped per channel,
 * which is what a screen without wide gamut shows.
 */
function toRgb(paint: Paint): Rgb {
  const hue = ((paint.h ?? 0) * Math.PI) / 180;
  const a = paint.c * Math.cos(hue);
  const b = paint.c * Math.sin(hue);
  const l = (paint.l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (paint.l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (paint.l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const linear: Rgb = [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
  return linear.map((channel) => {
    const x = Math.min(1, Math.max(0, channel));
    return x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055;
  }) as Rgb;
}

/** Alpha compositing, in gamma-encoded sRGB as browsers do it. */
function over(paint: Paint, ground: Rgb): Rgb {
  const rgb = toRgb(paint);
  return rgb.map(
    (channel, index) =>
      paint.alpha * channel + (1 - paint.alpha) * ground[index],
  ) as Rgb;
}

function luminance([r, g, b]: Rgb): number {
  const linear = (x: number) =>
    x <= 0.04045 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4;
  return 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
}

function ratio(a: Rgb, b: Rgb): number {
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (light + 0.05) / (dark + 0.05);
}

// ---------------------------------------------------------------------------
// Pairs

function token(theme: Theme, name: string): Paint {
  return resolve(theme, `var(--${name})`);
}

/** A ground, flattened onto the canvas when it carries alpha. */
function groundRgb(theme: Theme, ground: string): Rgb {
  const canvas = toRgb(token(theme, "page-background"));
  return over(token(theme, ground), canvas);
}

/** An ink over an optional fill over a ground, as the reader sees it. */
function contrastOf(ink: Paint, fill: Paint | undefined, ground: Rgb): number {
  const backdrop = fill ? over(fill, ground) : ground;
  return ratio(over(ink, backdrop), backdrop);
}

type Failure = string;

function check(
  failures: Failure[],
  label: string,
  measured: number,
  minimum: number,
) {
  if (measured < minimum) {
    failures.push(`${label}: ${measured.toFixed(2)}:1, needs ${minimum}:1`);
  }
}

function tokenPairs(
  inks: string[],
  grounds: (ink: string) => string[],
  minimum: number,
): Failure[] {
  const failures: Failure[] = [];
  for (const theme of THEMES) {
    for (const ink of inks) {
      for (const ground of grounds(ink)) {
        check(
          failures,
          `${theme}: --${ink} on --${ground}`,
          contrastOf(token(theme, ink), undefined, groundRgb(theme, ground)),
          minimum,
        );
      }
    }
  }
  return failures;
}

// ---------------------------------------------------------------------------
// Reading component classes

/**
 * The color a class list paints for one property and state, e.g. the text
 * color of a button on hover. The last matching utility wins, as it does once
 * `cn` has merged the list. Returns undefined when nothing sets it.
 */
function classPaint(
  theme: Theme,
  classes: string,
  property: "bg" | "text" | "border",
  state?: "hover" | "focus-visible",
): Paint | undefined {
  const prefix = state ? `${state}:${property}-` : `${property}-`;
  let found: Paint | undefined;
  for (const utility of classes.split(/\s+/)) {
    if (!utility.startsWith(prefix)) continue;
    const value = utility.slice(prefix.length);
    const arbitrary = /^\[(.*)\]$/.exec(value);
    if (arbitrary) {
      found = resolve(theme, arbitrary[1].replaceAll("_", " "));
      continue;
    }
    const [name, alpha] = value.split("/");
    if (!TOKENS[theme].has(`--${name}`)) continue;
    const paint = token(theme, name);
    found = alpha === undefined ? paint : withAlpha(paint, Number(alpha) / 100);
  }
  return found;
}

function classPairs(
  label: string,
  classes: string,
  grounds: string[],
  options: { state?: "hover"; inheritedInk?: string } = {},
): Failure[] {
  const failures: Failure[] = [];
  for (const theme of THEMES) {
    const ink =
      (options.state && classPaint(theme, classes, "text", options.state)) ??
      classPaint(theme, classes, "text") ??
      token(theme, options.inheritedInk ?? "foreground");
    const fill =
      (options.state && classPaint(theme, classes, "bg", options.state)) ??
      classPaint(theme, classes, "bg");
    // An opaque fill hides the ground, so one ground says it all.
    const under = fill?.alpha === 1 ? grounds.slice(0, 1) : grounds;
    for (const ground of under) {
      check(
        failures,
        fill?.alpha === 1
          ? `${theme}: ${label}`
          : `${theme}: ${label} on --${ground}`,
        contrastOf(ink, fill, groundRgb(theme, ground)),
        TEXT,
      );
    }
  }
  return failures;
}

// ---------------------------------------------------------------------------

describe("token contrast in both themes (see DESIGN.md)", () => {
  it("keeps body text readable on every surface and row", () => {
    expect(
      tokenPairs(["foreground", "muted-foreground"], () => GROUNDS, TEXT),
    ).toEqual([]);
  });

  it("keeps error ink readable as text", () => {
    // `text-destructive` and `text-critical` carry error messages and
    // destructive menu items, so the critical mark has to read as text too.
    expect(
      tokenPairs(["destructive", "critical"], () => GROUNDS, TEXT),
    ).toEqual([]);
  });

  it("keeps each fill's own foreground readable on it", () => {
    const pairs: [string, string][] = [
      ["primary-foreground", "primary"],
      ["secondary-foreground", "secondary"],
      ["accent-foreground", "accent"],
      ["card-foreground", "card"],
      ["popover-foreground", "popover"],
      ["destructive-foreground", "destructive"],
    ];
    const failures = pairs.flatMap(([ink, fill]) =>
      tokenPairs([ink], () => [fill], TEXT),
    );
    expect(failures).toEqual([]);
  });

  it("keeps status text readable on surfaces, rows, and its own tint", () => {
    const inks = TONES.flatMap((tone) => [
      `${tone}-foreground`,
      `${tone}-foreground-muted`,
    ]);
    expect(
      tokenPairs(
        inks,
        (ink) => [
          ...GROUNDS,
          `${ink.replace(/-foreground(-muted)?$/, "")}-background`,
        ],
        TEXT,
      ),
    ).toEqual([]);
  });

  it("keeps status marks and the focus ring visible on every ground", () => {
    expect(tokenPairs([...TONES, "ring"], () => GROUNDS, MARK)).toEqual([]);
  });

  it("keeps the unchecked switch track visible on the page and card", () => {
    expect(tokenPairs(["switch-track"], () => SURFACES, MARK)).toEqual([]);
  });

  it("keeps search matches readable on their marks", () => {
    // `.search-match` and `::highlight(transcript-find)` paint foreground ink
    // on the citation tint; the match being pointed at inverts to background
    // ink on the foreground.
    expect([
      ...tokenPairs(["foreground"], () => ["citation-mark"], TEXT),
      ...tokenPairs(["background"], () => ["foreground"], TEXT),
    ]).toEqual([]);
  });
});

describe("status tones keep their hue (see DESIGN.md)", () => {
  // Contrast alone let the dark theme swap the light values: text went
  // near-white and lost its color, and tints turned into saturated slabs.
  it("gives status text enough chroma to read as its tone", () => {
    const flat = THEMES.flatMap((theme) =>
      TONES.map((tone) => ({
        theme,
        tone,
        paint: token(theme, `${tone}-foreground`),
      }))
        .filter(({ paint }) => paint.c < 0.07)
        .map(
          ({ theme, tone, paint }) =>
            `${theme}: --${tone}-foreground chroma ${paint.c}`,
        ),
    );
    expect(flat).toEqual([]);
  });

  it("keeps status backgrounds a quiet tint near the canvas", () => {
    const slabs = THEMES.flatMap((theme) => {
      const canvas = token(theme, "page-background");
      return TONES.map((tone) => ({
        tone,
        paint: token(theme, `${tone}-background`),
      }))
        .filter(
          ({ paint }) => paint.c > 0.06 || Math.abs(paint.l - canvas.l) > 0.1,
        )
        .map(
          ({ tone, paint }) =>
            `${theme}: --${tone}-background oklch(${paint.l} ${paint.c} ${paint.h})`,
        );
    });
    expect(slabs).toEqual([]);
  });
});

describe("component color maps (see DESIGN.md)", () => {
  it("paints status text with a readable rung", () => {
    const failures = Object.entries(STATUS_TEXT).flatMap(([tone, classes]) =>
      classPairs(`STATUS_TEXT.${tone}`, classes, GROUNDS),
    );
    expect(failures).toEqual([]);
  });

  it("paints muted status text with a readable rung", () => {
    const failures = Object.entries(STATUS_TEXT_MUTED).flatMap(
      ([tone, classes]) =>
        classPairs(`STATUS_TEXT_MUTED.${tone}`, classes, GROUNDS),
    );
    expect(failures).toEqual([]);
  });

  it("paints status marks that stay visible", () => {
    const failures: Failure[] = [];
    for (const theme of THEMES) {
      for (const [tone, classes] of Object.entries(STATUS_MARK)) {
        const mark = classPaint(theme, classes, "text");
        if (!mark) throw new Error(`STATUS_MARK.${tone} sets no color`);
        for (const ground of GROUNDS) {
          check(
            failures,
            `${theme}: STATUS_MARK.${tone} on --${ground}`,
            contrastOf(mark, undefined, groundRgb(theme, ground)),
            MARK,
          );
        }
      }
    }
    expect(failures).toEqual([]);
  });

  it("keeps chip text readable on the chip", () => {
    const failures = Object.entries(STATUS_CHIP).flatMap(([tone, classes]) =>
      classPairs(`STATUS_CHIP.${tone}`, classes, SURFACES),
    );
    expect(failures).toEqual([]);
  });

  it("keeps every Badge readable", () => {
    const failures = BADGE_VARIANTS.flatMap((variant) =>
      classPairs(`Badge ${variant}`, badgeVariants({ variant }), SURFACES),
    );
    expect(failures).toEqual([]);
  });

  it("keeps every Button label readable at rest and on hover", () => {
    const failures = BUTTON_VARIANTS.flatMap((variant) => {
      const classes = cn(buttonVariants({ variant }));
      return [
        ...classPairs(`Button ${variant}`, classes, SURFACES),
        ...classPairs(`Button ${variant} hover`, classes, SURFACES, {
          state: "hover",
        }),
      ];
    });
    expect(failures).toEqual([]);
  });

  it("draws every Button's focus border at 3:1 against its ground", () => {
    const failures: Failure[] = [];
    for (const theme of THEMES) {
      for (const variant of BUTTON_VARIANTS) {
        const classes = cn(buttonVariants({ variant }));
        const border = classPaint(theme, classes, "border", "focus-visible");
        if (!border) throw new Error(`Button ${variant} draws no focus border`);
        for (const ground of SURFACES) {
          check(
            failures,
            `${theme}: Button ${variant} focus border on --${ground}`,
            contrastOf(border, undefined, groundRgb(theme, ground)),
            MARK,
          );
        }
      }
    }
    expect(failures).toEqual([]);
  });
});

describe("diff grounds and syntax ink (see DESIGN.md)", () => {
  const CODE_INKS = [
    "foreground",
    "syntax-keyword",
    "syntax-string",
    "syntax-comment",
    "syntax-number",
    "syntax-title",
    "syntax-attr",
  ];

  function layer(theme: Theme, name: string, under: Rgb): Rgb {
    return over(token(theme, name), under);
  }

  /** Every ground a line of code sits on, composited the way the view paints it. */
  function codeGrounds(theme: Theme): Array<[string, Rgb]> {
    const background = groundRgb(theme, "background");
    const added = layer(theme, "diff-add-row", background);
    const removed = layer(theme, "diff-del-row", background);
    return [
      ["the plain row", background],
      ["an added row", added],
      ["a removed row", removed],
      ["an added word", layer(theme, "diff-add-word", added)],
      ["a removed word", layer(theme, "diff-del-word", removed)],
      ["a selected row", layer(theme, "diff-selected", background)],
      ["a selected added row", layer(theme, "diff-selected", added)],
      ["a selected removed row", layer(theme, "diff-selected", removed)],
    ];
  }

  it("keeps code and every syntax role readable on every diff ground", () => {
    const failures: Failure[] = [];
    for (const theme of THEMES) {
      for (const [label, ground] of codeGrounds(theme)) {
        for (const ink of CODE_INKS) {
          check(
            failures,
            `${theme}: --${ink} on ${label}`,
            contrastOf(token(theme, ink), undefined, ground),
            TEXT,
          );
        }
      }
    }
    expect(failures).toEqual([]);
  });

  it("keeps markers, line numbers, and hunk headers readable", () => {
    const failures: Failure[] = [];
    for (const theme of THEMES) {
      const background = groundRgb(theme, "background");
      const added = layer(theme, "diff-add-row", background);
      const removed = layer(theme, "diff-del-row", background);
      const gutter = (under: Rgb) => layer(theme, "diff-gutter", under);
      const pairs: Array<[string, string, Rgb]> = [
        ["success-foreground", "an added row", added],
        ["critical-foreground", "a removed row", removed],
        [
          "info-foreground",
          "a hunk header",
          layer(theme, "diff-hunk-row", background),
        ],
        ["muted-foreground", "the gutter", gutter(background)],
        ["muted-foreground", "an added row's gutter", gutter(added)],
        ["muted-foreground", "a removed row's gutter", gutter(removed)],
        [
          "foreground",
          "a selected row's gutter",
          layer(theme, "diff-selected", gutter(added)),
        ],
      ];
      for (const [ink, label, ground] of pairs) {
        check(
          failures,
          `${theme}: --${ink} on ${label}`,
          contrastOf(token(theme, ink), undefined, ground),
          TEXT,
        );
      }
    }
    expect(failures).toEqual([]);
  });
});
