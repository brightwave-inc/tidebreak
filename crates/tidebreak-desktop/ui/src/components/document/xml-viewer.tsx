import {
  ChevronsDownUp,
  ChevronDown,
  ChevronRight,
  ChevronRightIcon,
  Loader2Icon,
} from "lucide-react";
import {
  createContext,
  type HTMLAttributes,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  useFileDownload,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { cn } from "@/lib/utils";
import { FileDownloadProgressIndicator } from "./FileDownloadProgress";

// ---------------------------------------------------------------------------
// Collapse signaling
// ---------------------------------------------------------------------------

type CollapseSignal = { counter: number };
const CollapseSignalContext = createContext<CollapseSignal>({ counter: 0 });

// ---------------------------------------------------------------------------
// Navigate-to-path signaling — expands + scrolls to a specific element
// ---------------------------------------------------------------------------

type NavigateSignal = { counter: number; path: string };
const NavigateSignalContext = createContext<NavigateSignal>({
  counter: 0,
  path: "",
});

// ---------------------------------------------------------------------------
// Highlight signaling
// ---------------------------------------------------------------------------

const HighlightSignalContext = createContext<string | undefined>(undefined);

type HighlightRefSetter = (el: HTMLDivElement | null) => void;
const HighlightRefSetterContext = createContext<HighlightRefSetter>(() => {});

// ---------------------------------------------------------------------------
// XML DOM helpers
// ---------------------------------------------------------------------------

/** Build an XPath-like address for a DOM node, e.g. "/root/items/item[2]" */
function buildNodePath(node: Node): string {
  const parts: string[] = [];
  let current: Node | null = node;
  while (current && current.nodeType === Node.ELEMENT_NODE) {
    const el = current as Element;
    const parent = el.parentNode;
    if (!parent || parent.nodeType === Node.DOCUMENT_NODE) {
      parts.unshift("/" + el.tagName);
      break;
    }
    // Count preceding siblings with same tag name for positional index
    let idx = 1;
    let sibling = el.previousSibling;
    while (sibling) {
      if (
        sibling.nodeType === Node.ELEMENT_NODE &&
        (sibling as Element).tagName === el.tagName
      ) {
        idx++;
      }
      sibling = sibling.previousSibling;
    }
    // Count total siblings with same tag
    let total = 0;
    const children = parent.childNodes;
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      if (
        child?.nodeType === Node.ELEMENT_NODE &&
        (child as Element).tagName === el.tagName
      ) {
        total++;
      }
    }
    parts.unshift("/" + el.tagName + (total > 1 ? `[${idx}]` : ""));
    current = parent;
  }
  return parts.join("");
}

/** Check if a path is an ancestor of (or equal to) the highlight path */
function isOnHighlightPath(
  currentPath: string,
  highlightPath: string,
): boolean {
  if (!highlightPath) return false;
  if (currentPath === highlightPath) return true;
  return highlightPath.startsWith(currentPath + "/");
}

function isExactHighlightPath(
  currentPath: string,
  highlightPath: string,
): boolean {
  return !!highlightPath && currentPath === highlightPath;
}

function isOnNavigatePath(currentPath: string, navigatePath: string): boolean {
  if (!navigatePath) return false;
  if (currentPath === navigatePath) return true;
  return navigatePath.startsWith(currentPath + "/");
}

/**
* Resolve a simplified XPath to the element path format we use internally.
*
* We use path strings like "/root/items/item[2]" for both internal addressing
* and for matching XPath highlight paths from the backend.
*/
function resolveXPathToNodePath(
  xmlDoc: Document,
  xpath: string,
): string | undefined {
  try {
    const result = xmlDoc.evaluate(
      xpath,
      xmlDoc,
      null,
      XPathResult.FIRST_ORDERED_NODE_TYPE,
      null,
    );
    const node = result.singleNodeValue;
    if (node && node.nodeType === Node.ELEMENT_NODE) {
      return buildNodePath(node);
    }
  } catch {
    // Invalid XPath — fall through
  }
  return undefined;
}

/** Get child elements of a DOM element */
function getChildElements(el: Element): Element[] {
  const children: Element[] = [];
  for (let i = 0; i < el.childNodes.length; i++) {
    const child = el.childNodes[i];
    if (child?.nodeType === Node.ELEMENT_NODE) {
      children.push(child as Element);
    }
  }
  return children;
}

/** Get the text content directly owned by this element (not from descendants) */
function getDirectTextContent(el: Element): string {
  let text = "";
  for (let i = 0; i < el.childNodes.length; i++) {
    const child = el.childNodes[i];
    if (child?.nodeType === Node.TEXT_NODE) {
      text += child.textContent ?? "";
    }
  }
  return text.trim();
}

/** Get attributes as key-value pairs */
function getAttributes(el: Element): [string, string][] {
  const attrs: [string, string][] = [];
  for (let i = 0; i < el.attributes.length; i++) {
    const attr = el.attributes[i];
    if (attr) {
      attrs.push([attr.name, attr.value]);
    }
  }
  return attrs;
}

// ---------------------------------------------------------------------------
// Main component
// ---------------------------------------------------------------------------

interface XmlViewerProps extends HTMLAttributes<HTMLDivElement> {
  source: FileBytesSource;
  /** XPath expression to highlight, e.g. "/root/items/item[1]" */
  highlightPath?: string;
}

export function XmlViewer({
  source,
  highlightPath,
  className,
  ...props
}: XmlViewerProps) {
  const fileDownload = useFileDownload(source, {
    parseAs: "text",
  });

  const xmlDoc = useMemo(() => {
    if (!fileDownload.data) return undefined;
    try {
      const parser = new DOMParser();
      const doc = parser.parseFromString(
        fileDownload.data,
        "application/xml",
      );
      // Check for parse errors
      if (doc.querySelector("parsererror")) return undefined;
      return doc;
    } catch {
      return undefined;
    }
  }, [fileDownload.data]);

  // Resolve the XPath highlight to our internal node path format
  const resolvedHighlightPath = useMemo(() => {
    if (!highlightPath || !xmlDoc) return undefined;
    return resolveXPathToNodePath(xmlDoc, highlightPath);
  }, [highlightPath, xmlDoc]);

  const highlightRef = useRef<HTMLDivElement>(null);
  const setHighlightEl = useCallback((el: HTMLDivElement | null) => {
    highlightRef.current = el;
  }, []);
  const nodeRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  const [collapseSignal, setCollapseSignal] = useState<CollapseSignal>({
    counter: 0,
  });
  const [navigateSignal, setNavigateSignal] = useState<NavigateSignal>({
    counter: 0,
    path: "",
  });

  const collapseAll = useCallback(
    () => setCollapseSignal((s) => ({ counter: s.counter + 1 })),
    [],
  );

  const navigateToPath = useCallback((path: string) => {
    setNavigateSignal((s) => ({ counter: s.counter + 1, path }));
    const tryScroll = (attempts: number) => {
      const el = nodeRefs.current.get(path);
      if (el) {
        el.scrollIntoView({ behavior: "smooth", block: "start" });
      } else if (attempts < 10) {
        requestAnimationFrame(() => tryScroll(attempts + 1));
      }
    };
    requestAnimationFrame(() => tryScroll(0));
  }, []);

  const registerNodeRef = useCallback(
    (path: string, el: HTMLDivElement | null) => {
      if (el) {
        nodeRefs.current.set(path, el);
      } else {
        nodeRefs.current.delete(path);
      }
    },
    [],
  );

  useEffect(() => {
    if (highlightRef.current) {
      highlightRef.current.scrollIntoView({
        behavior: "smooth",
        block: "center",
      });
    }
  }, [resolvedHighlightPath, xmlDoc]);

  if (fileDownload.error) {
    return (
      <div className={cn("relative overflow-auto", className)} {...props}>
        <div className="text-muted-foreground flex h-64 items-center justify-center">
          <p>Failed to download document</p>
        </div>
      </div>
    );
  }

  if (fileDownload.isLoading) {
    return (
      <div
        className={cn("flex items-center justify-center", className)}
        {...props}
      >
        {fileDownload.progress ? (
          <FileDownloadProgressIndicator progress={fileDownload.progress} />
        ) : (
          <Loader2Icon className="text-muted-foreground size-6 animate-spin" />
        )}
      </div>
    );
  }

  if (!xmlDoc || !xmlDoc.documentElement) {
    return (
      <div className={cn("relative overflow-auto", className)} {...props}>
        <div className="text-muted-foreground flex h-64 items-center justify-center">
          <p>Unable to parse XML</p>
        </div>
      </div>
    );
  }

  return (
    <div
      className={cn("flex grow flex-col overflow-hidden", className)}
      {...props}
    >
      <XmlBreadcrumbBar
        root={xmlDoc.documentElement}
        onCollapseAll={collapseAll}
        onNavigate={navigateToPath}
      />
      <div className="bg-page-background grow overflow-auto p-6 font-mono text-sm">
        <HighlightSignalContext.Provider value={resolvedHighlightPath}>
          <HighlightRefSetterContext.Provider value={setHighlightEl}>
            <CollapseSignalContext.Provider value={collapseSignal}>
              <NavigateSignalContext.Provider
                value={navigateSignal}
              >
                <XmlElementNode
                  element={xmlDoc.documentElement}
                  path={buildNodePath(xmlDoc.documentElement)}
                  registerNodeRef={registerNodeRef}
                  depth={0}
                />
              </NavigateSignalContext.Provider>
            </CollapseSignalContext.Provider>
          </HighlightRefSetterContext.Provider>
        </HighlightSignalContext.Provider>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Breadcrumb navigation bar
// ---------------------------------------------------------------------------

function XmlBreadcrumbBar({
  root,
  onCollapseAll,
  onNavigate,
}: {
  root: Element;
  onCollapseAll: () => void;
  onNavigate: (path: string) => void;
}) {
  const [breadcrumb, setBreadcrumb] = useState<
    { element: Element; path: string }[]
  >([]);

  useEffect(() => setBreadcrumb([]), [root]);

  const handleSelect = useCallback(
    (depth: number, element: Element, path: string) => {
      setBreadcrumb((prev) => [
        ...prev.slice(0, depth),
        { element, path },
      ]);
      onNavigate(path);
    },
    [onNavigate],
  );

  const segments = useMemo(() => {
    const result: {
      parentElement: Element;
      children: Element[];
      selectedPath: string | null;
    }[] = [];

    // Root level
    const rootChildren = getChildElements(root);
    if (rootChildren.length > 0) {
      result.push({
        parentElement: root,
        children: rootChildren,
        selectedPath: breadcrumb[0]?.path ?? null,
      });
    }

    // Each navigated level
    for (let i = 0; i < breadcrumb.length; i++) {
      const entry = breadcrumb[i];
      if (!entry) break;
      const children = getChildElements(entry.element);
      if (children.length === 0) break;
      result.push({
        parentElement: entry.element,
        children,
        selectedPath: breadcrumb[i + 1]?.path ?? null,
      });
    }

    return result;
  }, [root, breadcrumb]);

  return (
    <div className="bg-muted/50 flex items-center gap-0.5 border-b px-2 py-1">
      <Button
        variant="ghost"
        size="icon-xs"
        onClick={onCollapseAll}
        title="Collapse all"
      >
        <ChevronsDownUp className="size-3.5" />
        <span className="sr-only">Collapse all</span>
      </Button>
      <div className="bg-border mx-1 h-4 w-px" />
      <div className="flex min-w-0 items-center gap-0.5 overflow-x-auto">
        {segments.map((seg, i) => (
          <XmlBreadcrumbSegment
            key={
              i === 0
                ? "__root"
                : (breadcrumb[i - 1]?.path ?? "__root")
            }
            depth={i}
            children={seg.children}
            selectedPath={seg.selectedPath}
            onSelect={handleSelect}
            showSeparator={i > 0}
          />
        ))}
      </div>
    </div>
  );
}

function XmlBreadcrumbSegment({
  depth,
  children,
  selectedPath,
  onSelect,
  showSeparator,
}: {
  depth: number;
  children: Element[];
  selectedPath: string | null;
  onSelect: (depth: number, element: Element, path: string) => void;
  showSeparator: boolean;
}) {
  const [open, setOpen] = useState(false);

  const selectedLabel = useMemo(() => {
    if (!selectedPath)
      return `<${children[0]?.parentElement?.tagName ?? "..."}>`;
    const selected = children.find(
      (child) => buildNodePath(child) === selectedPath,
    );
    return selected ? `<${selected.tagName}>` : "...";
  }, [selectedPath, children]);

  return (
    <>
      {showSeparator && (
        <ChevronRightIcon className="text-muted-foreground size-3 shrink-0" />
      )}
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <button
            className={cn(
              "text-muted-foreground hover:text-foreground hover:bg-muted shrink-0 rounded px-1.5 py-0.5 font-mono text-xs transition-colors",
              selectedPath && "text-foreground",
            )}
          >
            {selectedLabel}
          </button>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="max-h-64 w-56 overflow-y-auto p-1"
        >
          {children.map((child) => {
            const childPath = buildNodePath(child);
            const childElements = getChildElements(child);
            const attrs = getAttributes(child);
            const attrPreview =
              attrs.length > 0
                ? ` ${attrs[0]?.[0]}="${attrs[0]?.[1]}"${attrs.length > 1 ? " ..." : ""}`
                : "";
            return (
              <button
                key={childPath}
                onClick={() => {
                  onSelect(depth, child, childPath);
                  setOpen(false);
                }}
                className={cn(
                  "hover:bg-muted flex w-full items-center gap-1 rounded px-2 py-1 text-left font-mono text-xs",
                  childPath === selectedPath && "bg-muted",
                )}
              >
                <span className="text-rose-600 dark:text-rose-400">
                  &lt;{child.tagName}
                  {attrPreview}&gt;
                </span>
                {childElements.length > 0 && (
                  <span className="text-muted-foreground">
                    ({childElements.length})
                  </span>
                )}
              </button>
            );
          })}
        </PopoverContent>
      </Popover>
    </>
  );
}

// ---------------------------------------------------------------------------
// Recursive XML element renderer
// ---------------------------------------------------------------------------

interface XmlElementNodeProps {
  element: Element;
  path: string;
  registerNodeRef?: (path: string, el: HTMLDivElement | null) => void;
  depth: number;
  isLast?: boolean;
}

function XmlElementNode({
  element,
  path,
  registerNodeRef,
  depth,
  isLast: _isLast = true,
}: XmlElementNodeProps) {
  const collapseSignal = useContext(CollapseSignalContext);
  const navigateSignal = useContext(NavigateSignalContext);
  const highlightPath = useContext(HighlightSignalContext);

  const isHighlighted = highlightPath
    ? isExactHighlightPath(path, highlightPath)
    : false;
  const isAncestor = highlightPath
    ? isOnHighlightPath(path, highlightPath) && !isHighlighted
    : false;

  const childElements = useMemo(() => getChildElements(element), [element]);
  const attrs = useMemo(() => getAttributes(element), [element]);
  const directText = useMemo(() => getDirectTextContent(element), [element]);
  const hasChildren = childElements.length > 0;

  const [expanded, setExpanded] = useState(
    depth < 1 || isAncestor || isHighlighted,
  );

  useEffect(() => {
    if (collapseSignal.counter === 0) return;
    setExpanded(depth < 1);
  }, [collapseSignal, depth]);

  useEffect(() => {
    if (navigateSignal.counter === 0 || !path) return;
    if (isOnNavigatePath(path, navigateSignal.path)) {
      setExpanded(true);
    }
  }, [navigateSignal, path]);

  useEffect(() => {
    if (isAncestor || isHighlighted) {
      setExpanded(true);
    }
  }, [isAncestor, isHighlighted]);

  const toggle = useCallback(() => setExpanded((e) => !e), []);

  const nodeRefCallback = useCallback(
    (el: HTMLDivElement | null) => {
      if (registerNodeRef && path) {
        registerNodeRef(path, el);
      }
    },
    [registerNodeRef, path],
  );

  const setHighlightEl = useContext(HighlightRefSetterContext);
  const refCallback = useCallback(
    (el: HTMLDivElement | null) => {
      if (isHighlighted) {
        setHighlightEl(el);
      }
      nodeRefCallback(el);
    },
    [isHighlighted, setHighlightEl, nodeRefCallback],
  );

  const tagName = element.tagName;

  // Self-closing or text-only leaf
  if (!hasChildren && !directText) {
    return (
      <div
        ref={refCallback}
        className={cn(
          "py-px pl-6",
          isHighlighted &&
            "rounded bg-yellow-200/40 dark:bg-yellow-500/20",
        )}
      >
        <TagOpen tagName={tagName} attrs={attrs} selfClosing />
      </div>
    );
  }

  if (!hasChildren && directText) {
    return (
      <div
        ref={refCallback}
        className={cn(
          "flex flex-wrap gap-x-0 py-px pl-6",
          isHighlighted &&
            "rounded bg-yellow-200/40 dark:bg-yellow-500/20",
        )}
      >
        <TagOpen tagName={tagName} attrs={attrs} />
        <TextContent text={directText} />
        <TagClose tagName={tagName} />
      </div>
    );
  }

  // Element with child elements
  return (
    <div>
      <div
        ref={refCallback}
        className={cn(
          "hover:bg-muted/50 flex cursor-pointer items-center gap-x-1 py-px pl-1 select-none",
          isHighlighted &&
            "rounded bg-yellow-200/40 dark:bg-yellow-500/20",
        )}
        onClick={toggle}
      >
        {expanded ? (
          <ChevronDown className="text-muted-foreground size-4 shrink-0" />
        ) : (
          <ChevronRight className="text-muted-foreground size-4 shrink-0" />
        )}
        <TagOpen tagName={tagName} attrs={attrs} />
        {!expanded && (
          <>
            <span className="text-muted-foreground text-xs">
              {childElements.length}{" "}
              {childElements.length === 1 ? "child" : "children"}
            </span>
            <TagClose tagName={tagName} />
          </>
        )}
      </div>
      {expanded && (
        <>
          <div className="border-border/50 ml-3 border-l">
            {directText && (
              <div className="py-px pl-6">
                <TextContent text={directText} />
              </div>
            )}
            {childElements.map((child, i) => (
              <XmlElementNode
                key={buildNodePath(child)}
                element={child}
                path={buildNodePath(child)}
                registerNodeRef={registerNodeRef}
                depth={depth + 1}
                isLast={i === childElements.length - 1}
              />
            ))}
          </div>
          <div className="text-foreground/60 py-px pl-4">
            <TagClose tagName={tagName} />
          </div>
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

function TagOpen({
  tagName,
  attrs,
  selfClosing = false,
}: {
  tagName: string;
  attrs: [string, string][];
  selfClosing?: boolean;
}) {
  return (
    <span>
      <span className="text-foreground/60">&lt;</span>
      <span className="text-rose-600 dark:text-rose-400">{tagName}</span>
      {attrs.map(([name, value]) => (
        <span key={name}>
          {" "}
          <span className="text-violet-700 dark:text-violet-400">
            {name}
          </span>
          <span className="text-foreground/60">=</span>
          <span className="text-green-700 dark:text-green-400">
            &quot;{value}&quot;
          </span>
        </span>
      ))}
      <span className="text-foreground/60">
        {selfClosing ? " />" : ">"}
      </span>
    </span>
  );
}

function TagClose({ tagName }: { tagName: string }) {
  return (
    <span>
      <span className="text-foreground/60">&lt;/</span>
      <span className="text-rose-600 dark:text-rose-400">{tagName}</span>
      <span className="text-foreground/60">&gt;</span>
    </span>
  );
}

function TextContent({ text }: { text: string }) {
  const MAX_INLINE_LENGTH = 300;
  const truncated =
    text.length > MAX_INLINE_LENGTH
      ? text.slice(0, MAX_INLINE_LENGTH) + "…"
      : text;
  return <span className="text-foreground break-all">{truncated}</span>;
}

export default XmlViewer;
