import { AppCardList } from "tidebreak-desktop-ui";

export function PublishedApps() {
  return (
    <AppCardList
      apps={[
        {
          kind: "app",
          label: "Release dashboard",
          detail: null,
          meta: "Revision 3 · published just now",
          mediaType: null,
          targetId: "app_9f2c",
          url: null,
        },
        {
          kind: "app",
          label: "Flaky test triage board",
          detail: null,
          meta: "Revision 1 · published 2 minutes ago",
          mediaType: null,
          targetId: "app_b41d",
          url: null,
        },
      ]}
    />
  );
}

export function WithoutDestination() {
  return (
    <AppCardList
      apps={[
        {
          kind: "app",
          label: "Sprint retro notes",
          detail: null,
          meta: "Revision 2 · rehydrated from an older journal",
          mediaType: null,
          targetId: null,
          url: null,
        },
      ]}
    />
  );
}
