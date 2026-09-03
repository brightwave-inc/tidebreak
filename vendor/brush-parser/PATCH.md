# Vendored brush-parser dependency trim

This directory vendors `brush-parser` 0.4.0 from crates.io. It remained the
newest published version on 2026-09-03.

The published manifest lists `insta` as a production dependency even though
the crate uses it only in tests. It also uses `cached` proc-macro attributes on
three parser functions. Tidebreak parses short approval commands and does not
need those process-wide caches.

This patch keeps the parser behavior and public API unchanged. It moves
`insta` back to the existing development dependency and removes the three
cache attributes plus the `cached` dependency.

Drop this directory and the `brush-parser` entry in the root
`[patch.crates-io]` when an upstream release makes both dependencies optional
or removes them from production builds.
