import { useState, type FormEvent } from "react";
import type { Project } from "./api";

type Props = {
  projects: Project[];
  selectedProjectId: string | null;
  disabled: boolean;
  error: string | null;
  onSelect: (projectId: string | null) => void;
  onCreate: (title: string) => Promise<boolean>;
};

export function ProjectNavigation({
  projects,
  selectedProjectId,
  disabled,
  error,
  onSelect,
  onCreate,
}: Props) {
  const [adding, setAdding] = useState(false);
  const [title, setTitle] = useState("");

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const trimmed = title.trim();
    if (!trimmed || disabled) return;
    if (await onCreate(trimmed)) {
      setTitle("");
      setAdding(false);
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
          disabled={disabled}
          onClick={() => setAdding((current) => !current)}
        >
          +
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
          title="Loose chats"
          selected={selectedProjectId === null}
          disabled={disabled}
          onClick={() => onSelect(null)}
        />
        {projects.map((project) => (
          <ProjectButton
            key={project.id}
            title={project.title?.trim() || "Untitled project"}
            selected={selectedProjectId === project.id}
            disabled={disabled}
            onClick={() => onSelect(project.id)}
          />
        ))}
      </div>
      {error && <p className="sidebar-error">{error}</p>}
    </section>
  );
}

function ProjectButton({
  title,
  selected,
  disabled,
  onClick,
}: {
  title: string;
  selected: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={`project-item${selected ? " is-active" : ""}`}
      aria-current={selected ? "page" : undefined}
      disabled={disabled}
      onClick={onClick}
    >
      <span className="project-icon" aria-hidden="true">
        {selected ? "◆" : "◇"}
      </span>
      <span>{title}</span>
    </button>
  );
}
