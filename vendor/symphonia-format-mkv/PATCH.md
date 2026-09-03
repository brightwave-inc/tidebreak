# Vendored symphonia-format-mkv fix

This directory vendors `symphonia-format-mkv` 0.6.0 from crates.io with the
WebM/MediaRecorder fix from upstream Symphonia applied.

- Upstream repository: https://github.com/pdeljanov/Symphonia
- Upstream PR: #541
- Upstream commit: 619c6806e5122b3206b8938773a801dd4a8a5950
- What it fixes: the EBML iterator treated unknown-size clusters as the end of
  the parent, so WebM files recorded by browsers stopped demuxing at the first
  cluster boundary. The vendor patch carries the upstream iterator correction
  needed to continue reading those clusters.

Drop this directory and the `symphonia-format-mkv` entry in the root
`[patch.crates-io]` once symphonia 0.6.1 ships.
