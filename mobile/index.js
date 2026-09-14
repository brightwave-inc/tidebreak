// The app entry.
//
// `expo-router/entry` would be enough on its own, except for one thing: the
// Android notification render task must be *defined* before the OS can wake
// this bundle headlessly for a data-only push or a killed-app button press.
// Route modules are evaluated lazily by the router, so a task defined inside
// the app tree does not exist at that moment. Importing it here puts its
// definition in the entry chain's module scope, which every launch — headless
// included — evaluates first.
import "./src/push/renderTask";
import "expo-router/entry";
