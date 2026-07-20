import {
  Children,
  isValidElement,
  type PropsWithChildren,
  type ReactElement,
  type ReactNode,
} from "react";
import { ClipboardCopyButton, copyPlainText } from "./ClipboardCopyButton";

export function MarkdownTable({ children }: PropsWithChildren) {
  const plainText = markdownTablePlainText(children);

  return (
    <div className="markdown-table-frame">
      <div className="markdown-table-wrap">
        <table>{children}</table>
      </div>
      {plainText && (
        <div className="markdown-table-actions">
          <ClipboardCopyButton
            value={plainText}
            label="Copy table contents"
            copiedAnnouncement="Table copied to clipboard."
            failedAnnouncement="Table could not be copied."
            className="markdown-table-copy"
          />
        </div>
      )}
    </div>
  );
}

export function markdownTablePlainText(children: ReactNode): string {
  const rows: string[] = [];
  collectRows(children, rows);
  return rows.length > 0 ? `${rows.join("\n")}\n` : "";
}

export async function copyMarkdownTable(
  children: ReactNode,
  clipboard: Parameters<typeof copyPlainText>[1],
): Promise<void> {
  const text = markdownTablePlainText(children);
  if (!text) throw new Error("Table has no visible text");
  await copyPlainText(text, clipboard);
}

function collectRows(node: ReactNode, rows: string[]): void {
  Children.forEach(node, (child) => {
    if (!isValidElement(child)) return;
    const element = child as ReactElement<PropsWithChildren>;
    if (element.type === "tr") {
      const cells: string[] = [];
      collectCells(element.props.children, cells);
      if (cells.some((cell) => cell.length > 0)) rows.push(cells.join("\t"));
      return;
    }
    collectRows(element.props.children, rows);
  });
}

function collectCells(node: ReactNode, cells: string[]): void {
  Children.forEach(node, (child) => {
    if (!isValidElement(child)) return;
    const element = child as ReactElement<PropsWithChildren>;
    if (element.type === "th" || element.type === "td") {
      cells.push(normalizeCellText(visibleText(element.props.children)));
      return;
    }
    collectCells(element.props.children, cells);
  });
}

function visibleText(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") {
    return String(node);
  }
  if (!node || typeof node === "boolean") return "";

  let text = "";
  Children.forEach(node, (child) => {
    if (typeof child === "string" || typeof child === "number") {
      text += String(child);
      return;
    }
    if (!isValidElement(child)) return;
    const element = child as ReactElement<PropsWithChildren>;
    text += element.type === "br" ? " " : visibleText(element.props.children);
  });
  return text;
}

function normalizeCellText(value: string): string {
  return value.replace(/\s+/g, " ").trim();
}
