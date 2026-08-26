#!/usr/bin/env node
/**
 * Copy the desktop UI generated wire types into the mobile tree.
 * The files must stay byte-identical; do not add a header.
 */
import { copyFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "../..");
const src = join(
  root,
  "crates/tidebreak-desktop/ui/src/generated/wire.ts",
);
const dest = join(root, "mobile/src/generated/wire.ts");
copyFileSync(src, dest);
