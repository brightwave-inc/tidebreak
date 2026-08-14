//! Table and column identifiers used by the schema baseline.

use sea_orm_migration::prelude::*;

#[derive(DeriveIden)]
pub(crate) enum AssistantCitation {
    Table,
    Id,
    MessageId,
    Ordinal,
    DocumentId,
    Locator,
}

#[derive(DeriveIden)]
pub(crate) enum Project {
    Table,
    Id,
    Title,
    AttachmentRevision,
    CreatedAt,
    Owner,
}

#[derive(DeriveIden)]
pub(crate) enum Document {
    Table,
    Id,
    ChatId,
    ProjectId,
    OriginUri,
    MediaType,
    Title,
    SourceBlobId,
    SourceSha256,
    SourceByteLen,
    CanonicalText,
    CreatedAt,
    UpdatedAt,
    Owner,
}

#[derive(DeriveIden)]
pub(crate) enum Output {
    Table,
    Id,
    ChatId,
    Filename,
    MediaType,
    CurrentRevisionId,
    RevisionCount,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
}

#[derive(DeriveIden)]
pub(crate) enum OutputRevision {
    Table,
    Id,
    OutputId,
    Ordinal,
    ByteLen,
    Sha256,
    TurnId,
    ProducingRunId,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum App {
    Table,
    Id,
    Name,
    CurrentRevisionId,
    RevisionCount,
    CreatedAt,
    UpdatedAt,
    DeletedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AppRevision {
    Table,
    Id,
    AppId,
    Ordinal,
    ManifestJson,
    ByteLen,
    Sha256,
    TurnId,
    ProducingRunId,
    ChatId,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AppGrant {
    Table,
    AppId,
    BindingsJson,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AppGatewayDraft {
    Table,
    AppId,
    GatewayBaseUrl,
    SharedAppId,
    GatewayRevisionId,
    SyncedRevisionId,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum ConnectedApp {
    Table,
    Id,
    Name,
    Kind,
    DefinitionJson,
    Position,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum OperationLog {
    Table,
    RunId,
    OperationId,
    State,
    Fingerprint,
    ExternalEffect,
    OwnerEpoch,
    Body,
    Retained,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum SandboxProvision {
    Table,
    RunId,
    Tag,
    State,
    Admission,
    Handle,
    LateResultEvidence,
    WindowExpiresAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum BlobRetirement {
    Table,
    BlobId,
    Status,
    AttemptCount,
    MaxAttempts,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum Chat {
    Table,
    Id,
    ProjectId,
    Title,
    Model,
    ReasoningEffort,
    PermissionMode,
    NetworkPolicy,
    AttachmentRevision,
    CreatedAt,
    Owner,
}

#[derive(DeriveIden)]
pub(crate) enum AgentRun {
    Table,
    Id,
    ChatId,
    ParentId,
    ParentDepth,
    SpawnCallId,
    Tier,
    ExecutionLocation,
    Depth,
    Status,
    Input,
    Model,
    AttemptCount,
    MaxAttempts,
    ClaimCount,
    CheckinGrants,
    CheckinWatermark,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    AvailableAt,
    DeadlineAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    OriginTurnId,
    DelegatedRootId,
    DelegatedRelativePath,
    AdmittedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum SandboxSpawnCheckpoint {
    Table,
    CallId,
    ChildRunId,
    ParentRunId,
    OriginTurnId,
    ChatId,
    LeaseToken,
    AttemptCount,
    ClaimCount,
    ProviderId,
    HistoryOrder,
    Arguments,
    Result,
    RemainingRequests,
    SteerRevision,
    EventOrdinal,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    EventSeq,
    CommittedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AdvisoryLock {
    Table,
    Name,
}

#[derive(DeriveIden)]
pub(crate) enum AgentRunClaim {
    Table,
    Token,
    AgentRunId,
    AttemptCount,
    ClaimCount,
    ClaimedAt,
    LeaseExpiresAt,
}

#[derive(DeriveIden)]
pub(crate) enum AgentRunResult {
    Table,
    AgentRunId,
    LeaseToken,
    AttemptCount,
    ClaimCount,
    PayloadKind,
    PayloadJson,
    Text,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    SubmittedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AgentRunProgress {
    Table,
    AgentRunId,
    Sequence,
    SourceKey,
    Text,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AgentRunCancellation {
    Table,
    AgentRunId,
    LeaseToken,
    AttemptCount,
    ClaimCount,
    Reason,
    RequestedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AgentRunInbox {
    Table,
    ChildRunId,
    ParentRunId,
    ChatId,
    ParentDepth,
    ResultLeaseToken,
    ResultAttemptCount,
    ResultClaimCount,
    Status,
    ClaimCount,
    LeaseToken,
    LeaseExpiresAt,
    ConsumedLeaseToken,
    ConsumedAt,
    DeliveredAt,
}

#[derive(DeriveIden)]
pub(crate) enum TurnAgentRunWaitSet {
    Table,
    Id,
    ParentRunId,
    TurnId,
    ChatId,
    Condition,
    ParkLeaseToken,
    ExpectedSteerRevision,
    AttemptCount,
    ClaimCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    EventOrdinal,
    EventSeq,
    Status,
    ParkedAt,
    ClosedAt,
    ResumeToken,
}

#[derive(DeriveIden)]
pub(crate) enum TurnAgentRunWaitMember {
    Table,
    WaitId,
    Position,
    ChildRunId,
    ParentRunId,
    OriginTurnId,
    ChatId,
    Open,
}

#[derive(DeriveIden, Clone, Copy)]
pub(crate) enum SandboxToolCall {
    Table,
    Id,
    AgentRunId,
    ChatId,
    AgentRunDepth,
    ProviderId,
    Name,
    Arguments,
    Status,
    ParkLeaseToken,
    ParkAttemptCount,
    ParkClaimCount,
    BatchOrdinal,
    ExecutorLeaseToken,
    ExecutorLeaseExpiresAt,
    RetryAt,
    ResolutionLeaseToken,
    Result,
    ErrorCode,
    ErrorDetail,
    CreatedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
pub(crate) enum ProjectRootAttachment {
    Table,
    ProjectId,
    RootId,
    Position,
}

#[derive(DeriveIden)]
pub(crate) enum ChatRootAttachment {
    Table,
    ChatId,
    RootId,
    Position,
    Origin,
}

#[derive(DeriveIden)]
pub(crate) enum RootAttachmentChange {
    Table,
    Id,
    ChatId,
    SubjectKind,
    SubjectId,
    ExecutorId,
    RootId,
    Action,
    Origin,
    ProjectionPosition,
    ProjectionExistedBefore,
    ExpectedRevision,
    BeforeRevision,
    IntentRevision,
    Phase,
    ResultRevision,
    ProjectionChanged,
    BrokerChanged,
    BrokerCurrentlyAttached,
    FailureCode,
    FailureMessage,
    FailureRetryable,
    CreatedAt,
    FinishedAt,
}

#[derive(DeriveIden)]
pub(crate) enum Message {
    Table,
    Id,
    ChatId,
    TurnId,
    Seq,
    Role,
    Content,
    LlmContent,
    Reasoning,
    TurnLeaseToken,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum ContextCheckpoint {
    Table,
    ChatId,
    SourceMessageId,
    SourceMessageSeq,
    FormatVersion,
    Content,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum ExecFileChange {
    Table,
    Id,
    ChatId,
    TurnId,
    Classification,
    FolderPath,
    RelativePath,
    ChangeKind,
    PriorBlobId,
    PriorByteLen,
    NewSha256,
    UndoState,
    Reason,
    RecordedAt,
}

#[derive(DeriveIden)]
pub(crate) enum ChatImagePublication {
    Table,
    ChatId,
    BlobId,
    MediaType,
    Width,
    Height,
    ByteLen,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum MessageAttachment {
    Table,
    MessageId,
    Ordinal,
    ChatId,
    BlobId,
    MediaType,
    Width,
    Height,
    ByteLen,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum MessageDocumentAttachment {
    Table,
    MessageId,
    Ordinal,
    ChatId,
    DocumentId,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum MessageIdentity {
    Table,
    Id,
    ChatId,
    TurnId,
    Owner,
}

#[derive(DeriveIden, Clone, Copy)]
pub(crate) enum TurnRun {
    Table,
    Id,
    ChatId,
    AgentRunId,
    AgentRunDepth,
    InputMessageId,
    OutputMessageId,
    Model,
    InvokedSkills,
    VoiceInputUsed,
    Status,
    AttemptCount,
    MaxAttempts,
    ClaimCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    AvailableAt,
    LeaseToken,
    LeaseExpiresAt,
    StartedAt,
    FinishedAt,
    LastErrorCode,
    LastErrorDetail,
    SteerRevision,
    LastSteerAppliedAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum TurnClaim {
    Table,
    Token,
    TurnId,
    AttemptCount,
    ClaimCount,
    ClaimedAt,
    LeaseExpiresAt,
}

#[derive(DeriveIden)]
pub(crate) enum TurnFailure {
    Table,
    LeaseToken,
    TurnId,
    AttemptCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    RequestedRetryAt,
    ErrorCode,
    ErrorDetail,
    ResolvedAt,
    ResultStatus,
}

#[derive(DeriveIden)]
pub(crate) enum TurnSteer {
    Table,
    Id,
    TurnId,
    ChatId,
    Content,
    InvokedSkills,
    VoiceInputUsed,
    Interrupt,
    Status,
    AppliedLeaseToken,
    MessageId,
    PrecedingAssistantMessageId,
    CreatedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
pub(crate) enum ToolCall {
    Table,
    Id,
    ChatId,
    TurnId,
    ProviderId,
    HistoryOrder,
    Name,
    Arguments,
    RawArguments,
    Execution,
    Status,
    Result,
    ResultPreview,
    ProviderReplay,
    ErrorCode,
    ErrorDetail,
    ApprovalStatus,
    ApprovalClass,
    ApprovalKind,
    ApprovalReason,
    ApprovalRequestedAt,
    ApprovalDecidedAt,
    ApprovalEventSeq,
    ApprovalGrantSourceCallId,
    AutoJudgeStatus,
    ClientExecutorId,
    ClientLeaseToken,
    ClientLeaseExpiresAt,
    TurnLeaseToken,
    ResolutionTurnLeaseToken,
    CreatedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
pub(crate) enum StandingToolGrant {
    Table,
    SourceCallId,
    ChatId,
    ProjectId,
    ToolName,
    ApprovalKind,
    Scope,
    GrantedAt,
}

#[derive(DeriveIden)]
pub(crate) enum TurnClientWait {
    Table,
    CallId,
    TurnId,
    ChatId,
    ParkLeaseToken,
    AttemptCount,
    ClaimCount,
    ModelSteps,
    InputTokens,
    OutputTokens,
    CacheReadInputTokens,
    CacheCreationInputTokens,
    Status,
    ParkedAt,
    ClosedAt,
}

#[derive(DeriveIden)]
pub(crate) enum UserQuestionRequest {
    Table,
    CallId,
    TurnId,
    ChatId,
    Status,
    EventSeq,
    AskedAt,
    ResolvedAt,
    AdditionalUserContext,
}

#[derive(DeriveIden)]
pub(crate) enum PlanRequest {
    Table,
    CallId,
    TurnId,
    ChatId,
    Status,
    EventSeq,
    Title,
    Plan,
    Feedback,
    ProposedAt,
    ResolvedAt,
}

#[derive(DeriveIden)]
pub(crate) enum TaskPlan {
    Table,
    ChatId,
    TurnId,
    CallId,
    Steps,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum AgentRunTaskPlan {
    Table,
    AgentRunId,
    CallId,
    Steps,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum UserQuestion {
    Table,
    CallId,
    QuestionId,
    Position,
    Header,
    Prompt,
    Options,
    QuestionType,
    AllowFreeForm,
    AnswerSelectedOptionIds,
    AnswerCustomAnswer,
    ResponseRecordedAt,
}

#[derive(DeriveIden)]
pub(crate) enum Setting {
    Table,
    Key,
    ValueJson,
}

#[derive(DeriveIden)]
pub(crate) enum Event {
    Table,
    ChatId,
    Seq,
    TurnId,
    LeaseToken,
    AttemptEventOrdinal,
    ScanToken,
    Terminal,
    Payload,
    CreatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum TurnAdmission {
    Table,
    Id,
    ChatId,
    Fingerprint,
    State,
    LeaseToken,
    LeaseExpiresAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
pub(crate) enum QueuedTurn {
    Table,
    Id,
    ChatId,
    Content,
    AttachmentsJson,
    FileAttachmentsJson,
    InvokedSkillsJson,
    VoiceInputUsed,
    Position,
    CreatedAt,
    UpdatedAt,
}
