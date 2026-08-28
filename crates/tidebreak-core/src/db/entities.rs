pub mod blob_retirement {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "blob_retirement")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub blob_id: Uuid,
        pub status: String,
        pub attempt_count: i32,
        pub max_attempts: i32,
        pub available_at: DateTimeUtc,
        pub lease_token: Option<Uuid>,
        pub lease_expires_at: Option<DateTimeUtc>,
        pub started_at: Option<DateTimeUtc>,
        pub finished_at: Option<DateTimeUtc>,
        pub last_error_code: Option<String>,
        pub last_error_detail: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod document {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "document")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Option<Uuid>,
        pub project_id: Option<Uuid>,
        pub origin_uri: Option<String>,
        pub media_type: String,
        pub title: Option<String>,
        pub source_blob_id: Option<Uuid>,
        pub source_sha256: Option<Vec<u8>>,
        pub source_byte_len: Option<i64>,
        #[sea_orm(column_type = "Text")]
        pub canonical_text: String,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
        pub owner: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod project {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "project")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub title: Option<String>,
        pub attachment_revision: i64,
        pub created_at: DateTimeUtc,
        pub owner: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(has_many = "super::project_root_attachment::Entity")]
        RootAttachment,
    }

    impl Related<super::project_root_attachment::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::RootAttachment.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod chat {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "chat")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub project_id: Option<Uuid>,
        pub title: Option<String>,
        pub model: Option<String>,
        pub reasoning_effort: Option<String>,
        pub permission_mode: Option<String>,
        pub network_policy: String,
        pub attachment_revision: i64,
        pub created_at: DateTimeUtc,
        pub owner: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(has_many = "super::chat_root_attachment::Entity")]
        RootAttachment,
        #[sea_orm(has_many = "super::root_attachment_change::Entity")]
        RootAttachmentChange,
    }

    impl Related<super::chat_root_attachment::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::RootAttachment.def()
        }
    }

    impl Related<super::root_attachment_change::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::RootAttachmentChange.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod project_root_attachment {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "project_root_attachment")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub project_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub root_id: Uuid,
        pub position: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::project::Entity",
            from = "Column::ProjectId",
            to = "super::project::Column::Id",
            on_update = "NoAction",
            on_delete = "Cascade"
        )]
        Project,
    }

    impl Related<super::project::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Project.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod chat_root_attachment {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "chat_root_attachment")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub chat_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub root_id: Uuid,
        pub position: i32,
        pub origin: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::chat::Entity",
            from = "Column::ChatId",
            to = "super::chat::Column::Id",
            on_update = "NoAction",
            on_delete = "Cascade"
        )]
        Chat,
    }

    impl Related<super::chat::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Chat.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod root_attachment_change {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "root_attachment_change")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub subject_kind: String,
        pub subject_id: Uuid,
        pub executor_id: Uuid,
        pub root_id: Uuid,
        pub action: String,
        pub origin: Option<String>,
        pub projection_position: Option<i32>,
        pub projection_existed_before: bool,
        pub expected_revision: i64,
        pub before_revision: i64,
        pub intent_revision: i64,
        pub phase: String,
        pub result_revision: Option<i64>,
        pub projection_changed: Option<bool>,
        pub broker_changed: Option<bool>,
        pub broker_currently_attached: Option<bool>,
        pub failure_code: Option<String>,
        pub failure_message: Option<String>,
        pub failure_retryable: Option<bool>,
        pub created_at: DateTimeUtc,
        pub finished_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::chat::Entity",
            from = "Column::ChatId",
            to = "super::chat::Column::Id",
            on_update = "NoAction",
            on_delete = "Restrict"
        )]
        Chat,
    }

    impl Related<super::chat::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Chat.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod message {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "message")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub seq: i64,
        pub role: String,
        pub content: String,
        pub llm_content: Option<String>,
        pub reasoning: Option<Json>,
        pub turn_lease_token: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod context_checkpoint {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "context_checkpoint")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub chat_id: Uuid,
        pub source_message_id: Uuid,
        pub source_message_seq: i64,
        pub format_version: i32,
        pub content: String,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod sandbox_provision {
    use sea_orm::entity::prelude::*;

    /// One container run's durable provisioning record, keyed by the run id
    /// (container runs have exactly one execution attempt). Written before the
    /// backend's create call, so recovery — the window lapse and the tag sweep —
    /// is driven by the intent rather than by what the provider reports. See
    /// issue #920.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "sandbox_provision")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub run_id: Uuid,
        /// The host-minted correlation tag stamped into the sandbox's metadata.
        pub tag: String,
        /// `intended` | `committed` | `teardown` | `done`.
        pub state: String,
        /// `attached_only` | `detached` — the run's durable admission
        /// decision, recorded before the create call. Fail closed: anything
        /// unrecognized reads as `attached_only`.
        pub admission: String,
        /// The backend's sandbox reference, `NULL` until committed.
        pub handle: Option<String>,
        /// A well-formed result that arrived after the run was already
        /// terminal: retained as non-authoritative evidence, never committed.
        pub late_result_evidence: Option<String>,
        /// When the provisioning window lapses for an `intended` record.
        pub window_expires_at: DateTimeUtc,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod operation_log {
    use sea_orm::entity::prelude::*;

    /// One durable reverse-RPC operation-log entry, keyed by `(run_id,
    /// operation_id)`. Bodies are opaque blobs; the protocol tier owns their
    /// typed meaning. See `tidebreak-sandbox-protocol::oplog` and issue #858.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "operation_log")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub run_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub operation_id: Uuid,
        /// `claimed` | `recorded` | `failed`.
        pub state: String,
        /// The serialized request the identity was first claimed with; a later
        /// re-issue with a different fingerprint is a conflict.
        pub fingerprint: Vec<u8>,
        /// Whether the claimed operation carries an external effect.
        pub external_effect: bool,
        /// The process lifetime that holds the claim, distinguishing a
        /// concurrent duplicate from an after-crash re-issue.
        pub owner_epoch: Uuid,
        /// The recorded terminal body, `NULL` while `claimed` or once #859
        /// evicts the body down to a commit marker.
        pub body: Option<Vec<u8>>,
        /// Whether the terminal body is still retained (#859 clears this when it
        /// keeps only a commit marker).
        pub retained: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod message_attachment {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "message_attachment")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub message_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub ordinal: i32,
        pub chat_id: Uuid,
        pub blob_id: Uuid,
        pub media_type: String,
        pub width: i32,
        pub height: i32,
        pub byte_len: i64,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::message::Entity",
            from = "Column::MessageId",
            to = "super::message::Column::Id",
            on_update = "NoAction",
            on_delete = "Restrict"
        )]
        Message,
    }

    impl Related<super::message::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Message.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod chat_image_publication {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "chat_image_publication")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub chat_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub blob_id: Uuid,
        pub media_type: String,
        pub width: i32,
        pub height: i32,
        pub byte_len: i64,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::chat::Entity",
            from = "Column::ChatId",
            to = "super::chat::Column::Id",
            on_update = "NoAction",
            on_delete = "Restrict"
        )]
        Chat,
    }

    impl Related<super::chat::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Chat.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod exec_file_change {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "exec_file_change")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub classification: String,
        pub folder_path: String,
        pub relative_path: String,
        pub change_kind: Option<String>,
        pub prior_blob_id: Option<Uuid>,
        pub prior_byte_len: Option<i64>,
        pub new_sha256: Option<String>,
        pub undo_state: Option<String>,
        pub reason: Option<String>,
        pub recorded_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::chat::Entity",
            from = "Column::ChatId",
            to = "super::chat::Column::Id",
            on_update = "NoAction",
            on_delete = "Restrict"
        )]
        Chat,
    }

    impl Related<super::chat::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Chat.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod message_document_attachment {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "message_document_attachment")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub message_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub ordinal: i32,
        pub chat_id: Uuid,
        pub document_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::message::Entity",
            from = "Column::MessageId",
            to = "super::message::Column::Id",
            on_update = "NoAction",
            on_delete = "Cascade"
        )]
        Message,
        #[sea_orm(
            belongs_to = "super::document::Entity",
            from = "Column::DocumentId",
            to = "super::document::Column::Id",
            on_update = "NoAction",
            on_delete = "Cascade"
        )]
        Document,
    }

    impl Related<super::message::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Message.def()
        }
    }

    impl Related<super::document::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Document.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod agent_run {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub chat_id: Uuid,
        pub parent_id: Option<Uuid>,
        pub parent_depth: Option<i16>,
        pub spawn_call_id: Option<Uuid>,
        pub tier: String,
        pub execution_location: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub depth: i16,
        pub status: String,
        pub input: Option<String>,
        pub model: Option<String>,
        pub attempt_count: i32,
        pub max_attempts: i32,
        pub claim_count: i32,
        pub checkin_grants: i32,
        pub checkin_watermark: i32,
        pub model_steps: i32,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub available_at: DateTimeUtc,
        pub deadline_at: Option<DateTimeUtc>,
        pub lease_token: Option<Uuid>,
        pub lease_expires_at: Option<DateTimeUtc>,
        pub started_at: Option<DateTimeUtc>,
        pub finished_at: Option<DateTimeUtc>,
        pub last_error_code: Option<String>,
        pub last_error_detail: Option<String>,
        pub origin_turn_id: Option<Uuid>,
        pub delegated_root_id: Option<Uuid>,
        pub delegated_relative_path: Option<String>,
        pub admitted_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_admission {
    use sea_orm::entity::prelude::*;

    /// Global owner and immutable request fingerprint for one client turn id.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_admission")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub fingerprint: Vec<u8>,
        /// `pending` | `queued` | `accepted`.
        pub state: String,
        pub lease_token: Option<Uuid>,
        pub lease_expires_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod queued_turn {
    use sea_orm::entity::prelude::*;

    /// One message accepted while its chat had a live turn, waiting to become
    /// a real turn when the chat is free. The row id is the client-generated
    /// turn id the promotion will accept under, so an ambiguous promotion
    /// retry lands on `AcceptTurnOutcome::Existing` instead of a duplicate.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "queued_turn")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub content: String,
        /// Image-attachment ids, JSON array of UUID strings.
        pub attachments_json: String,
        /// Chat-owned document ids, JSON array of UUID strings.
        pub file_attachments_json: String,
        /// Invoked skill names, JSON array of strings.
        pub invoked_skills_json: String,
        pub voice_input_used: bool,
        /// FIFO order within the chat; reorder rewrites positions.
        pub position: i32,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod sandbox_spawn_checkpoint {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "sandbox_spawn_checkpoint")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub call_id: Uuid,
        pub child_run_id: Uuid,
        pub parent_run_id: Uuid,
        pub origin_turn_id: Uuid,
        pub chat_id: Uuid,
        pub lease_token: Uuid,
        pub attempt_count: i32,
        pub claim_count: i32,
        pub provider_id: String,
        pub history_order: i64,
        #[sea_orm(column_type = "JsonBinary")]
        pub arguments: Json,
        pub result: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub remaining_requests: Json,
        pub steer_revision: i64,
        pub event_ordinal: i32,
        pub model_steps: i32,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub event_seq: i64,
        pub committed_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod advisory_lock {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "advisory_lock")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod agent_run_claim {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run_claim")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub token: Uuid,
        pub agent_run_id: Option<Uuid>,
        pub attempt_count: Option<i32>,
        pub claim_count: Option<i32>,
        pub claimed_at: DateTimeUtc,
        pub lease_expires_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod agent_run_result {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run_result")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub agent_run_id: Uuid,
        pub lease_token: Uuid,
        pub attempt_count: i32,
        pub claim_count: i32,
        pub payload_kind: String,
        pub payload_json: String,
        pub text: String,
        pub model_steps: i32,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub submitted_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod agent_run_progress {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run_progress")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub agent_run_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub sequence: i64,
        pub source_key: String,
        pub text: String,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod agent_run_cancellation {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run_cancellation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub agent_run_id: Uuid,
        pub lease_token: Uuid,
        pub attempt_count: i32,
        pub claim_count: i32,
        pub reason: String,
        pub requested_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod agent_run_inbox {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run_inbox")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub child_run_id: Uuid,
        pub parent_run_id: Uuid,
        pub chat_id: Uuid,
        pub parent_depth: i16,
        pub result_lease_token: Uuid,
        pub result_attempt_count: i32,
        pub result_claim_count: i32,
        pub status: String,
        pub claim_count: i32,
        pub lease_token: Option<Uuid>,
        pub lease_expires_at: Option<DateTimeUtc>,
        pub consumed_lease_token: Option<Uuid>,
        pub consumed_at: Option<DateTimeUtc>,
        pub delivered_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod sandbox_tool_call {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "sandbox_tool_call")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub agent_run_id: Uuid,
        pub chat_id: Uuid,
        pub agent_run_depth: i16,
        pub provider_id: String,
        pub name: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub arguments: Json,
        pub status: String,
        pub park_lease_token: Uuid,
        pub park_attempt_count: i32,
        pub park_claim_count: i32,
        pub batch_ordinal: i16,
        pub executor_lease_token: Option<Uuid>,
        pub executor_lease_expires_at: Option<DateTimeUtc>,
        pub retry_at: Option<DateTimeUtc>,
        pub resolution_lease_token: Option<Uuid>,
        pub result: Option<String>,
        pub error_code: Option<String>,
        pub error_detail: Option<String>,
        pub created_at: DateTimeUtc,
        pub resolved_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_agent_run_wait_set {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_agent_run_wait_set")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub parent_run_id: Uuid,
        pub turn_id: Uuid,
        pub chat_id: Uuid,
        pub condition: String,
        pub park_lease_token: Uuid,
        pub expected_steer_revision: i64,
        pub attempt_count: i32,
        pub claim_count: i32,
        pub model_steps: i32,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub event_ordinal: i32,
        pub event_seq: Option<i64>,
        pub status: String,
        pub parked_at: DateTimeUtc,
        pub closed_at: Option<DateTimeUtc>,
        pub resume_token: Option<Uuid>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_agent_run_wait_member {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_agent_run_wait_member")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub wait_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub position: i16,
        pub child_run_id: Uuid,
        pub parent_run_id: Uuid,
        pub origin_turn_id: Uuid,
        pub chat_id: Uuid,
        pub open: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_run {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_run")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub agent_run_id: Uuid,
        pub agent_run_depth: i16,
        pub input_message_id: Uuid,
        pub output_message_id: Option<Uuid>,
        pub model: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub invoked_skills: Json,
        pub voice_input_used: bool,
        pub status: String,
        pub attempt_count: i32,
        pub max_attempts: i32,
        pub claim_count: i32,
        pub model_steps: i32,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub available_at: DateTimeUtc,
        pub lease_token: Option<Uuid>,
        pub lease_expires_at: Option<DateTimeUtc>,
        pub started_at: Option<DateTimeUtc>,
        pub finished_at: Option<DateTimeUtc>,
        pub last_error_code: Option<String>,
        pub last_error_detail: Option<String>,
        pub steer_revision: i64,
        pub last_steer_applied_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_claim {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_claim")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub token: Uuid,
        pub turn_id: Uuid,
        pub attempt_count: i32,
        pub claim_count: i32,
        pub claimed_at: DateTimeUtc,
        pub lease_expires_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_client_wait {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_client_wait")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub call_id: Uuid,
        pub turn_id: Uuid,
        pub chat_id: Uuid,
        pub park_lease_token: Uuid,
        pub attempt_count: i32,
        pub claim_count: i32,
        pub model_steps: i32,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub status: String,
        pub parked_at: DateTimeUtc,
        pub closed_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod plan_request {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "plan_request")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub call_id: Uuid,
        pub turn_id: Uuid,
        pub chat_id: Uuid,
        pub status: String,
        pub event_seq: i64,
        pub title: String,
        pub plan: String,
        pub feedback: Option<String>,
        pub proposed_at: DateTimeUtc,
        pub resolved_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod task_plan {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "task_plan")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub call_id: Uuid,
        pub steps: String,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod agent_run_task_plan {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run_task_plan")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub agent_run_id: Uuid,
        pub call_id: Uuid,
        pub steps: String,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod user_question_request {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "user_question_request")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub call_id: Uuid,
        pub turn_id: Uuid,
        pub chat_id: Uuid,
        pub status: String,
        pub event_seq: i64,
        pub asked_at: DateTimeUtc,
        pub resolved_at: Option<DateTimeUtc>,
        pub additional_user_context: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod user_question {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "user_question")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub call_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub question_id: String,
        pub position: i32,
        pub header: String,
        pub prompt: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub options: Json,
        pub question_type: String,
        pub allow_free_form: bool,
        #[sea_orm(column_type = "JsonBinary")]
        pub answer_selected_option_ids: Option<Json>,
        pub answer_custom_answer: Option<String>,
        pub response_recorded_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod message_identity {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "message_identity")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub owner: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[allow(dead_code)]
pub mod turn_steer {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_steer")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub turn_id: Uuid,
        pub chat_id: Uuid,
        pub content: String,
        pub invoked_skills: Json,
        pub voice_input_used: bool,
        pub interrupt: bool,
        pub status: String,
        pub applied_lease_token: Option<Uuid>,
        pub message_id: Option<Uuid>,
        pub preceding_assistant_message_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub resolved_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_failure {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_failure")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub lease_token: Uuid,
        pub turn_id: Uuid,
        pub attempt_count: i32,
        pub model_steps: i32,
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_input_tokens: i64,
        pub cache_creation_input_tokens: i64,
        pub requested_retry_at: Option<DateTimeUtc>,
        pub error_code: String,
        pub error_detail: Option<String>,
        pub resolved_at: DateTimeUtc,
        pub result_status: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod tool_call {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "tool_call")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub provider_id: String,
        pub history_order: i64,
        pub name: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub arguments: Json,
        /// The exact bytes the provider streamed when `arguments` would not
        /// parse and had to be coerced; `NULL` for well-formed calls.
        pub raw_arguments: Option<String>,
        pub execution: String,
        pub status: String,
        pub result: Option<String>,
        /// The closed renderer projection of what this call produced, as it
        /// crossed the boundary live. `None` for a call that projected none.
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub result_preview: Option<Json>,
        /// Provider-native blocks for same-route replay of a provider-executed
        /// call. `None` for host tools and for providers with nothing opaque.
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub provider_replay: Option<Json>,
        pub error_code: Option<String>,
        pub error_detail: Option<String>,
        pub approval_status: Option<String>,
        pub approval_class: Option<String>,
        pub approval_kind: Option<String>,
        pub approval_reason: Option<String>,
        pub approval_requested_at: Option<DateTimeUtc>,
        pub approval_decided_at: Option<DateTimeUtc>,
        pub approval_event_seq: Option<i64>,
        pub approval_grant_source_call_id: Option<Uuid>,
        pub auto_judge_status: Option<String>,
        pub client_executor_id: Option<Uuid>,
        pub client_lease_token: Option<Uuid>,
        pub client_lease_expires_at: Option<DateTimeUtc>,
        pub turn_lease_token: Option<Uuid>,
        pub resolution_turn_lease_token: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub resolved_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod standing_tool_grant {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "standing_tool_grant")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub source_call_id: Uuid,
        pub chat_id: Option<Uuid>,
        pub project_id: Option<Uuid>,
        pub tool_name: String,
        pub approval_kind: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub scope: Json,
        pub granted_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod assistant_citation {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "assistant_citation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub message_id: Uuid,
        pub ordinal: i32,
        pub document_id: Uuid,
        #[sea_orm(column_type = "JsonBinary")]
        pub locator: Json,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {
        #[sea_orm(
            belongs_to = "super::message::Entity",
            from = "Column::MessageId",
            to = "super::message::Column::Id"
        )]
        Message,
    }

    impl Related<super::message::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Message.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod setting {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "setting")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub key: String,
        // Matches the migration's `.json_binary()` (JSONB on Postgres).
        #[sea_orm(column_type = "JsonBinary")]
        pub value_json: Json,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod output {
    use sea_orm::entity::prelude::*;

    // `current_revision_id` and `revision_count` are maintained in the same
    // transaction that inserts a revision. They are deliberately not a foreign
    // key: `output_revision` already references `output`, and closing the cycle
    // would order the two inserts against each other for no added safety.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "output")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub chat_id: Uuid,
        pub filename: String,
        pub media_type: String,
        pub current_revision_id: Uuid,
        pub revision_count: i32,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
        pub deleted_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod output_revision {
    use sea_orm::entity::prelude::*;

    // Rows are insert-only. The bytes live in conversation-private scratch
    // under the revision id, so a revision row and its content are both
    // write-once and can never disagree.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "output_revision")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub output_id: Uuid,
        pub ordinal: i32,
        pub byte_len: i64,
        pub sha256: Vec<u8>,
        pub turn_id: Option<Uuid>,
        pub producing_run_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        Output,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Self::Output => Entity::belongs_to(super::output::Entity)
                    .from(Column::OutputId)
                    .to(super::output::Column::Id)
                    .into(),
            }
        }
    }

    impl Related<super::output::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::Output.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod connected_app {
    use sea_orm::entity::prelude::*;

    // One row per outside integration the profile can reach. `kind` is the
    // closed `ConnectedAppKind` vocabulary as text; `definition_json` is the
    // kind-specific definition, validated and bounded before it gets here.
    // `(kind, name)` is unique: for `mcp_server` rows the name is the mount
    // namespace, and two records may not claim one namespace.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "connected_app")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub name: String,
        pub kind: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub definition_json: Json,
        // Position within the record's kind. The settings surfaces edit an
        // ordered list, and creation timestamps tie within one save, so the
        // order is stored rather than inferred.
        pub position: i32,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod app {
    use sea_orm::entity::prelude::*;

    // The profile-scoped analog of `output`, with the one deliberate
    // difference that there is no chat foreign key: the profile owns the app
    // and it outlives every conversation that touched it. As with outputs,
    // `current_revision_id` and `revision_count` are maintained in the same
    // transaction that inserts a revision and are deliberately not a foreign
    // key (the revision table already references `app`; closing the cycle
    // would order the two inserts against each other for no added safety).
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "app")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub name: String,
        pub current_revision_id: Uuid,
        pub revision_count: i32,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
        pub deleted_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod app_revision {
    use sea_orm::entity::prelude::*;

    // Rows are insert-only. The bundle bytes live under the profile data
    // directory keyed by (app id, revision id), so a revision row and its
    // content are both write-once and can never disagree. `chat_id` is
    // provenance only — deliberately no foreign key, so the revision survives
    // deletion of the conversation that authored it.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "app_revision")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub app_id: Uuid,
        pub ordinal: i32,
        // Matches the migration's `.json_binary()` (JSONB on Postgres).
        #[sea_orm(column_type = "JsonBinary")]
        pub manifest_json: Json,
        pub byte_len: i64,
        pub sha256: Vec<u8>,
        pub turn_id: Option<Uuid>,
        pub producing_run_id: Option<Uuid>,
        pub chat_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        App,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Self::App => Entity::belongs_to(super::app::Entity)
                    .from(Column::AppId)
                    .to(super::app::Column::Id)
                    .into(),
            }
        }
    }

    impl Related<super::app::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::App.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod app_grant {
    use sea_orm::entity::prelude::*;

    // At most one grant per app — the app id is the primary key — replaced
    // wholesale by a fresh consent and deleted by revocation. The bindings
    // column carries the granted `(server, tools[])` set with each server's
    // definition fingerprint, in the serde shape of
    // `Vec<crate::local_app::AppGrantBinding>`.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "app_grant")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub app_id: Uuid,
        // Matches the migration's `.json_binary()` (JSONB on Postgres).
        #[sea_orm(column_type = "JsonBinary")]
        pub bindings_json: Json,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        App,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Self::App => Entity::belongs_to(super::app::Entity)
                    .from(Column::AppId)
                    .to(super::app::Column::Id)
                    .into(),
            }
        }
    }

    impl Related<super::app::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::App.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod app_gateway_draft {
    use sea_orm::entity::prelude::*;

    // One row per (local app, gateway deployment): the shared app the local
    // app is registered as there, the gateway revision that registration
    // currently serves, and the local revision it was projected from. The
    // deployment is part of the key, so a re-paired profile reads no
    // registration rather than a stale one.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "app_gateway_draft")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub app_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub gateway_base_url: String,
        pub shared_app_id: String,
        pub gateway_revision_id: String,
        pub synced_revision_id: Uuid,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        App,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Self::App => Entity::belongs_to(super::app::Entity)
                    .from(Column::AppId)
                    .to(super::app::Column::Id)
                    .into(),
            }
        }
    }

    impl Related<super::app::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::App.def()
        }
    }

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod event {
    use sea_orm::entity::prelude::*;

    // Composite primary key `(chat_id, seq)`: `seq` is monotonic *per
    // chat*, and the pair both enforces uniqueness and indexes the
    // "this chat's events after a cursor" replay query.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "event")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub chat_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub seq: i64,
        pub turn_id: Option<Uuid>,
        pub lease_token: Option<Uuid>,
        pub attempt_event_ordinal: Option<i32>,
        pub scan_token: Option<Uuid>,
        pub terminal: bool,
        #[sea_orm(column_type = "JsonBinary")]
        pub payload: Json,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_repo {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_repo")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub root_path: String,
        pub display_name: String,
        pub default_base_ref: String,
        pub branch_prefix: String,
        pub setup_script: Option<String>,
        pub archive_script: Option<String>,
        #[sea_orm(column_type = "JsonBinary")]
        pub quick_actions: Json,
        pub created_at: DateTimeUtc,
        pub removed_at: Option<DateTimeUtc>,
        pub cloned_from: Option<String>,
        pub origin_host: Option<String>,
        pub origin_owner: Option<String>,
        pub origin_name: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_pull_request {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_pull_request")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub host: String,
        pub repo_owner: String,
        pub repo_name: String,
        pub number: i64,
        pub url: String,
        pub title: String,
        pub state: String,
        pub draft: bool,
        pub author: Option<String>,
        pub head_branch: String,
        pub base_branch: String,
        pub head_sha: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
        pub merged_at: Option<DateTimeUtc>,
        pub closed_at: Option<DateTimeUtc>,
        pub first_seen_at: DateTimeUtc,
        pub last_seen_at: DateTimeUtc,
        pub checks_summary: Option<String>,
        pub checks: Option<String>,
        pub review_decision: Option<String>,
        pub mergeable: Option<String>,
        pub merge_state_status: Option<String>,
        pub auto_merge_enabled: Option<bool>,
        pub in_merge_queue: Option<bool>,
        pub live_observed_at: Option<DateTimeUtc>,
        pub pull_etag: Option<String>,
        pub checks_etag: Option<String>,
        pub reviews_etag: Option<String>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_pull_request_attribution {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_pull_request_attribution")]
    pub struct Model {
        pub owner: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub pull_request_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub workspace_id: Uuid,
        pub relation: String,
        pub discovered_via: String,
        pub session_id: Option<Uuid>,
        pub parent_call_id: Option<String>,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_workspace {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_workspace")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub repo_id: Uuid,
        pub title: String,
        pub worktree_path: String,
        pub branch_name: String,
        pub base_ref: String,
        pub status: String,
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub pr: Option<Json>,
        pub created_at: DateTimeUtc,
        pub archived_at: Option<DateTimeUtc>,
        pub released_at: Option<DateTimeUtc>,
        pub released_tip: Option<String>,
        pub bundle_bytes: Option<i64>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_session {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_session")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub workspace_id: Uuid,
        pub kind: String,
        pub harness_kind: String,
        pub harness_version: Option<String>,
        pub harness_resume_ref: Option<String>,
        pub permission_mode: String,
        pub permission_mode_revision: i64,
        pub permission_mode_intent: Option<String>,
        pub permission_mode_intent_revision: Option<i64>,
        pub permission_mode_intent_epoch: Option<i64>,
        pub permission_mode_intent_lifecycle: Option<String>,
        pub model: Option<String>,
        pub reasoning_effort: Option<String>,
        pub fast_mode: bool,
        pub lifecycle: String,
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub fence_reason: Option<Json>,
        pub child_pid: Option<i64>,
        pub child_process_identity: Option<String>,
        pub spawn_epoch: i64,
        #[sea_orm(column_type = "JsonBinary")]
        pub attention_state: Json,
        pub attention_source: String,
        pub unrecognized_event_count: i64,
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub subagents: Option<Json>,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_turn {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_turn")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub session_id: Uuid,
        pub ordinal: i64,
        pub status: String,
        pub model: Option<String>,
        pub fast_mode: bool,
        #[sea_orm(column_type = "Text")]
        pub user_input: String,
        pub user_input_blob_id: Option<Uuid>,
        pub checkpoint_ref: Option<String>,
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub diffstat: Option<Json>,
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub usage: Option<Json>,
        pub narrative: Option<String>,
        pub started_at: DateTimeUtc,
        pub ended_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_turn_attachment {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_turn_attachment")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub turn_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub ordinal: i32,
        pub owner: String,
        pub blob_id: Uuid,
        pub media_type: String,
        pub width: i32,
        pub height: i32,
        pub byte_len: i64,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_queued_turn {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_queued_turn")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub session_id: Uuid,
        #[sea_orm(column_type = "Text")]
        pub message: String,
        pub attachments_json: String,
        pub position: i32,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_session_incarnation {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_session_incarnation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub session_id: Uuid,
        pub incarnation: i32,
        pub state: String,
        pub sandbox_id: Option<String>,
        pub starting_turn: i32,
        pub stop_reason: Option<String>,
        pub spend_microusd: Option<i64>,
        pub terminal_events_journaled: bool,
        pub events_cursor: i64,
        pub task_output: Option<String>,
        pub last_wip_ref: Option<String>,
        pub created_at: DateTimeUtc,
        pub activated_at: Option<DateTimeUtc>,
        pub stopped_at: Option<DateTimeUtc>,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_external_binding {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_external_binding")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub channel_kind: String,
        pub external_key: String,
        pub grant_id: Uuid,
        pub session_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_session_image {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_session_image")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub session_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub blob_id: Uuid,
        pub owner: String,
        pub media_type: String,
        pub width: i32,
        pub height: i32,
        pub byte_len: i64,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_event {
    use sea_orm::entity::prelude::*;

    // Composite primary key `(session_id, seq)`: `seq` is monotonic *per
    // session*, and the pair both enforces uniqueness and indexes the
    // "this session's events after a cursor" replay query.
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_event")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub session_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub seq: i64,
        pub owner: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub event: Json,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_approval {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_approval")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub session_id: Uuid,
        pub turn_id: Uuid,
        #[sea_orm(column_type = "JsonBinary")]
        pub kind: Json,
        #[sea_orm(column_type = "JsonBinary")]
        pub harness_raw: Json,
        pub native_call_id: Option<String>,
        pub server_capability: Option<String>,
        pub request_sha256: Option<String>,
        pub worker_epoch: Option<i64>,
        pub decision_claim: Option<Uuid>,
        pub claimed_at: Option<DateTimeUtc>,
        pub state: String,
        pub feedback: Option<String>,
        pub requested_at: DateTimeUtc,
        pub decided_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_watch {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_watch")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub workspace_id: Uuid,
        pub session_id: Uuid,
        pub pr_number: i64,
        pub state: String,
        pub detail: Option<String>,
        pub last_fix_head: Option<String>,
        pub cycles: i64,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_trigger {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_trigger")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub repo_id: Uuid,
        pub condition: String,
        pub action: String,
        pub enabled: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_trigger_fire {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_trigger_fire")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub trigger_id: Uuid,
        pub owner: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub workspace_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub pr_number: i64,
        #[sea_orm(primary_key, auto_increment = false)]
        pub head_sha: String,
        pub fired_at: DateTimeUtc,
        pub delivery_id: Uuid,
        pub delivery_condition: Option<String>,
        pub delivery_action: Option<String>,
        pub delivery_message: Option<String>,
        pub state: String,
        pub attempt_count: i64,
        pub lease_token: Option<Uuid>,
        pub lease_expires_at: Option<DateTimeUtc>,
        pub next_attempt_at: Option<DateTimeUtc>,
        pub last_error: Option<String>,
        pub delivered_at: Option<DateTimeUtc>,
        pub cancelled_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod notification {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "notification")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub kind: String,
        #[sea_orm(column_type = "Text")]
        pub title: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub context: Json,
        pub dedupe_key: String,
        pub created_at: DateTimeUtc,
        pub read_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_trigger_delivery_receipt {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_trigger_delivery_receipt")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub delivery_id: Uuid,
        pub owner: String,
        pub sink: String,
        pub session_id: Uuid,
        pub turn_id: Option<Uuid>,
        pub acceptance_token: Uuid,
        pub accepted_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_workflow_run {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_workflow_run")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
        pub host: String,
        pub repo_owner: String,
        pub repo_name: String,
        pub github_id: i64,
        pub run_attempt: Option<i64>,
        pub name: String,
        pub url: String,
        pub status: String,
        pub conclusion: Option<String>,
        pub workflow: Option<String>,
        pub branch: Option<String>,
        pub sha: Option<String>,
        pub event: Option<String>,
        pub actor: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
        pub first_seen_at: DateTimeUtc,
        pub last_seen_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod code_workflow_run_fetch {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_workflow_run_fetch")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub owner: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub host: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub repo_owner: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub repo_name: String,
        pub list_etag: Option<String>,
        pub observed_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}
