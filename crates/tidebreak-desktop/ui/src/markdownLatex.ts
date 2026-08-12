/**
 * Rewrite the `\[ … \]` and `\( … \)` LaTeX delimiters models often emit into
 * the `$$ … $$` / `$ … $` dollar delimiters that remark-math understands, and
 * escape stray text so a formula's prose (`\text{…}`) survives the KaTeX pass.
 * Delimiters that never close are written back out untouched so a mid-stream
 * fragment renders as its literal source rather than swallowing the rest of the
 * message.
 */
enum Brackets {
  SQUARE = "[",
  ROUND = "(",
}

const ESCAPE = "\\";

export function escapeLatexText(input: string): string {
  let result = "";
  let bracket: Brackets | null = null;
  let buffer = "";
  let char = "";

  for (let i = 0; i < input.length; ++i) {
    char = input[i]!;

    if (
      char === ESCAPE &&
      i + 1 < input.length &&
      input[i + 1] === Brackets.SQUARE
    ) {
      bracket = Brackets.SQUARE;
      i++;
      continue;
    }

    if (
      bracket !== null &&
      char === ESCAPE &&
      i + 1 < input.length &&
      input[i + 1] === "]"
    ) {
      bracket = null;
      result += `$$${escapeTextCommand(buffer)}$$`;
      buffer = "";
      i++;
      continue;
    }

    if (
      char === ESCAPE &&
      i + 1 < input.length &&
      input[i + 1] === Brackets.ROUND
    ) {
      bracket = Brackets.ROUND;
      i++;
      continue;
    }

    if (
      bracket !== null &&
      char === ESCAPE &&
      i + 1 < input.length &&
      input[i + 1] === ")"
    ) {
      bracket = null;
      result += `$${escapeTextCommand(buffer)}$`;
      buffer = "";
      i++;
      continue;
    }

    if (bracket !== null) {
      buffer += char;
    } else {
      result += char;
    }
  }

  // An unclosed delimiter: write the opener back out and flush its buffer.
  if (bracket !== null) {
    result += ESCAPE + bracket;
  }

  if (buffer.length > 0) {
    result += buffer;
  }

  return result;
}

const CHAR_MAP = new Map([
  ["_", ESCAPE + "_"],
  ["&", ESCAPE + "&"],
  ["%", ESCAPE + "%"],
  ["$", ESCAPE + "$"],
  ["#", ESCAPE + "#"],
  ["{", ESCAPE + "{"],
  ["^", ESCAPE + "^{}"],
  ["~", ESCAPE + "textasciitilde{}"],
]);

const END_CMD = new Set([" ", "{"]);

const escapeTextCommand = (input: string): string => {
  let recordCmd = false;
  let inText = false;
  let buffer = "";
  let result = "";
  let char = "";

  for (let i = 0; i < input.length; ++i) {
    char = input[i]!;

    if (inText) {
      if (char === "}") {
        result += buffer + char;
        buffer = "";
        inText = false;
        continue;
      }

      buffer += CHAR_MAP.get(char) ?? char;
      continue;
    }

    if (char === ESCAPE) {
      result += buffer + char;
      buffer = "";
      recordCmd = true;
      continue;
    }

    if (recordCmd) {
      if (END_CMD.has(char)) {
        if (buffer === "text") {
          inText = true;
        }
        result += buffer + char;
        buffer = "";
        continue;
      }

      buffer += char;
      continue;
    }

    result += char;
  }

  if (buffer.length > 0) {
    result += buffer;
  }

  return result;
};
