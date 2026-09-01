import type {
  CodeDeliveryPrViewFilters,
  CodeDeliveryRunViewFilters,
} from "../CodeDeliveryStore";

export type PullRequestGrouping = "attention" | "repository" | "none";

export type PrBuiltInView = {
  id: string;
  label: string;
  /**
   * Fill `authors` with the signed-in GitHub login. The login only arrives
   * with the repository snapshot, so the view carries the intent and the
   * page resolves it once `gh` reports who you are.
   */
  viewerAuthored?: boolean;
  filters: CodeDeliveryPrViewFilters;
};

/**
 * The first entry is the default view. Delivery opens on your own open work —
 * drafts included, because `state` is still `open` on a draft — rather than on
 * everyone's review queue.
 */
export const PR_BUILT_IN_VIEWS: readonly PrBuiltInView[] = [
  {
    id: "mine",
    label: "Yours",
    viewerAuthored: true,
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: false,
    },
  },
  {
    id: "attention",
    label: "Needs attention",
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: true,
      readyOnly: false,
    },
  },
  {
    id: "ready",
    label: "Ready to merge",
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: true,
    },
  },
  {
    id: "open",
    label: "Open",
    filters: {
      search: "",
      repositoryKeys: [],
      states: ["open"],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: false,
    },
  },
  {
    id: "all",
    label: "All",
    filters: {
      search: "",
      repositoryKeys: [],
      states: [],
      reviewStates: [],
      checkStates: [],
      authors: [],
      attentionOnly: false,
      readyOnly: false,
    },
  },
];

export const RUN_BUILT_IN_VIEWS: readonly {
  id: string;
  label: string;
  filters: CodeDeliveryRunViewFilters;
}[] = [
  {
    id: "failures",
    label: "Needs attention",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: [],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: true,
    },
  },
  {
    id: "deployments",
    label: "Deployments",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: ["deployment"],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: false,
    },
  },
  {
    id: "actions",
    label: "Actions",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: ["workflow_run"],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: false,
    },
  },
  {
    id: "all",
    label: "All recent",
    filters: {
      search: "",
      repositoryKeys: [],
      kinds: [],
      statuses: [],
      conclusions: [],
      workflows: [],
      environments: [],
      branches: [],
      events: [],
      actors: [],
      attentionOnly: false,
    },
  },
];
