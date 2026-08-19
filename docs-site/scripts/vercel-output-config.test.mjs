import assert from 'node:assert/strict';
import test from 'node:test';

import { VERCEL_OUTPUT_CONFIG } from './vercel-output-config.mjs';

test('the docs route carries the release security headers', () => {
  const docsRoute = VERCEL_OUTPUT_CONFIG.routes.find(
    (route) => route.src === '^/docs(?:/.*)?$',
  );
  assert.ok(docsRoute);

  const csp = docsRoute.headers['Content-Security-Policy'];
  assert.match(csp, /(?:^|; )default-src 'self'(?:;|$)/);
  assert.match(csp, /(?:^|; )object-src 'none'(?:;|$)/);
  assert.match(csp, /(?:^|; )frame-ancestors 'none'(?:;|$)/);
  assert.equal(docsRoute.headers['Cross-Origin-Opener-Policy'], 'same-origin');
  assert.equal(docsRoute.headers['Cross-Origin-Resource-Policy'], 'same-origin');
  assert.equal(docsRoute.headers['X-Content-Type-Options'], 'nosniff');
  assert.equal(docsRoute.headers['X-Frame-Options'], 'DENY');
});

test('the output redirects only the root docs entry points', () => {
  assert.deepEqual(
    VERCEL_OUTPUT_CONFIG.routes
      .filter((route) => route.status === 308)
      .map(({ src, headers }) => [src, headers.Location]),
    [
      ['^/$', '/docs/'],
      ['^/docs$', '/docs/'],
    ],
  );
  assert.deepEqual(
    VERCEL_OUTPUT_CONFIG.routes.at(-1),
    { handle: 'filesystem' },
  );
});

test('the legacy search-index URL serves the Fumadocs search export', () => {
  assert.deepEqual(
    VERCEL_OUTPUT_CONFIG.routes.find(
      (route) => route.src === '^/docs/search-index\\.json$',
    ),
    {
      src: '^/docs/search-index\\.json$',
      dest: '/docs/api/search',
    },
  );
});
