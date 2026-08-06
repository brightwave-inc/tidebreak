import { BarChart3, FileText, Package, Table2 } from "lucide-react";
import type { LucideIcon } from "lucide-react";

import type { PluginCapability, PluginCategory } from "@/api";

/**
 * A short, human reading of one derived badge.
 *
 * The vocabulary is closed server-side, so this table is total: a capability
 * the host can derive always has words here rather than falling back to the
 * wire slug. `live-control` is not derived by anything today; it is named
 * anyway because the surface renders whatever comes back, and the day a bundle
 * earns one it should read like the others.
 */
const CAPABILITY_LABELS: Record<PluginCapability, string> = {
  "write-files": "Writes files",
  network: "Installs packages over the network",
  "host-install": "Installs host software",
  "live-control": "Controls a live surface",
  mcp: "Bundles an MCP server",
};

export function capabilityLabel(capability: PluginCapability): string {
  return CAPABILITY_LABELS[capability];
}

/**
 * The short form for a list row, where a badge sits beside a name rather than
 * on a detail page with room to explain itself.
 */
const CAPABILITY_SHORT_LABELS: Record<PluginCapability, string> = {
  "write-files": "Files",
  network: "Network",
  "host-install": "Host install",
  "live-control": "Live control",
  mcp: "MCP",
};

export function capabilityShortLabel(capability: PluginCapability): string {
  return CAPABILITY_SHORT_LABELS[capability];
}

const CATEGORY_LABELS: Record<PluginCategory, string> = {
  documents: "Documents",
  data: "Data",
  visualization: "Visualization",
  other: "Other",
};

export function categoryLabel(category: PluginCategory): string {
  return CATEGORY_LABELS[category];
}

/** A plugin has no icon of its own yet, so its category stands in for one. */
const CATEGORY_ICONS: Record<PluginCategory, LucideIcon> = {
  documents: FileText,
  data: Table2,
  visualization: BarChart3,
  other: Package,
};

export function categoryIcon(category: PluginCategory): LucideIcon {
  return CATEGORY_ICONS[category];
}
