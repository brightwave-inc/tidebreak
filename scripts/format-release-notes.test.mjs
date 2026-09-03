import assert from "node:assert/strict";
import test from "node:test";

import { formatReleaseNotes } from "./format-release-notes.mjs";

const THANK_YOU = `> ❤️ **Thanks for using Tidebreak.**
>
> If you filed an issue, reviewed a pull request, or shipped a change, you are in these notes.`;

test("separates grouped and singleton scopes into tight Markdown lists", () => {
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
    `${THANK_YOU}

## ✨ New Features
### Desktop
- add document search ([#12](https://example.com/12)) by @octo
- add keyboard shortcuts ([#15](https://example.com/15)) by @octo

### Other
- add a default workspace ([#13](https://example.com/13)) by @octo
- **Core:** add saved searches ([#14](https://example.com/14)) by @octo

## 🐛 Bug Fixes
- **Desktop:** prevent a startup crash ([#16](https://example.com/16)) by @octo
## 📝 Other Changes
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
    `${THANK_YOU}

## 📦 Dependency Updates
- update sqlite ([#17](https://example.com/17)) by @octo
`,
  );
});

test("formats improvements as their own release category", () => {
  const notes = `### Improvements
- improve(desktop): simplify settings navigation ([#18](https://example.com/18)) by @octo
`;

  assert.equal(
    formatReleaseNotes(notes),
    `${THANK_YOU}

## 🌟 Improvements
- **Desktop:** simplify settings navigation ([#18](https://example.com/18)) by @octo
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
    `${THANK_YOU}

## 💥 Breaking Changes
- **MCP:** replace the protocol ([#18](https://example.com/18)) by @octo
## 📝 Other Changes
- Plain historical change ([#19](https://example.com/19)) by @octo
`,
  );
});

test("keeps historical titles with malformed scopes without crashing", () => {
  const notes = `### Bug Fixes
- fix(ui/): keep release automation running ([#20](https://example.com/20)) by @octo
`;

  assert.equal(
    formatReleaseNotes(notes),
    `${THANK_YOU}

## 🐛 Bug Fixes
- fix(ui/): keep release automation running ([#20](https://example.com/20)) by @octo
`,
  );
});

test("keeps the larger hierarchy stable when formatting an existing draft", () => {
  const notes = `${THANK_YOU}

## ✨ New Features
### Desktop
- add document search ([#12](https://example.com/12)) by @octo
`;

  assert.equal(formatReleaseNotes(notes), notes);
});

test("drops the page heading and empty first-contributor section", () => {
  const notes = `${THANK_YOU}

# What's Changed

## ✨ New Features
- add document search ([#12](https://example.com/12)) by @octo

## 🙌 New Contributors


**Full Changelog**: https://github.com/brightwave-inc/tidebreak/compare/v0.1.0...v0.2.0
`;

  assert.equal(
    formatReleaseNotes(notes),
    `${THANK_YOU}

## ✨ New Features
- add document search ([#12](https://example.com/12)) by @octo

**Full Changelog**: https://github.com/brightwave-inc/tidebreak/compare/v0.1.0...v0.2.0
`,
  );
});

test("keeps first-time contributors and a compare link", () => {
  const notes = `## ✨ New Features
- add document search ([#12](https://example.com/12)) by @octo

## 🙌 New Contributors

- @newbie made their first contribution in [#21](https://example.com/21)

**Full Changelog**: https://github.com/brightwave-inc/tidebreak/compare/v0.1.0...v0.2.0
`;

  assert.equal(
    formatReleaseNotes(notes),
    `${THANK_YOU}

## ✨ New Features
- add document search ([#12](https://example.com/12)) by @octo

## 🙌 New Contributors

- @newbie made their first contribution in [#21](https://example.com/21)

**Full Changelog**: https://github.com/brightwave-inc/tidebreak/compare/v0.1.0...v0.2.0
`,
  );
});
