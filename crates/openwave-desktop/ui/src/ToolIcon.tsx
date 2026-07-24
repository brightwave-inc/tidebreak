import {
  BookOpenText,
  CircleHelp,
  Clock,
  FilePenLine,
  FilePlus2,
  FileSearch,
  FileText,
  FolderOpen,
  FolderPlus,
  Globe,
  List,
  Bot as Robot,
  Terminal,
  Wrench,
  type LucideIcon,
} from "lucide-react";

/**
 * The icon for a tool, derived only from its allowlisted renderer name.
 *
 * A row's icon says which capability ran; its status is carried separately by
 * [`ToolStatusIcon`]. Anything outside the allowlist gets the generic wrench
 * rather than leaking a provider-supplied name into an icon choice.
 */
const TOOL_ICONS: Record<string, LucideIcon> = {
  search: FileSearch,
  list_sources: List,
  read_source: BookOpenText,
  web_search: Globe,
  read_delegated_file: FileText,
  read_file: FileText,
  list_dir: FolderOpen,
  write_file: FilePenLine,
  create_deliverable: FilePlus2,
  request_folder_access: FolderPlus,
  connect_folder: FolderPlus,
  list_connected_folders: FolderOpen,
  read_connected_file: FileText,
  ask_user_questions: CircleHelp,
  spawn_sandbox_agent: Robot,
  wait_for_agents: Clock,
  exec: Terminal,
};

const FALLBACK_ICON = Wrench;

export function ToolIcon({
  name,
  className,
}: {
  name: string;
  className?: string;
}) {
  const Icon = TOOL_ICONS[name] ?? FALLBACK_ICON;
  return <Icon className={className} />;
}
