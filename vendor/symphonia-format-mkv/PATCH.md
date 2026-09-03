# Tidebreak patch provenance

This directory vendors `symphonia-format-mkv` 0.6.0 from crates.io.

- Upstream repository: <https://github.com/pdeljanov/Symphonia>
- Upstream pull request: <https://github.com/pdeljanov/Symphonia/pull/541>
- Upstream commit: `619c6806e5122b3206b8938773a801dd4a8a5950`
- Local adaptation: `src/ebml.rs` validates a candidate child at the depth below
  its ancestor, allowing an unknown-size Cluster to end when the next sibling
  Cluster begins. This lets Symphonia demux WebM recordings produced by a
  browser `MediaRecorder` writing to a non-seekable sink.

The original removal condition for this backport was: "drop this directory and
the patch entry once symphonia 0.6.1 ships". Symphonia 0.6.1 was available on
crates.io by September 3, 2026; remove this directory and the patch entry when
Tidebreak's exact Symphonia pin moves to 0.6.1 or newer.
