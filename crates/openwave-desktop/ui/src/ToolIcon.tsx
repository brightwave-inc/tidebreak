import {
  BookOpenText,
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
  Newspaper,
  NotebookPen,
  Bot as Robot,
  ScrollText,
  Terminal,
  Wrench,
  type LucideIcon,
} from "lucide-react";

import { isRendererToolName, type RendererToolName } from "./api";

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
  list_sources: List,
  read_source: BookOpenText,
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

export function ToolIcon({
  name,
  className,
}: {
  name: string;
  className?: string;
}) {
  const Icon = isRendererToolName(name) ? TOOL_ICONS[name] : TOOL_ICONS.other;
  return <Icon className={className} />;
}
