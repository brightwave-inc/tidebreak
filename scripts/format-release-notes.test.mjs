import assert from "node:assert/strict";
import test from "node:test";

import { formatReleaseNotes } from "./format-release-notes.mjs";

test("groups repeated scopes and keeps singleton scopes compact at the end", () => {
  const notes = `## What's Changed

### New Features
- feat(desktop): add document search ([#12](https://example.com/12)) by @octo
- feat: add a default workspace ([#13](https://example.com/13)) by @octo
- feat(core): add saved searches ([#14](https://example.com/14)) by @octo
- feat(desktop): add keyboard shortcuts ([#15](https://example.com/15)) by @octo

### Bug Fixes
- fix(desktop): prevent a startup crash ([#16](https://example.com/16)) by @octo
### Other Changes
- docs: update the setup guide ([#17](https://example.com/17)) by @octo
`;

  assert.equal(
    formatReleaseNotes(notes),
    `## What's Changed

### New Features
#### Desktop
- add document search ([#12](https://example.com/12)) by @octo
- add keyboard shortcuts ([#15](https://example.com/15)) by @octo

- add a default workspace ([#13](https://example.com/13)) by @octo
- **Core:** add saved searches ([#14](https://example.com/14)) by @octo

### Bug Fixes
- **Desktop:** prevent a startup crash ([#16](https://example.com/16)) by @octo
### Other Changes
- docs: update the setup guide ([#17](https://example.com/17)) by @octo
`,
  );
});

test("keeps unscoped release entries in their category without a subheading", () => {
  const notes = `### Dependency Updates
- deps: update sqlite ([#17](https://example.com/17)) by @octo
`;

  assert.equal(
    formatReleaseNotes(notes),
    `### Dependency Updates
- update sqlite ([#17](https://example.com/17)) by @octo
`,
  );
});

test("uses readable acronyms in singleton scope prefixes", () => {
  const notes = `### Breaking Changes
- feat(mcp)!: replace the protocol ([#18](https://example.com/18)) by @octo
### Other Changes
- Plain historical change ([#19](https://example.com/19)) by @octo
`;

  assert.equal(
    formatReleaseNotes(notes),
    `### Breaking Changes
- **MCP:** replace the protocol ([#18](https://example.com/18)) by @octo
### Other Changes
- Plain historical change ([#19](https://example.com/19)) by @octo
`,
  );
});
