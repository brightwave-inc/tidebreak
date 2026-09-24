/**
 * Runtime validators for every code-mode wire type the desktop consumes.
 *
 * Generated types describe the JSON; these functions decide whether a payload
 * is safe to keep. A field the server renamed or a notice that grew control
 * characters must fail here rather than render as if it were well-formed.
 *
 * Each domain's validators live in their own module beside this one, and
 * the string bounds they share live in `shared.ts`. Import from this barrel.
 */

export { parseCodeSubscriptionUsage, parseCodeAnalytics } from "./analytics";
export {
  parseCodeWorkspacePullRequests,
  parseCodeDeliveryRepositories,
  parseCodeDeliveryPullRequestsPage,
  parseCodeDeliveryPullRequestDetail,
  parseCodeDeliveryRunsPage,
  parseCodeDeliveryRunDetail,
  parseCodeDeliveryActionResult,
} from "./delivery";
export { parseSequencedCodeEvent, parseCodeEvent } from "./events";
export {
  parseCodeWorkspaceTree,
  parseCodeWorkspaceSearch,
  parseCodeWorkspaceFiles,
  parseCodeWorkspaceBlob,
  parseCodeWorkspaceFileSaved,
  parseCodeWorkspaceDiff,
  parseCheckpointRestoreTarget,
  parseCodeCheckpointRestorePreview,
  parseCodeCheckpointRestoreResult,
  parseCodeWorktreeChange,
  parseDiffstat,
} from "./files";
export {
  parseCodeGrant,
  parseCodeGrantList,
  parseCodeConnectPage,
} from "./grants";
export {
  parseCodeHarnessInstall,
  parseHarnessModelList,
  parseHarnessDoctorReport,
  parseHarnessDoctorEntry,
} from "./harness";
export type { ParsedHarnessModel, ParsedHarnessModelList } from "./harness";
export {
  parseCodeCloneJob,
  parseCodeCloneDefaults,
  parseCodeRepoSources,
  parseCodeGithubRepositories,
  parseCodeWorktreeRoot,
  parseCodeRepo,
  parseCodeRepoTrust,
} from "./repos";
export { parseCodeReview, parseCodeReviewList } from "./review";
export {
  parseCodeForkTranscript,
  parseCodeSession,
  parseCodeSessionList,
  workspaceCodeSessions,
  parseAttention,
  parseFenceReason,
} from "./sessions";
export {
  parseCodeTerminal,
  parseCodeTerminalList,
  parseHarnessSignInTerminal,
  parseHarnessSignInRead,
  parseCodeTerminalRead,
} from "./terminals";
export {
  parseCodeTurn,
  parseTurnActor,
  parseQueuedCodeTurn,
  parseCodeTurnSubmission,
  parseCodeTurnList,
  parseCodeApproval,
} from "./turns";
export type { CodeTurnSubmission } from "./turns";
export { parseCodeSessionDigest, parseCodeUpdateNotice } from "./updates";
export {
  parseCodeCheckLogsSnapshot,
  parseCodeWorkspace,
  parsePullRequestDigest,
  parseCodeWorkspacePr,
  parseCodeWatch,
  parseCodeTrigger,
  parseCodeTriggers,
  parseCodePrComments,
  parseCodeCommit,
  parseCodePush,
  parseCodeAction,
} from "./workspaces";
