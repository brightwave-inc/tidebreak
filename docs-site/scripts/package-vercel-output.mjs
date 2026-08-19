import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { VERCEL_OUTPUT_CONFIG } from './vercel-output-config.mjs';

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const docsDirectory = path.resolve(scriptDirectory, '..');
const repositoryDirectory = path.resolve(docsDirectory, '..');
const exportDirectory = path.join(docsDirectory, 'out');
const outputDirectory = path.join(repositoryDirectory, '.vercel', 'output');
const staticDirectory = path.join(outputDirectory, 'static', 'docs');

for (const requiredFile of [
  'index.html',
  'quickstart/index.html',
  'api/search',
  'sitemap.xml',
]) {
  const requiredPath = path.join(exportDirectory, requiredFile);
  if (!fs.existsSync(requiredPath)) {
    throw new Error(`Static documentation is missing ${requiredPath}`);
  }
}

fs.rmSync(outputDirectory, { recursive: true, force: true });
fs.mkdirSync(staticDirectory, { recursive: true });
fs.cpSync(exportDirectory, staticDirectory, { recursive: true });

fs.writeFileSync(
  path.join(outputDirectory, 'config.json'),
  `${JSON.stringify(VERCEL_OUTPUT_CONFIG, null, 2)}\n`,
);

console.log(`Vercel static output: ${staticDirectory}`);
