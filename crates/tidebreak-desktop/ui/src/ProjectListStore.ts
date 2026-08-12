import { create } from "zustand";
import type { Project } from "./api";

/**
 * The project list and its mutation progress, shaped like
 * {@link import("./ChatListStore").ChatListStore}: state only, with the async
 * mutations left to the caller so the rail can subscribe directly.
 *
 * Which projects are expanded lives here too. It is view state, not server
 * state, but it belongs next to the list it indexes — the rail is the only
 * reader, and unlike the open conversation it has no URL to live in.
 */
export type ProjectListStore = {
  projects: Project[];
  /**
   * Whether the list has been fetched. Empty means "none yet" only after the
   * first load; before it, the rail must not claim the reader has no projects.
   */
  projectsLoaded: boolean;
  creatingProject: boolean;
  deletingProjectId: string | null;
  renamingProjectId: string | null;
  renameProjectDraft: string;
  savingProjectTitle: boolean;
  /** Ids of the projects whose chats are showing. */
  expandedProjectIds: string[];
  setProjects: (projects: Project[]) => void;
  /** Settle the load without rows, so an empty rail reads as empty. */
  failProjectsLoad: () => void;
  replaceProject: (project: Project) => void;
  prependProject: (project: Project) => void;
  removeProject: (projectId: string) => void;
  setCreatingProject: (creating: boolean) => void;
  setDeletingProjectId: (projectId: string | null) => void;
  beginProjectRename: (project: Project) => void;
  setProjectRenameDraft: (draft: string) => void;
  setSavingProjectTitle: (saving: boolean) => void;
  endProjectRename: () => void;
  toggleProjectExpanded: (projectId: string) => void;
  /** Open a project's chats without closing any other. */
  expandProject: (projectId: string) => void;
};

export function createProjectListStore() {
  return create<ProjectListStore>()((set) => ({
    projects: [],
    projectsLoaded: false,
    creatingProject: false,
    deletingProjectId: null,
    renamingProjectId: null,
    renameProjectDraft: "",
    savingProjectTitle: false,
    expandedProjectIds: [],
    setProjects: (projects) => set({ projects, projectsLoaded: true }),
    failProjectsLoad: () => set({ projectsLoaded: true }),
    replaceProject: (project) =>
      set((state) => ({
        projects: state.projects.map((item) =>
          item.id === project.id ? project : item,
        ),
      })),
    prependProject: (project) =>
      set((state) => ({
        projects: [project, ...state.projects],
        // A project is made in order to put something in it, so it opens.
        expandedProjectIds: [...state.expandedProjectIds, project.id],
      })),
    removeProject: (projectId) =>
      set((state) => ({
        projects: state.projects.filter((item) => item.id !== projectId),
        expandedProjectIds: state.expandedProjectIds.filter(
          (id) => id !== projectId,
        ),
      })),
    setCreatingProject: (creatingProject) => set({ creatingProject }),
    setDeletingProjectId: (deletingProjectId) => set({ deletingProjectId }),
    beginProjectRename: (project) =>
      set({
        renamingProjectId: project.id,
        renameProjectDraft: project.title ?? "",
      }),
    setProjectRenameDraft: (renameProjectDraft) => set({ renameProjectDraft }),
    setSavingProjectTitle: (savingProjectTitle) => set({ savingProjectTitle }),
    endProjectRename: () =>
      set({
        renamingProjectId: null,
        renameProjectDraft: "",
        savingProjectTitle: false,
      }),
    toggleProjectExpanded: (projectId) =>
      set((state) => ({
        expandedProjectIds: state.expandedProjectIds.includes(projectId)
          ? state.expandedProjectIds.filter((id) => id !== projectId)
          : [...state.expandedProjectIds, projectId],
      })),
    expandProject: (projectId) =>
      set((state) =>
        state.expandedProjectIds.includes(projectId)
          ? {}
          : { expandedProjectIds: [...state.expandedProjectIds, projectId] },
      ),
  }));
}

export const useProjectListStore = createProjectListStore();
