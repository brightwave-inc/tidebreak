import { useEffect, useRef, useState, type FormEvent, type Ref } from "react";
import {
  Check,
  FolderOpen,
  MessagesSquare,
  Pencil,
  Plus,
  X,
} from "lucide-react";
import type { ReactNode } from "react";
import type { Project } from "./api";

type Props = {
  projects: Project[];
  selectedProjectId: string | null;
  disabled: boolean;
  error: string | null;
  onSelect: (projectId: string | null) => void;
  onCreate: (title: string) => Promise<boolean>;
  onRename: (projectId: string, title: string | null) => Promise<boolean>;
  onDelete: (project: Project) => Promise<boolean>;
};

export function ProjectNavigation({
  projects,
  selectedProjectId,
  disabled,
  error,
  onSelect,
  onCreate,
  onRename,
  onDelete,
}: Props) {
  const [adding, setAdding] = useState(false);
  const [title, setTitle] = useState("");
  const [editingProjectId, setEditingProjectId] = useState<string | null>(null);
  const [renameTitle, setRenameTitle] = useState("");
  const [savingRename, setSavingRename] = useState(false);
  const renameInFlightRef = useRef(false);
  const renameActionRefs = useRef(new Map<string, HTMLButtonElement>());
  const looseChatsRef = useRef<HTMLButtonElement>(null);
  const renaming = editingProjectId !== null;

  useEffect(() => {
    if (editingProjectId !== null && editingProjectId !== selectedProjectId) {
      setEditingProjectId(null);
      setRenameTitle("");
    }
  }, [editingProjectId, selectedProjectId]);

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmed = title.trim();
    if (!trimmed || disabled) return;
    if (await onCreate(trimmed)) {
      setTitle("");
      setAdding(false);
    }
  }

  async function submitRename(
    event: FormEvent<HTMLFormElement>,
    projectId: string,
  ) {
    event.preventDefault();
    const trimmed = renameTitle.trim();
    if (disabled || renameInFlightRef.current) return;
    renameInFlightRef.current = true;
    setSavingRename(true);
    try {
      if (await onRename(projectId, trimmed || null)) finishRename(projectId);
    } finally {
      renameInFlightRef.current = false;
      setSavingRename(false);
    }
  }

  function finishRename(projectId: string) {
    setEditingProjectId(null);
    setRenameTitle("");
    requestAnimationFrame(() => renameActionRefs.current.get(projectId)?.focus());
  }

  async function deleteProject(project: Project) {
    if (await onDelete(project)) {
      requestAnimationFrame(() => looseChatsRef.current?.focus());
    }
  }

  return (
    <section className="project-navigation" aria-label="Projects">
      <div className="sidebar-section-heading">
        <span className="sidebar-label">Projects</span>
        <button
          type="button"
          className="sidebar-add"
          aria-label="Create project"
          title="Create project"
          disabled={disabled || renaming}
          onClick={() => setAdding((current) => !current)}
        >
          <Plus size={14} />
        </button>
      </div>

      {adding && (
        <form className="project-create" onSubmit={(event) => void submit(event)}>
          <input
            autoFocus
            aria-label="Project name"
            placeholder="Project name"
            value={title}
            maxLength={120}
            disabled={disabled}
            onChange={(event) => setTitle(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                setTitle("");
                setAdding(false);
              }
            }}
          />
          <div className="project-create-actions">
            <button type="submit" disabled={disabled || !title.trim()}>
              Create
            </button>
            <button
              type="button"
              disabled={disabled}
              onClick={() => {
                setTitle("");
                setAdding(false);
              }}
            >
              Cancel
            </button>
          </div>
        </form>
      )}

      <div className="project-list">
        <ProjectButton
          buttonRef={looseChatsRef}
          title="Direct chats"
          icon={<MessagesSquare size={13} />}
          selected={selectedProjectId === null}
          disabled={disabled || renaming}
          onClick={() => onSelect(null)}
        />
        {projects.map((project) => {
          const projectTitle = project.title?.trim() || "Untitled project";
          const selected = selectedProjectId === project.id;
          if (editingProjectId === project.id && selected) {
            return (
              <form
                key={project.id}
                className="project-rename"
                onSubmit={(event) => void submitRename(event, project.id)}
              >
                <input
                  autoFocus
                  aria-label="Project name"
                  placeholder="Untitled project"
                  value={renameTitle}
                  maxLength={120}
                  disabled={disabled || savingRename}
                  onChange={(event) => setRenameTitle(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") {
                      finishRename(project.id);
                    }
                  }}
                />
                <button
                  type="submit"
                  aria-label="Save project name"
                  disabled={disabled || savingRename}
                >
                  <Check size={13} />
                </button>
                <button
                  type="button"
                  aria-label="Cancel project rename"
                  disabled={disabled || savingRename}
                  onClick={() => finishRename(project.id)}
                >
                  <X size={13} />
                </button>
              </form>
            );
          }
          return (
            <div key={project.id} className={`project-row${selected ? " is-active" : ""}`}>
              <ProjectButton
                title={projectTitle}
                selected={selected}
                disabled={disabled || savingRename || renaming}
                onClick={() => onSelect(project.id)}
              />
              {selected && (
                <button
                  type="button"
                  className="project-rename-action"
                  ref={(element) => {
                    if (element) renameActionRefs.current.set(project.id, element);
                    else renameActionRefs.current.delete(project.id);
                  }}
                  aria-label={`Rename ${projectTitle}`}
                  title="Rename project"
                  disabled={disabled || savingRename || adding}
                  onClick={() => {
                    setEditingProjectId(project.id);
                    setRenameTitle(project.title ?? "");
                  }}
                >
                  <Pencil size={12} />
                </button>
              )}
              <button
                type="button"
                className="project-delete-action"
                aria-label={`Delete ${projectTitle}`}
                title="Delete project"
                disabled={disabled || savingRename || renaming || adding}
                onClick={() => void deleteProject(project)}
              >
                <X size={13} />
              </button>
            </div>
          );
        })}
      </div>
      {error && <p className="sidebar-error">{error}</p>}
    </section>
  );
}

function ProjectButton({
  buttonRef,
  title,
  icon,
  selected,
  disabled,
  onClick,
}: {
  buttonRef?: Ref<HTMLButtonElement>;
  title: string;
  icon?: ReactNode;
  selected: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      ref={buttonRef}
      type="button"
      className={`project-item${selected ? " is-active" : ""}`}
      aria-current={selected ? "page" : undefined}
      disabled={disabled}
      onClick={onClick}
    >
      <span className="project-icon" aria-hidden="true">
        {icon ?? <FolderOpen size={12} />}
      </span>
      <span>{title}</span>
    </button>
  );
}
