// The design-system surface synced to claude.ai/design (see .design-sync/).
// Re-exports every shared visual component so the design tool builds with the
// app's real parts. App wiring (routes, stores, views) stays out on purpose.
// Unreferenced by the app itself; bundled only by the design-sync converter.

export * from "./components/ui/alert-dialog";
export * from "./components/ui/badge";
export * from "./components/ui/button";
export * from "./components/ui/card";
export * from "./components/ui/checkbox";
export * from "./components/ui/dialog";
export * from "./components/ui/dropdown-menu";
export * from "./components/ui/empty";
export * from "./components/ui/input";
export * from "./components/ui/popover";
export * from "./components/ui/progress";
export * from "./components/ui/radio-group";
export * from "./components/ui/resizable";
export * from "./components/ui/scroll-area";
export * from "./components/ui/select";
export * from "./components/ui/separator";
export * from "./components/ui/skeleton";
export * from "./components/ui/sonner";
export * from "./components/ui/spinner";
export * from "./components/ui/switch";
export * from "./components/ui/table";
export * from "./components/ui/tabs";
export * from "./components/ui/textarea";
export * from "./components/ui/toggle";
export * from "./components/ui/tooltip";

export * from "./components/PanelHeader";
export * from "./components/SearchInput";
export * from "./components/OptionListbox";

export * from "./MessageMarkdown";
export * from "./ToolCardShell";
export * from "./ThinkingAccordion";
export * from "./ClipboardCopyButton";
export * from "./MessageFooter";
export * from "./AssistantWorkingIndicator";
export * from "./Logomark";
export * from "./ApprovalCard";
export * from "./ToolActivityGroup";
export * from "./ToolCallCard";
export * from "./ToolIcon";
export * from "./ToolStatusIcon";
export * from "./ChatStatusChip";
export * from "./AttentionCard";
export * from "./InlineCitation";
export * from "./DomainFavicon";
export * from "./ScrollableContainer";
export * from "./TurnFailureNotice";
export * from "./ContextUsageIndicator";
export * from "./ChangeSummaryCard";
export * from "./AppCard";

export * from "./sidebar/primitives";

export * from "./code/AttentionBadge";
export * from "./code/TurnReviewCard";
export * from "./code/HarnessPicker";
export * from "./code/DoctorList";
export * from "./code/CodeTranscript";
