export const VERCEL_OUTPUT_CONFIG = {
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
      src: '^/docs/search-index\\.json$',
      dest: '/docs/api/search',
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
