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
        pub workspace_dir: String,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

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
        pub workspace_dir: String,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

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
        pub created_at: DateTimeUtc,
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
        pub name: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub arguments: Json,
        pub execution: String,
        pub status: String,
        pub result: Option<String>,
        pub error_code: Option<String>,
        pub error_detail: Option<String>,
        pub client_executor_id: Option<Uuid>,
        pub client_lease_token: Option<Uuid>,
        pub client_lease_expires_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub resolved_at: Option<DateTimeUtc>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

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
