import {
  BookOpenText,
  Camera,
  CircleHelp,
  Clock,
  FileInput,
  FilePenLine,
  FileSearch,
  FileText,
  FolderOpen,
  FolderPlus,
  FolderTree,
  Globe,
  LayoutGrid,
  List,
  ListChecks,
  MousePointerClick,
  Navigation,
  Newspaper,
  NotebookPen,
  Bot as Robot,
  ScanText,
  ScrollText,
  Terminal,
  Wrench,
  type LucideIcon,
} from "lucide-react";

import { isRendererToolName, type RendererToolName } from "./api";
import { cn } from "./lib/utils";

/**
 * The icon for a tool, derived only from its allowlisted renderer name.
 *
 * A row's icon says which capability ran; its status is carried separately by
 * [`ToolStatusIcon`]. Anything outside the allowlist gets the generic wrench
 * rather than leaking a provider-supplied name into an icon choice.
 *
 * Keyed by [`RendererToolName`] rather than `string` so adding a tool to that
 * union without giving it an icon fails to compile. Two tools had already
 * reached the allowlist without one and were silently rendering the wrench.
 */
const TOOL_ICONS: Record<RendererToolName, LucideIcon> = {
  search: FileSearch,
  list_documents: List,
  read_document: BookOpenText,
  read_tool_result: ScrollText,
  web_search: Globe,
  web_extract: Newspaper,
  read_delegated_file: FileText,
  read_file: FileText,
  list_dir: FolderOpen,
  write_file: FilePenLine,
  write_output_to_connected_folder: FileInput,
  request_folder_access: FolderPlus,
  connect_folder: FolderPlus,
  list_connected_folders: FolderOpen,
  list_folder: FolderTree,
  read_connected_file: FileText,
  import_connected_file: FileInput,
  ask_user_questions: CircleHelp,
  exit_plan_mode: NotebookPen,
  update_task_plan: ListChecks,
  browser_list: Globe,
  browser_navigate: Navigation,
  browser_snapshot: ScanText,
  browser_wait: Clock,
  browser_screenshot: Camera,
  browser_act: MousePointerClick,
  spawn_sandbox_agent: Robot,
  wait_for_agents: Clock,
  exec: Terminal,
  // The same mark the Apps library carries, so the row and the panel it
  // opens read as one thing.
  create_app: LayoutGrid,
  // The server folds every unrecognized tool name to `other`, so this is the
  // one entry that is genuinely "some tool ran" rather than a missing icon.
  other: Wrench,
};

/**
 * Capability color is categorical, not status. Keep routine reads, lists,
 * plans, and commands neutral; reserve color for the smaller set of actions
 * where the capability boundary itself is useful to notice at a glance.
 */
const TOOL_ICON_TONES: Record<RendererToolName, string> = {
  search: "text-icon-cyan",
  list_documents: "text-muted-foreground",
  read_document: "text-muted-foreground",
  read_tool_result: "text-muted-foreground",
  web_search: "text-icon-cyan",
  web_extract: "text-icon-cyan",
  read_delegated_file: "text-muted-foreground",
  read_file: "text-muted-foreground",
  list_dir: "text-muted-foreground",
  write_file: "text-icon-green",
  write_output_to_connected_folder: "text-icon-green",
  request_folder_access: "text-icon-amber",
  connect_folder: "text-icon-amber",
  list_connected_folders: "text-muted-foreground",
  list_folder: "text-muted-foreground",
  read_connected_file: "text-muted-foreground",
  import_connected_file: "text-icon-green",
  ask_user_questions: "text-icon-rose",
  exit_plan_mode: "text-muted-foreground",
  update_task_plan: "text-muted-foreground",
  browser_list: "text-muted-foreground",
  browser_navigate: "text-muted-foreground",
  browser_snapshot: "text-muted-foreground",
  browser_wait: "text-muted-foreground",
  browser_screenshot: "text-muted-foreground",
  browser_act: "text-muted-foreground",
  spawn_sandbox_agent: "text-icon-violet",
  wait_for_agents: "text-muted-foreground",
  exec: "text-muted-foreground",
  create_app: "text-icon-blue",
  other: "text-muted-foreground",
};

export function ToolIcon({
  name,
  className,
}: {
  name: string;
  className?: string;
}) {
  const safeName = isRendererToolName(name) ? name : "other";
  const Icon = TOOL_ICONS[safeName];
  return <Icon className={cn(TOOL_ICON_TONES[safeName], className)} />;
}
