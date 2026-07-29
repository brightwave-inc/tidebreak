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

pub mod document_generation {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "document_generation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub document_id: Uuid,
        pub content_revision: i64,
        pub revision_token: Uuid,
        pub tombstone: bool,
        pub retirement_pending: bool,
        pub retirement_content_revision: Option<i64>,
        pub retirement_revision_token: Option<Uuid>,
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
        pub source_uri: Option<String>,
        pub media_type: String,
        pub title: Option<String>,
        pub source_blob_id: Option<Uuid>,
        pub source_sha256: Option<Vec<u8>>,
        pub source_byte_len: Option<i64>,
        #[sea_orm(column_type = "Text")]
        pub canonical_text: String,
        pub canonical_fingerprint: Option<String>,
        pub source_regions: Json,
        pub content_revision: i64,
        pub revision_token: Uuid,
        pub processing_status: String,
        pub indexed_revision: Option<i64>,
        pub index_fingerprint: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
        pub indexed_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod document_job {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "document_job")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub document_id: Uuid,
        pub content_revision: i64,
        pub revision_token: Uuid,
        pub kind: String,
        pub status: String,
        pub pipeline_fingerprint: String,
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
        pub attachment_revision: i64,
        pub created_at: DateTimeUtc,
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

pub mod operation_log {
    use sea_orm::entity::prelude::*;

    /// One durable reverse-RPC operation-log entry, keyed by `(run_id,
    /// operation_id)`. Bodies are opaque blobs; the protocol tier owns their
    /// typed meaning. See `openwave-sandbox-protocol::oplog` and issue #858.
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
        pub available_at: DateTimeUtc,
        pub deadline_at: Option<DateTimeUtc>,
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

pub mod sandbox_agent_admission {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "sandbox_agent_admission")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub child_run_id: Uuid,
        pub parent_run_id: Uuid,
        pub origin_turn_id: Uuid,
        pub chat_id: Uuid,
        pub spawn_call_id: Uuid,
        pub delegated_root_id: Option<Uuid>,
        pub delegated_relative_path: Option<String>,
        pub admitted_at: DateTimeUtc,
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

pub mod agent_run_claim_lock {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "agent_run_claim_lock")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i32,
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
        pub submitted_at: DateTimeUtc,
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
        pub executor_lease_token: Option<Uuid>,
        pub executor_lease_expires_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub resolved_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod sandbox_tool_call_receipt {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "sandbox_tool_call_receipt")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub call_id: Uuid,
        pub executor_lease_token: Uuid,
        pub status: String,
        pub result: String,
        pub error_code: Option<String>,
        pub error_detail: Option<String>,
        pub resolved_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

pub mod turn_agent_run_wait {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_agent_run_wait")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub child_run_id: Uuid,
        pub parent_run_id: Uuid,
        pub turn_id: Uuid,
        pub chat_id: Uuid,
        pub park_lease_token: Uuid,
        pub atomic_admission: bool,
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
        pub provider_id: String,
        pub history_order: i64,
        #[sea_orm(column_type = "JsonBinary")]
        pub arguments: Json,
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

pub mod turn_agent_run_wait_lock {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_agent_run_wait_lock")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i32,
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

pub mod turn_claim_lock {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "turn_claim_lock")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i32,
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
        pub allow_free_form: bool,
        pub answer_option_id: Option<String>,
        pub answer_free_form: Option<String>,
        pub answered_at: Option<DateTimeUtc>,
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
        pub execution: String,
        pub status: String,
        pub result: Option<String>,
        /// The closed renderer projection of what this call produced, as it
        /// crossed the boundary live. `None` for a call that projected none.
        #[sea_orm(column_type = "JsonBinary", nullable)]
        pub result_preview: Option<Json>,
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
        pub chat_id: Uuid,
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

pub mod retrieval_evidence {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "retrieval_evidence")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub call_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub rank: i32,
        pub source_token: Uuid,
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub document_id: Uuid,
        pub content_revision: i64,
        pub revision_token: Uuid,
        pub chunk_id: Uuid,
        pub span_start: i64,
        pub span_end: i64,
        pub snippet: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub heading_path: Json,
        #[sea_orm(column_type = "JsonBinary")]
        pub source_regions: Json,
        pub source_kind: String,
        pub source_uri: Option<String>,
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
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub evidence_call_id: Uuid,
        pub evidence_rank: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        RetrievalEvidence,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Self::RetrievalEvidence => Entity::belongs_to(super::retrieval_evidence::Entity)
                    .from((Column::EvidenceCallId, Column::EvidenceRank))
                    .to((
                        super::retrieval_evidence::Column::CallId,
                        super::retrieval_evidence::Column::Rank,
                    ))
                    .into(),
            }
        }
    }

    impl Related<super::retrieval_evidence::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::RetrievalEvidence.def()
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

pub mod output_revision_citation {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "output_revision_citation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub output_revision_id: Uuid,
        pub ordinal: i32,
        pub chat_id: Uuid,
        pub turn_id: Uuid,
        pub evidence_call_id: Uuid,
        pub evidence_rank: i32,
    }

    #[derive(Copy, Clone, Debug, EnumIter)]
    pub enum Relation {
        OutputRevision,
        RetrievalEvidence,
    }

    impl RelationTrait for Relation {
        fn def(&self) -> RelationDef {
            match self {
                Self::OutputRevision => Entity::belongs_to(super::output_revision::Entity)
                    .from(Column::OutputRevisionId)
                    .to(super::output_revision::Column::Id)
                    .into(),
                Self::RetrievalEvidence => Entity::belongs_to(super::retrieval_evidence::Entity)
                    .from((Column::EvidenceCallId, Column::EvidenceRank))
                    .to((
                        super::retrieval_evidence::Column::CallId,
                        super::retrieval_evidence::Column::Rank,
                    ))
                    .into(),
            }
        }
    }

    impl Related<super::retrieval_evidence::Entity> for Entity {
        fn to() -> RelationDef {
            Relation::RetrievalEvidence.def()
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
