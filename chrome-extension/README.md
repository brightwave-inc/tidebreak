# Tidebreak Chrome bridge (local unpacked extension)

Load this directory at `chrome://extensions` with Developer mode enabled. It
communicates only with the native messaging host named by
`chrome.runtime.connectNative`, which Tidebreak's host setup registers. The
extension shares explicit tab references; the native host enforces the same
`DeveloperAllSites` consent before any tab is used. Store publication is a
separate product decision outside this slice.
