# The E2B template built from the published Tidebreak documents image.
#
# E2B provisions sandboxes from account-registered templates, not arbitrary OCI
# refs, so the image reaches E2B by being built into a template and published
# (made public) from the Tidebreak account. A published template is usable by
# every E2B account by alias, which is why a user who pastes only an API key
# still gets this image. See README.md for the publish and version-bump flow.
#
# Digest-pinned so the template's contents are exactly the image we tested:
# ghcr.io/brightwave-inc/tidebreak-sandbox-agent-documents:main-20260820-0ea836f-r642.
FROM ghcr.io/brightwave-inc/tidebreak-sandbox-agent-documents@sha256:e0caf06b0ecf3f138ef6c11769357533f4dc8843de4f2cc8b01f6e77c0bfcb10

# E2B replaces the image's entrypoint with envd as PID 1 and runs commands as
# the `user` account out of /home/user — the root the E2B provider's file and
# command APIs address. The Tidebreak image's own unprivileged account is
# `tidebreak` with a different home, so make E2B's expected identity exist
# rather than relying on the builder to add it. The `id` guard keeps this a
# no-op if a future base already ships the account.
USER root
RUN (id -u user >/dev/null 2>&1 \
      || useradd --create-home --home-dir /home/user --shell /bin/bash user) \
    && mkdir -p /home/user \
    && chown -R user:user /home/user

# The document helpers are installed system-wide in the base image (LibreOffice,
# poppler, and the hash-pinned Python dependency closure), so nothing else is
# layered here: no start command, and no in-sandbox package install at run time.
# The sandbox agent binary the image also carries is unused on E2B — envd is the
# transport there, and Tidebreak's E2B provider speaks to envd directly.
