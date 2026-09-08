// Tidebreak Chrome computer use local bridge.
// This unpacked extension shares only explicit tab references through the
// Tidebreak native messaging host. It never receives debugger endpoints and
// never grants the model direct CDP access.
const NATIVE_HOST = "dev.tidebreak.chrome_bridge";

let port = null;

function connectHost() {
  if (port) return port;
  try {
    port = chrome.runtime.connectNative(NATIVE_HOST);
    port.onMessage.addListener((message) => {
      if (message && message.kind === "activateTab" && message.tabId) {
        chrome.tabs.update(message.tabId, {active: true}).catch(() => {});
      }
    });
    port.onDisconnect.addListener(() => {
      port = null;
    });
  } catch (_) {
    port = null;
  }
  return port;
}

chrome.runtime.onInstalled.addListener(connectHost);
chrome.runtime.onStartup.addListener(connectHost);
chrome.action.onClicked.addListener(() => {
  connectHost();
});

// User-visible tab reference messages are bounded and scoped: the host
// resolves them only after the explicit wider-grant consent.
setInterval(() => {
  if (!port) return;
  chrome.tabs.query({}, (tabs) => {
    const summaries = tabs.slice(0, 64).map((tab) => ({
      tabId: tab.id,
      windowId: tab.windowId,
      url: (tab.url || "").slice(0, 4096),
      title: (tab.title || "").slice(0, 1024),
      active: Boolean(tab.active),
    }));
    try {
      port.postMessage({kind: "tabs", tabs: summaries});
    } catch (_) {}
  });
}, 5000);
