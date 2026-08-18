import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  ChevronRight,
  Ellipsis,
  Files,
  Folder,
  FolderOpen,
  Pencil,
  Plus,
  SquarePen,
  Trash2,
} from "lucide-react";

import type { Project } from "@/api";
import { useApp } from "@/AppContext";
import { useChatAttention } from "@/ChatAttention";
import { useChatListStore } from "@/ChatListStore";
import { useProjectListStore } from "@/ProjectListStore";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { NewProjectDialog } from "./NewProjectDialog";
import { RecentChatRow } from "./RecentChatRow";

/**
 * The rail's projects, above the chat list.
 *
 * Each project is a folder that opens onto the conversations filed under it;
 * those conversations are not repeated in the chat list below, so every chat
 * has exactly one place in the rail. The heading stays even with no projects,
 * because its `+` is the only way to make the first one — but nothing sits
 * under it, so an empty rail still reads as a plain list of chats.
 */
export function ProjectsSection({ activeChatId }: { activeChatId?: string }) {
  const {
    newProject,
    deleteProject,
    startProjectRename,
    commitProjectRename,
    cancelProjectRename,
    newChatInProject,
    moveChatToProject,
    deleteChat,
    startRename,
    commitRename,
    cancelRename,
  } = useApp();
  const navigate = useNavigate();
  const projects = useProjectListStore((state) => state.projects);
  const creatingProject = useProjectListStore((state) => state.creatingProject);
  const deletingProjectId = useProjectListStore(
    (state) => state.deletingProjectId,
  );
  const renamingProjectId = useProjectListStore(
    (state) => state.renamingProjectId,
  );
  const renameProjectDraft = useProjectListStore(
    (state) => state.renameProjectDraft,
  );
  const savingProjectTitle = useProjectListStore(
    (state) => state.savingProjectTitle,
  );
  const setProjectRenameDraft = useProjectListStore(
    (state) => state.setProjectRenameDraft,
  );
  const expandedProjectIds = useProjectListStore(
    (state) => state.expandedProjectIds,
  );
  const toggleProjectExpanded = useProjectListStore(
    (state) => state.toggleProjectExpanded,
  );
  const chats = useChatListStore((state) => state.chats);
  const creatingChat = useChatListStore((state) => state.creatingChat);
  const deletingChatId = useChatListStore((state) => state.deletingChatId);
  const renamingChatId = useChatListStore((state) => state.renamingChatId);
  const renameChatDraft = useChatListStore((state) => state.renameChatDraft);
  const savingTitle = useChatListStore((state) => state.savingTitle);
  const setRenameDraft = useChatListStore((state) => state.setRenameDraft);
  const chatIdsWithPendingPrompts = useChatAttention(
    (state) => state.chatIdsWithPendingPrompts,
  );
  const [creatingOpen, setCreatingOpen] = useState(false);

  return (
    <div className="mt-4 flex shrink-0 flex-col">
      <div className="flex shrink-0 items-center gap-0.5 pr-1">
        <span className="min-w-0 flex-1 px-2 py-1 text-sm font-medium text-muted-foreground">
          Projects
        </span>
        <button
          type="button"
          className="shrink-0 cursor-pointer rounded p-1 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
          aria-label="New project"
          disabled={creatingProject || deletingProjectId !== null}
          onClick={() => setCreatingOpen(true)}
        >
          <Plus size={15} />
        </button>
      </div>

      <NewProjectDialog
        open={creatingOpen}
        onOpenChange={setCreatingOpen}
        onCreate={newProject}
        creating={creatingProject}
      />

      <div className="flex flex-col gap-0.5" aria-label="Projects">
        {projects.map((project) => {
          const held = chats.filter((chat) => chat.project_id === project.id);
          const expanded = expandedProjectIds.includes(project.id);
          return (
            <div key={project.id} className="flex flex-col gap-0.5">
              <ProjectRow
                project={project}
                expanded={expanded}
                renaming={renamingProjectId === project.id}
                renameDraft={renameProjectDraft}
                savingTitle={savingProjectTitle}
                mutating={deletingProjectId !== null}
                onRenameDraftChange={setProjectRenameDraft}
                onToggle={() => toggleProjectExpanded(project.id)}
                onNewChat={() => newChatInProject(project.id)}
                onOpenFiles={() =>
                  void navigate({
                    to: "/p/$projectId",
                    params: { projectId: project.id },
                  })
                }
                onStartRename={() => startProjectRename(project)}
                onCommitRename={() => commitProjectRename(project)}
                onCancelRename={cancelProjectRename}
                onDelete={() => deleteProject(project)}
              />
              {expanded && (
                <div className="ml-4 flex flex-col gap-0.5 border-l pl-1">
                  {held.map((chat) => (
                    <RecentChatRow
                      key={chat.id}
                      chat={chat}
                      active={chat.id === activeChatId}
                      needsAttention={chatIdsWithPendingPrompts.has(chat.id)}
                      renaming={renamingChatId === chat.id}
                      renameDraft={renameChatDraft}
                      savingTitle={savingTitle}
                      mutating={deletingChatId !== null || creatingChat}
                      projects={projects}
                      onRenameDraftChange={setRenameDraft}
                      onOpen={() =>
                        void navigate({
                          to: "/p/$projectId/c/$chatId",
                          params: { projectId: project.id, chatId: chat.id },
                        })
                      }
                      onStartRename={() => startRename(chat)}
                      onCommitRename={() => commitRename(chat)}
                      onCancelRename={cancelRename}
                      onMoveToProject={(projectId) =>
                        moveChatToProject(chat, projectId)
                      }
                      onDelete={() => deleteChat(chat)}
                    />
                  ))}
                  {held.length === 0 && (
                    <button
                      type="button"
                      className="cursor-pointer rounded-md px-2 py-1 text-left text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:pointer-events-none disabled:opacity-50"
                      disabled={creatingChat || deletingChatId !== null}
                      onClick={() => newChatInProject(project.id)}
                    >
                      New chat
                    </button>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** One project folder: the disclosure, its name, and its row actions. */
function ProjectRow({
  project,
  expanded,
  renaming,
  renameDraft,
  savingTitle,
  mutating,
  onRenameDraftChange,
  onToggle,
  onNewChat,
  onOpenFiles,
  onStartRename,
  onCommitRename,
  onCancelRename,
  onDelete,
}: {
  project: Project;
  expanded: boolean;
  renaming: boolean;
  renameDraft: string;
  savingTitle: boolean;
  mutating: boolean;
  onRenameDraftChange: (draft: string) => void;
  onToggle: () => void;
  onNewChat: () => void;
  onOpenFiles: () => void;
  onStartRename: () => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onDelete: () => void;
}) {
  const title = project.title?.trim() || "Untitled project";

  if (renaming) {
    return (
      <Input
        className="h-auto px-2 py-1.5 text-sm"
        autoFocus
        aria-label="Project title"
        value={renameDraft}
        disabled={savingTitle}
        onChange={(event) => onRenameDraftChange(event.target.value)}
        onBlur={onCommitRename}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            event.currentTarget.blur();
          }
          if (event.key === "Escape") {
            event.preventDefault();
            onCancelRename();
          }
        }}
      />
    );
  }

  return (
    <div className="group flex items-center rounded-md transition-colors hover:bg-muted">
      <button
        type="button"
        className="flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 px-2 py-1.5 text-left text-sm disabled:pointer-events-none disabled:opacity-50"
        aria-expanded={expanded}
        disabled={mutating}
        onClick={onToggle}
      >
        {expanded ? (
          <FolderOpen aria-hidden="true" className="size-4 shrink-0" />
        ) : (
          <Folder aria-hidden="true" className="size-4 shrink-0" />
        )}
        <span className="min-w-0 flex-1 truncate">{title}</span>
        <ChevronRight
          aria-hidden="true"
          className={cn(
            "size-3.5 shrink-0 text-muted-foreground transition-transform",
            expanded && "rotate-90",
          )}
        />
      </button>
      <button
        type="button"
        className="cursor-pointer rounded p-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 disabled:pointer-events-none"
        aria-label={`New chat in ${title}`}
        disabled={mutating}
        onClick={onNewChat}
      >
        <SquarePen size={15} />
      </button>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            // Kept in the layout while hidden so the row does not reflow under
            // the cursor, matching the conversation rows below.
            className="mr-1 cursor-pointer rounded p-1 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 disabled:pointer-events-none"
            aria-label={`Actions for ${title}`}
            disabled={mutating}
          >
            <Ellipsis size={15} />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" side="right">
          <DropdownMenuItem onSelect={onNewChat}>
            <SquarePen />
            New chat
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onOpenFiles}>
            <Files />
            Project files
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onStartRename}>
            <Pencil />
            Rename
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem variant="destructive" onSelect={onDelete}>
            <Trash2 />
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
