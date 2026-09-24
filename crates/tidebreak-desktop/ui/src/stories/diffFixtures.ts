/**
 * Diffs for the diff review stories and tests, built from hunks so every
 * header's counts match its lines.
 */

type HunkSpec = {
  oldStart: number;
  newStart: number;
  /** Lines with their unified-diff marker: " ", "-", or "+". */
  lines: readonly string[];
  /** Text after the second `@@`, as git prints the enclosing function. */
  section?: string;
};

function hunkText({ oldStart, newStart, lines, section }: HunkSpec): string {
  const oldCount = lines.filter((line) => !line.startsWith("+")).length;
  const newCount = lines.filter((line) => !line.startsWith("-")).length;
  const suffix = section ? ` ${section}` : "";
  return [
    `@@ -${oldStart},${oldCount} +${newStart},${newCount} @@${suffix}`,
    ...lines,
  ].join("\n");
}

export function fileDiff(path: string, hunks: readonly HunkSpec[]): string {
  return [
    `diff --git a/${path} b/${path}`,
    "index 3b18e51..a9c4f02 100644",
    `--- a/${path}`,
    `+++ b/${path}`,
    ...hunks.map(hunkText),
  ].join("\n");
}

export const QUEUE_PATH =
  "crates/tidebreak-desktop/ui/src/code/sessionQueue.ts";

/** A TypeScript change with edits worth reading word by word. */
export const QUEUE_DIFF = fileDiff(QUEUE_PATH, [
  {
    oldStart: 12,
    newStart: 12,
    section: 'import { useRefreshSignals } from "../RefreshSignals";',
    lines: [
      " /**",
      "  * Messages that wait for the running turn to finish.",
      "- * The queue drains in the order the reader sent them.",
      "+ * The queue drains in the order the reader sent them, one per turn.",
      "  */",
      " export type QueuedMessage = {",
      "   id: string;",
      "-  text: string;",
      "+  message: string;",
      "+  attachments: readonly CodeTurnImage[];",
      "   createdAt: string;",
      " };",
      " ",
      "-const MAX_QUEUED = 10;",
      "+const MAX_QUEUED = 20;",
      " ",
      " export function enqueue(queue: QueuedMessage[], text: string) {",
      '-  if (queue.length >= MAX_QUEUED) throw new Error("The queue is full");',
      "-  return [...queue, { id: crypto.randomUUID(), text, createdAt: now() }];",
      "+  if (queue.length >= MAX_QUEUED) {",
      "+    throw new Error(`The queue holds ${MAX_QUEUED} messages at most`);",
      "+  }",
      "+  const message = { id: crypto.randomUUID(), message: text, createdAt: now() };",
      "+  return [...queue, { ...message, attachments: [] }];",
      " }",
    ],
  },
  {
    oldStart: 58,
    newStart: 64,
    section: "export function dequeue(queue: QueuedMessage[]) {",
    lines: [
      "   const [next, ...rest] = queue;",
      "   if (!next) return { next: null, rest };",
      "-  // Promote the oldest message; the tray reads the rest.",
      "+  // Promote the oldest message. The tray reads the rest on its next signal.",
      '   useRefreshSignals.getState().signal("queuedTurns");',
      "   return { next, rest };",
      " }",
    ],
  },
]);

export const CHECKPOINT_PATH =
  "crates/tidebreak-server/src/code/remote/checkpoints/periodic_push.rs";

/**
 * A Rust change whose hunks open inside a doc comment and carry a raw
 * string across lines: the fragments the highlighter must read whole.
 */
export const CHECKPOINT_DIFF = fileDiff(CHECKPOINT_PATH, [
  {
    oldStart: 1,
    newStart: 1,
    lines: [
      " /// Pushed about once a minute while a turn runs.",
      "-pub const PERIODIC_SECS: u64 = 90;",
      "+pub const PERIODIC_SECS: u64 = 60;",
      "+",
      "+/// The terminal push always runs, even after a failed turn.",
      ' pub const TERMINAL: &str = "terminal";',
    ],
  },
  {
    oldStart: 40,
    newStart: 43,
    section: "impl PeriodicPush {",
    lines: [
      "     /* The ref names a sandbox and its turn:",
      "        mg-wip/<sandbox>-i<turn> */",
      "     fn wip_ref(&self, turn: u32) -> String {",
      '-        format!("mg-wip/{}-{}", self.sandbox, turn)',
      '+        format!("mg-wip/{}-i{}", self.sandbox, turn)',
      "     }",
      " ",
      "     pub async fn push(&self, turn: u32) -> Result<(), PushError> {",
      '-        let script = r#"git push origin HEAD:refs/heads/wip"#;',
      '+        let script = r#"',
      "+            git push --force-with-lease origin \\",
      '+              "HEAD:refs/heads/$WIP_REF"',
      '+        "#;',
      '         self.run(script, &[("WIP_REF", self.wip_ref(turn))]).await',
      "     }",
    ],
  },
]);

export const LAYOUT_PATH = "crates/tidebreak-desktop/ui/src/code/layout.tsx";

/** A block moved under a condition: every line re-indented, two added. */
export const REINDENT_DIFF = fileDiff(LAYOUT_PATH, [
  {
    oldStart: 20,
    newStart: 20,
    section: "export function WorkspaceLayout({ inspector }: Props) {",
    lines: [
      "   const [open, setOpen] = useState(true);",
      "-  useEffect(() => {",
      '-    window.addEventListener("resize", onResize);',
      '-    return () => window.removeEventListener("resize", onResize);',
      "-  }, [onResize]);",
      "+  if (inspector) {",
      "+    useEffect(() => {",
      '+      window.addEventListener("resize", onResize);',
      '+      return () => window.removeEventListener("resize", onResize);',
      "+    }, [onResize]);",
      "+  }",
      "   return <Panel open={open} onOpenChange={setOpen} />;",
    ],
  },
  {
    oldStart: 61,
    newStart: 63,
    lines: [
      " function onResize() {",
      "-\tlayoutPanels();",
      "+  layoutPanels();",
      " }",
    ],
  },
]);

/** Only indentation changed: hiding whitespace leaves nothing. */
export const WHITESPACE_ONLY_DIFF = fileDiff(LAYOUT_PATH, [
  {
    oldStart: 61,
    newStart: 61,
    lines: [
      " function onResize() {",
      "-\tlayoutPanels();",
      "+  layoutPanels();",
      " }",
    ],
  },
]);

export const LOGO_PATH = "crates/tidebreak-desktop/ui/public/logo.png";

export const BINARY_DIFF = [
  `diff --git a/${LOGO_PATH} b/${LOGO_PATH}`,
  "index 7d1c2e0..4f0a9b3 100644",
  `Binary files a/${LOGO_PATH} and b/${LOGO_PATH} differ`,
].join("\n");

export const GENERATED_PATH =
  "crates/tidebreak-desktop/ui/src/generated/wire.ts";

/**
 * A 5,000-line diff of one file: a hundred hunks of edits, the size a
 * regenerated wire file or a wide rename produces.
 */
export function longFileDiff(lines = 5_000): string {
  const hunks: HunkSpec[] = [];
  let oldLine = 1;
  let newLine = 1;
  let produced = 0;
  let index = 0;
  while (produced < lines) {
    const body: string[] = [];
    for (let k = 0; k < 3; k += 1) {
      body.push(`   readonly field${index}_${k}: string;`);
    }
    for (let k = 0; k < 10; k += 1) {
      body.push(`-  readonly status${index}_${k}?: "open" | "closed";`);
    }
    for (let k = 0; k < 12; k += 1) {
      body.push(
        `+  readonly status${index}_${k}?: "open" | "closed" | "merged";`,
      );
    }
    for (let k = 0; k < 20; k += 1) {
      body.push(
        `   /** Field ${k} of record ${index}, as the server sends it. */`,
      );
    }
    for (let k = 0; k < 4; k += 1) {
      body.push(`+  readonly added${index}_${k}: number; // ${k * 7} bytes`);
    }
    body.push(" };");
    hunks.push({
      oldStart: oldLine,
      newStart: newLine,
      section: `export type Record${index} = {`,
      lines: body,
    });
    const oldCount = body.filter((line) => !line.startsWith("+")).length;
    const newCount = body.filter((line) => !line.startsWith("-")).length;
    oldLine += oldCount + 40;
    newLine += newCount + 40;
    produced += body.length + 1;
    index += 1;
  }
  return fileDiff(GENERATED_PATH, hunks);
}
