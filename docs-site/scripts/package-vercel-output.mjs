import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const docsDirectory = path.resolve(scriptDirectory, '..');
const repositoryDirectory = path.resolve(docsDirectory, '..');
const exportDirectory = path.join(docsDirectory, 'out');
const outputDirectory = path.join(repositoryDirectory, '.vercel', 'output');
const staticDirectory = path.join(outputDirectory, 'static', 'docs');

for (const requiredFile of [
  'index.html',
  'quickstart/index.html',
  'search-index.json',
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

const config = {
  version: 3,
  routes: [
    {
      src: '^/docs(?:/.*)?$',
      headers: {
        'Content-Security-Policy': "default-src 'self'; base-uri 'self'; connect-src 'self'; font-src 'self'; form-action 'self'; frame-ancestors 'none'; frame-src 'none'; img-src 'self' data:; object-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; upgrade-insecure-requests",
        'Cross-Origin-Opener-Policy': 'same-origin',
        'Cross-Origin-Resource-Policy': 'same-origin',
        'Permissions-Policy': 'camera=(), geolocation=(), microphone=(), payment=(), usb=()',
        'Referrer-Policy': 'strict-origin-when-cross-origin',
        'X-Content-Type-Options': 'nosniff',
        'X-Frame-Options': 'DENY',
      },
      continue: true,
    },
    {
      src: '^/$',
      headers: { Location: '/docs/' },
      status: 308,
    },
    {
      src: '^/docs$',
      headers: { Location: '/docs/' },
      status: 308,
    },
    {
      src: '/docs/_next/static/.+',
      headers: {
        'cache-control': 'public,max-age=31536000,immutable',
      },
      continue: true,
    },
    { handle: 'filesystem' },
  ],
};

fs.writeFileSync(
  path.join(outputDirectory, 'config.json'),
  `${JSON.stringify(config, null, 2)}\n`,
);

console.log(`Vercel static output: ${staticDirectory}`);
