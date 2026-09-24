// Known accessibility problems that `pnpm storybook:a11y` tolerates for now.
//
// Each entry names a story, a rule, and the reason the problem is still here.
// `story` is a Storybook story id, the `id=` value in a story URL; end it with
// `*` to match every story whose id starts with the rest, such as every story
// in one file. `rule` is an axe rule id, or `story-error` for a story that ends
// before axe can run: it throws, its play function fails, or it times out.
//
// Fix the story or component, then delete its entry. The check reports an
// entry that matched nothing in a run, so you can see when one is done. CI
// runs on Linux, where a few stories render differently than on a Mac, so an
// entry can match in CI and not locally.
export const allowlist = [
  // Stories that are broken on main: each fails the same way in the
  // Storybook UI, so axe never runs on them.
  {
    story: "code-computer-use-activity--status-keeps-viewport-size",
    rule: "story-error",
    reason: "The play function expects the status to contain 'Action completed'.",
  },
  {
    story: "code-delivery-center--archive-remote-workspaces",
    rule: "story-error",
    reason: "The play function expects a button to disappear, and it stays.",
  },
  {
    story: "code-delivery-center--narrow-pull-request-detail",
    rule: "story-error",
    reason: "The play function looks for 'Build the delivery center', which the detail no longer shows.",
  },
  {
    story: "code-delivery-center--pull-request-detail-checks",
    rule: "story-error",
    reason: "The play function looks for 'Build the delivery center', which the detail no longer shows.",
  },
  {
    story: "code-delivery-center--pull-request-detail-conversation",
    rule: "story-error",
    reason: "The play function looks for 'Build the delivery center', which the detail no longer shows.",
  },
  {
    story: "code-delivery-center--pull-request-detail-files",
    rule: "story-error",
    reason: "The play function looks for 'Build the delivery center', which the detail no longer shows.",
  },
  {
    story: "code-delivery-center--pull-request-stack-detail",
    rule: "story-error",
    reason: "The play function looks for 'Stack base: extract the fact store', which is not rendered.",
  },
  {
    story: "code-delivery-center--pull-request-stack-detail-without-auto-merge",
    rule: "story-error",
    reason: "The play function looks for 'Stack base: extract the fact store', which is not rendered.",
  },
  {
    story: "code-repository-setup--compact-new-workspace",
    rule: "story-error",
    reason: "The component reads router state, and the story renders it without a router.",
  },
  {
    story: "code-repository-setup--new-workspace",
    rule: "story-error",
    reason: "The component reads router state, and the story renders it without a router.",
  },
  {
    story: "code-repository-setup--new-workspace-needs-harness",
    rule: "story-error",
    reason: "The component reads router state, and the story renders it without a router.",
  },
  {
    story: "code-session-tree--paused-child",
    rule: "story-error",
    reason: "The play function looks for 'Needs attention', which is not rendered.",
  },
  {
    story: "code-session-tree--waiting",
    rule: "story-error",
    reason: "The play function looks for 'Waiting on 3 of 5', which is not rendered.",
  },
  {
    story: "code-start-session--all-engines",
    rule: "story-error",
    reason: "The play function looks for a heading named 'acme/api', which is not rendered.",
  },
  {
    story: "code-workspace-workflow--sandbox-pull-request",
    rule: "story-error",
    reason: "The play function expects an element to be visible, and it is not.",
  },
  {
    story: "code-workspace-workflow--sandbox-refresh-failed",
    rule: "story-error",
    reason: "The play function expects an element to be visible, and it is not.",
  },
  {
    story: "code-workspace-workflow--sandbox-without-pull-request",
    rule: "story-error",
    reason: "The play function expects an element to be visible, and it is not.",
  },
  {
    story: "conversation-transcript--notices",
    rule: "story-error",
    reason: "The play function expects more than two notices and finds none.",
  },

  // A play function that fails only some of the time, so this entry also
  // shows up as unmatched in runs where the story passes.
  {
    story: "code-new-workspace--pasted-image",
    rule: "story-error",
    reason:
      "On a busy machine the play function sometimes finds the pasted image's name " +
      "before it is visible, in both passes. It passes in the Storybook UI.",
  },

  // Names and labels. axe prohibits aria-label on a plain div or span, so give
  // the element a role or drop the label. Every dialog needs a name.
  {
    story: "apps-library--*",
    rule: "aria-prohibited-attr",
    reason: "The app detail's Access section is a div with aria-label and no role.",
  },
  {
    story: "code-browser--*",
    rule: "aria-prohibited-attr",
    reason: "The sample web page these stories draw labels a plain div; it is story fixture content.",
  },
  {
    story: "code-diff-detail--*",
    rule: "aria-prohibited-attr",
    reason: "The diff scroller carries aria-label without a role.",
  },
  {
    story: "code-file-viewer--*",
    rule: "aria-prohibited-attr",
    reason: "The comparison pane is a div with aria-label='Comparison' and no role.",
  },
  {
    story: "code-workspace-header--*",
    rule: "aria-prohibited-attr",
    reason: "A header column carries aria-label without a role.",
  },
  {
    story: "foundations-primitives--*",
    rule: "aria-prohibited-attr",
    reason: "The story labels a Card 'Loading workspace list' without a role.",
  },
  {
    story: "navigation-sidebar--*",
    rule: "aria-prohibited-attr",
    reason: "The sidebar's Projects list is a div with aria-label and no role.",
  },
  {
    story: "sources-facet-filter--*",
    rule: "aria-dialog-name",
    reason: "The facet filter's popover is a dialog with no accessible name.",
  },

  // Scroll containers that a keyboard cannot reach. Each needs a focus stop
  // (tabIndex 0 with a label) or focusable content.
  {
    story: "code-analytics--*",
    rule: "scrollable-region-focusable",
    reason: "A capped-height analytics list scrolls without a focus stop.",
  },
  {
    story: "code-bulk-workspace-archive--*",
    rule: "scrollable-region-focusable",
    reason: "The workspace list in the archive dialog scrolls without a focus stop.",
  },
  {
    story: "code-diff-detail--*",
    rule: "scrollable-region-focusable",
    reason: "The horizontal diff scroller has no focus stop.",
  },
  {
    story: "code-file-viewer--*",
    rule: "scrollable-region-focusable",
    reason: "The file viewer's scroll container has no focus stop.",
  },
  {
    story: "code-repository-trust-sheet--*",
    rule: "scrollable-region-focusable",
    reason: "The capped-height list in the trust sheet scrolls without a focus stop.",
  },
  {
    story: "code-workspace-page--*",
    rule: "scrollable-region-focusable",
    reason: "A muted scroll panel on the workspace page has no focus stop.",
  },
  {
    story: "documents-viewer-chrome--*",
    rule: "scrollable-region-focusable",
    reason: "The document viewer body scrolls without a focus stop.",
  },
  {
    story: "outputs-content--*",
    rule: "scrollable-region-focusable",
    reason:
      "A long markdown code block (.message-markdown > pre) scrolls without a focus stop. " +
      "It shows only when the block has overflowed by the time axe runs.",
  },
  {
    story: "settings-mcp-directory--*",
    rule: "scrollable-region-focusable",
    reason: "The capped-height server directory list scrolls without a focus stop.",
  },

  // Text below the WCAG AA contrast ratio.
  {
    story: "code-browser--*",
    rule: "color-contrast",
    reason: "The sample web page these stories draw uses text-black/45 captions; it is story fixture content.",
  },
  {
    story: "code-file-viewer--*",
    rule: "color-contrast",
    reason: "Monaco's syntax token colors (.mtk7) fall below AA on the editor background.",
  },
  {
    story: "code-new-workspace--*",
    rule: "color-contrast",
    reason:
      "The Create button's shortcut hint (text-2xs at opacity-60) falls below AA. axe skips " +
      "symbols, so it measures the hint only off macOS, where it reads Ctrl instead of ⌘.",
  },
  {
    story: "code-workspace-card--*",
    rule: "color-contrast",
    reason: "The repository name in the detailed card's meta row falls below AA.",
  },
  {
    story: "conversation-user-questions--*",
    rule: "color-contrast",
    reason: "In the Sending Failed story, the question options fall below AA.",
  },
  {
    story: "documents-viewer-content--*",
    rule: "color-contrast",
    reason: "Syntax colors on the yellow search-match highlight fall below AA.",
  },

  // Structure: roles and lists that are missing required parents or children.
  {
    story: "code-center-tabs--*",
    rule: "aria-valid-attr-value",
    reason: "Tabs point aria-controls at panels the workspace page renders; these stories render the tabs alone.",
  },
  {
    story: "code-composer-slash--*",
    rule: "aria-required-children",
    reason: "The slash-command listbox wraps its options in li elements.",
  },
  {
    story: "code-composer-slash--*",
    rule: "aria-required-parent",
    reason: "The slash-command listbox wraps its options in li elements.",
  },
  {
    story: "code-composer-slash--*",
    rule: "listitem",
    reason: "The slash-command listbox wraps its options in li elements.",
  },
  {
    story: "conversation-chatview--*",
    rule: "list",
    reason: "Markdown ordered lists in these messages wrap each li in a div row.",
  },
  {
    story: "conversation-chatview--*",
    rule: "listitem",
    reason: "Markdown ordered lists in these messages wrap each li in a div row.",
  },
  {
    story: "foundations-command-palette--*",
    rule: "aria-required-children",
    reason: "The loading and no-match states render a listbox with no options.",
  },
  {
    story: "code-file-viewer--*",
    rule: "aria-input-field-name",
    reason: "Monaco's diff editor renders its input with an empty aria-label.",
  },

  // Controls nested inside other controls.
  {
    story: "navigation-routes--*",
    rule: "nested-interactive",
    reason: "The provider card header is a role=button disclosure that contains other controls.",
  },
  {
    story: "settings-advanced-panels--*",
    rule: "nested-interactive",
    reason: "The provider card header is a role=button disclosure that contains other controls.",
  },
  {
    story: "settings-provider-models--*",
    rule: "nested-interactive",
    reason: "The provider card header is a role=button disclosure that contains other controls.",
  },

  // An open Radix menu or dialog hides the rest of the page with aria-hidden
  // while that page still holds focusable controls.
  {
    story: "code-workspace-card--*",
    rule: "aria-hidden-focus",
    reason: "An open menu sets aria-hidden on #storybook-root, which still holds focusable controls.",
  },
  {
    story: "conversation-transcript--regenerate-that-starts-new-chat",
    rule: "aria-hidden-focus",
    reason: "An open menu sets aria-hidden on #storybook-root, which still holds focusable controls.",
  },
  {
    story: "conversation-transcript--retry-with-model",
    rule: "aria-hidden-focus",
    reason: "An open menu sets aria-hidden on #storybook-root, which still holds focusable controls.",
  },
  {
    story: "foundations-primitives--*",
    rule: "aria-hidden-focus",
    reason: "An open menu sets aria-hidden on #storybook-root, which still holds focusable controls.",
  },
  {
    story: "navigation-sidebar--*",
    rule: "aria-hidden-focus",
    reason: "An open menu sets aria-hidden on #storybook-root, which still holds focusable controls.",
  },
  {
    story: "composer-work-menus--*",
    rule: "aria-hidden-focus",
    reason: "An open menu sets aria-hidden on #storybook-root, which still holds focusable controls.",
  },
  {
    story: "composer-work-menus--last-used-and-default",
    rule: "aria-required-children",
    reason: "The model menu keeps its provider rail (a tablist) and its search field inside the role=menu content.",
  },
];
