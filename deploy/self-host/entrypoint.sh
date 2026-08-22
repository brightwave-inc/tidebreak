#!/bin/sh
# Publish the image's managed Node runtime into the data directory, then start
# the server.
#
# The server resolves the runtime from one exact path,
# `{TIDEBREAK_DATA_DIR}/tools/node/{version}`, and refuses to look anywhere
# else (crates/tidebreak-code-execution/src/managed_node.rs). That path is
# inside the data directory, which every real deployment mounts a volume over
# — and a mount hides whatever the image layer put underneath it. Unpacking
# Node there at build time would therefore work only on a container whose data
# volume is brand new, and break on exactly the upgrade path operators take.
#
# So the image keeps one copy under /opt and this script points the data
# directory at it on every start. The link costs nothing, survives a volume
# that already has content, and leaves a real install already sitting at that
# path untouched.
#
# Keep `node_version` equal to the version the Dockerfile installs and to
# `MANAGED_NODE_VERSION`; scripts/self-host-image-pins.test.mjs enforces it.
set -eu

node_version=20.20.2
image_node_dir="/opt/tidebreak/node/${node_version}"
data_dir="${TIDEBREAK_DATA_DIR:-/var/lib/tidebreak}"
link="${data_dir}/tools/node/${node_version}"

link_managed_node() {
    if [ ! -d "${image_node_dir}" ]; then
        echo "tidebreak: this image carries no managed Node runtime at ${image_node_dir}" >&2
        return 1
    fi
    # Already resolvable, either from an earlier start or because someone
    # installed a real runtime there. The server verifies whatever it finds
    # against the same pin, so leave it alone. `-d` follows the link, so a
    # link whose target went away is replaced rather than kept.
    if [ -d "${link}" ]; then
        return 0
    fi
    mkdir -p "${data_dir}/tools/node" || return 1
    ln -sfn "${image_node_dir}" "${link}" || return 1
}

# Code mode is one feature of the server, so a data directory this process
# cannot write is worth a loud warning, not a refusal to boot: chat still
# works, and the server's own startup checks are what own the data directory.
if ! link_managed_node; then
    echo "tidebreak: could not publish the managed Node runtime to ${link};" \
         "code mode will report every engine as not found" >&2
fi

exec /usr/local/bin/tidebreak serve "$@"
