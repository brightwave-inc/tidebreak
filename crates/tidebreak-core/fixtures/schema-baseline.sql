CREATE TABLE "project" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "title" text,
    "attachment_revision" integer NOT NULL DEFAULT 0,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "owner" text NOT NULL DEFAULT 'local',
    CHECK ("attachment_revision" >= 0),
    CHECK ("attachment_revision" <= 9007199254740991)
);


CREATE TABLE "setting" (
    "key" text NOT NULL PRIMARY KEY,
    "value_json" jsonb_text NOT NULL
);


CREATE TABLE "chat" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "project_id" uuid_text,
    "title" text,
    "attachment_revision" integer NOT NULL DEFAULT 0,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "model" text,
    "reasoning_effort" text,
    "permission_mode" text,
    "network_policy" text NOT NULL DEFAULT '{"mode":"off"}',
    "owner" text NOT NULL DEFAULT 'local',
    FOREIGN KEY ("project_id") REFERENCES "project" ("id") ON DELETE RESTRICT,
    CHECK ("attachment_revision" >= 0),
    CHECK ("attachment_revision" <= 9007199254740991)
);


CREATE TABLE "project_root_attachment" (
    "project_id" uuid_text NOT NULL,
    "root_id" uuid_text NOT NULL,
    "position" integer NOT NULL, PRIMARY KEY ("project_id",
    "root_id"),
    FOREIGN KEY ("project_id") REFERENCES "project" ("id") ON DELETE CASCADE,
    CHECK ("root_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("position" >= 0),
    CHECK ("position" < 32)
);

CREATE UNIQUE INDEX "idx_project_root_attachment_position" ON "project_root_attachment" ("project_id", "position");

CREATE TABLE "chat_root_attachment" (
    "chat_id" uuid_text NOT NULL,
    "root_id" uuid_text NOT NULL,
    "position" integer NOT NULL,
    "origin" varchar(24) NOT NULL, PRIMARY KEY ("chat_id",
    "root_id"),
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    CHECK ("root_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("position" >= 0),
    CHECK ("position" < 32),
    CHECK ("origin" IN ('project_default', 'conversation'))
);

CREATE UNIQUE INDEX "idx_chat_root_attachment_position" ON "chat_root_attachment" ("chat_id", "position");

CREATE TABLE "root_attachment_change" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "subject_kind" varchar(24) NOT NULL,
    "subject_id" uuid_text NOT NULL,
    "executor_id" uuid_text NOT NULL,
    "root_id" uuid_text NOT NULL,
    "action" varchar(16) NOT NULL,
    "origin" varchar(24),
    "projection_position" integer,
    "projection_existed_before" boolean NOT NULL,
    "expected_revision" integer NOT NULL,
    "before_revision" integer NOT NULL,
    "intent_revision" integer NOT NULL,
    "phase" varchar(24) NOT NULL,
    "result_revision" integer,
    "projection_changed" boolean,
    "broker_changed" boolean,
    "broker_currently_attached" boolean,
    "failure_code" varchar(64),
    "failure_message" varchar(256),
    "failure_retryable" boolean,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "finished_at" timestamp_with_timezone_text,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    CHECK ("id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("chat_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("subject_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("executor_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("root_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("subject_kind" IN ('project', 'conversation')),
    CHECK ("action" IN ('attach', 'detach')),
    CHECK ("origin" IS NULL OR "origin" IN ('project_default', 'conversation')),
    CHECK ((("projection_existed_before" = TRUE OR "action" = 'attach') AND "origin" IS NOT NULL AND "projection_position" IS NOT NULL) OR ("projection_existed_before" = FALSE AND "action" = 'detach' AND "origin" IS NULL AND "projection_position" IS NULL)),
    CHECK ("projection_position" IS NULL OR ("projection_position" BETWEEN 0 AND 31)),
    CHECK ("action" <> 'attach' OR "projection_existed_before" = TRUE OR "origin" = 'conversation'),
    CHECK ("expected_revision" BETWEEN 0 AND 9007199254740991),
    CHECK ("before_revision" BETWEEN 0 AND 9007199254740991),
    CHECK ("intent_revision" BETWEEN 0 AND 9007199254740991),
    CHECK ("result_revision" IS NULL OR ("result_revision" BETWEEN 0 AND 9007199254740991)),
    CHECK ("expected_revision" = "before_revision"),
    CHECK ("intent_revision" = "before_revision" OR "intent_revision" = "before_revision" + 1),
    CHECK ("action" <> 'attach' OR "projection_existed_before" = TRUE OR "intent_revision" = "before_revision" + 1),
    CHECK (("action" = 'attach' AND "projection_existed_before" = FALSE) OR "intent_revision" = "before_revision"),
    CHECK ("action" <> 'attach' OR "projection_existed_before" = TRUE OR "before_revision" <= 9007199254740989),
    CHECK ("action" <> 'detach' OR "projection_existed_before" = FALSE OR "before_revision" <= 9007199254740990),
    CHECK ("failure_code" IS NULL OR (LENGTH("failure_code") BETWEEN 1 AND 64)),
    CHECK ("failure_message" IS NULL OR (LENGTH("failure_message") BETWEEN 1 AND 256)),
    CHECK (("phase" = 'awaiting_broker' AND "result_revision" IS NULL AND "projection_changed" IS NULL AND "broker_changed" IS NULL AND "broker_currently_attached" IS NULL AND "failure_code" IS NULL AND "failure_message" IS NULL AND "failure_retryable" IS NULL AND "finished_at" IS NULL) OR ("phase" = 'completed' AND "result_revision" IS NOT NULL AND "projection_changed" IS NOT NULL AND "broker_changed" IS NOT NULL AND "broker_currently_attached" IS NOT NULL AND "failure_code" IS NULL AND "failure_message" IS NULL AND "failure_retryable" IS NULL AND "finished_at" IS NOT NULL) OR ("phase" = 'failed' AND "result_revision" IS NOT NULL AND "projection_changed" IS NOT NULL AND "failure_code" IS NOT NULL AND "failure_message" IS NOT NULL AND "failure_retryable" IS NOT NULL AND "finished_at" IS NOT NULL)),
    CHECK ("finished_at" IS NULL OR "finished_at" >= "created_at")
);

CREATE UNIQUE INDEX "idx_root_attachment_change_one_awaiting" ON "root_attachment_change" ("chat_id") WHERE "phase" = 'awaiting_broker';
CREATE INDEX "idx_root_attachment_change_pending_scan" ON "root_attachment_change" ("executor_id", "phase", "created_at", "id");
CREATE INDEX "idx_root_attachment_change_history" ON "root_attachment_change" ("chat_id", "created_at", "id");

CREATE TABLE "message_identity" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "turn_id" uuid_text NOT NULL,
    "owner" varchar(16) NOT NULL,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id"),
    CHECK ("owner" IN ('message', 'turn_steer'))
);


CREATE TABLE "message" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "turn_id" uuid_text NOT NULL,
    "seq" integer NOT NULL,
    "role" text NOT NULL,
    "content" text NOT NULL,
    "llm_content" text,
    "reasoning" jsonb_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "turn_lease_token" uuid_text,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id")
);

CREATE UNIQUE INDEX "idx_message_chat" ON "message" ("chat_id", "seq");
CREATE UNIQUE INDEX "idx_message_turn_identity" ON "message" ("id", "chat_id", "turn_id");

CREATE TABLE "blob_retirement" (
    "blob_id" uuid_text NOT NULL PRIMARY KEY,
    "status" varchar(32) NOT NULL DEFAULT 'queued',
    "attempt_count" integer NOT NULL DEFAULT 0,
    "max_attempts" integer NOT NULL DEFAULT 5,
    "available_at" timestamp_with_timezone_text NOT NULL,
    "lease_token" uuid_text,
    "lease_expires_at" timestamp_with_timezone_text,
    "started_at" timestamp_with_timezone_text,
    "finished_at" timestamp_with_timezone_text,
    "last_error_code" varchar(128),
    "last_error_detail" varchar(4096),
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    CHECK ("status" IN ('queued', 'running', 'retry_wait', 'succeeded', 'failed', 'cancelled')),
    CHECK ("attempt_count" >= 0 AND "max_attempts" >= 1 AND "attempt_count" <= "max_attempts"),
    CHECK (("status" = 'running' AND "lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL) OR ("status" <> 'running' AND "lease_token" IS NULL AND "lease_expires_at" IS NULL)),
    CHECK (("status" IN ('succeeded', 'failed', 'cancelled') AND "finished_at" IS NOT NULL) OR ("status" IN ('queued', 'running', 'retry_wait') AND "finished_at" IS NULL)),
    CHECK (("status" = 'queued' AND "attempt_count" = 0 AND "started_at" IS NULL) OR ("status" = 'running' AND "attempt_count" >= 1 AND "started_at" IS NOT NULL) OR ("status" = 'retry_wait' AND "attempt_count" >= 1 AND "attempt_count" < "max_attempts" AND "started_at" IS NOT NULL) OR ("status" IN ('succeeded', 'failed') AND "attempt_count" >= 1 AND "started_at" IS NOT NULL) OR ("status" = 'cancelled' AND (("attempt_count" = 0 AND "started_at" IS NULL) OR ("attempt_count" >= 1 AND "started_at" IS NOT NULL)))),
    CHECK ("last_error_code" IS NULL OR (LENGTH("last_error_code") BETWEEN 1 AND 128)),
    CHECK ("last_error_detail" IS NULL OR (LENGTH("last_error_detail") BETWEEN 1 AND 4096))
);

CREATE INDEX "idx_blob_retirement_due" ON "blob_retirement" ("status", "available_at");
CREATE INDEX "idx_blob_retirement_stale_lease" ON "blob_retirement" ("status", "lease_expires_at");

CREATE TABLE "advisory_lock" (
    "name" varchar(64) NOT NULL PRIMARY KEY
);


CREATE TABLE "turn_claim" (
    "token" uuid_text NOT NULL PRIMARY KEY,
    "turn_id" uuid_text NOT NULL,
    "attempt_count" integer NOT NULL,
    "claim_count" integer NOT NULL,
    "claimed_at" timestamp_with_timezone_text NOT NULL,
    "lease_expires_at" timestamp_with_timezone_text NOT NULL,
    CHECK ("attempt_count" >= 1),
    CHECK ("claim_count" >= "attempt_count"),
    CHECK ("lease_expires_at" > "claimed_at")
);

CREATE UNIQUE INDEX "idx_turn_claim_identity" ON "turn_claim" ("token", "turn_id", "attempt_count", "claim_count");
CREATE UNIQUE INDEX "idx_turn_claim_failure_identity" ON "turn_claim" ("token", "turn_id", "attempt_count");
CREATE UNIQUE INDEX "idx_turn_claim_count" ON "turn_claim" ("turn_id", "claim_count");
CREATE UNIQUE INDEX "idx_turn_claim_turn_token" ON "turn_claim" ("turn_id", "token");

CREATE TABLE "turn_failure" (
    "lease_token" uuid_text NOT NULL PRIMARY KEY,
    "turn_id" uuid_text NOT NULL,
    "attempt_count" integer NOT NULL,
    "model_steps" integer NOT NULL,
    "input_tokens" integer NOT NULL,
    "output_tokens" integer NOT NULL,
    "cache_read_input_tokens" integer NOT NULL,
    "cache_creation_input_tokens" integer NOT NULL,
    "requested_retry_at" timestamp_with_timezone_text,
    "error_code" varchar(128) NOT NULL,
    "error_detail" varchar(4096),
    "resolved_at" timestamp_with_timezone_text NOT NULL,
    "result_status" varchar(32) NOT NULL,
    FOREIGN KEY ("lease_token",
    "turn_id",
    "attempt_count") REFERENCES "turn_claim" ("token",
    "turn_id",
    "attempt_count") ON DELETE RESTRICT,
    CHECK ("attempt_count" >= 1),
    CHECK ("model_steps" >= 0),
    CHECK ("model_steps" <= 2147483647),
    CHECK ("input_tokens" >= 0),
    CHECK ("output_tokens" >= 0),
    CHECK ("cache_read_input_tokens" >= 0),
    CHECK ("cache_creation_input_tokens" >= 0),
    CHECK ("input_tokens" <= 4294967295),
    CHECK ("output_tokens" <= 4294967295),
    CHECK ("cache_read_input_tokens" <= 4294967295),
    CHECK ("cache_creation_input_tokens" <= 4294967295),
    CHECK ("result_status" IN ('retry_wait', 'failed')),
    CHECK ("result_status" <> 'retry_wait' OR "requested_retry_at" IS NOT NULL),
    CHECK ("requested_retry_at" IS NULL OR "requested_retry_at" > "resolved_at"),
    CHECK (LENGTH("error_code") BETWEEN 1 AND 128),
    CHECK ("error_detail" IS NULL OR (LENGTH("error_detail") BETWEEN 1 AND 4096))
);


CREATE TABLE "agent_run_claim" (
    "token" uuid_text NOT NULL PRIMARY KEY,
    "agent_run_id" uuid_text,
    "attempt_count" integer,
    "claim_count" integer,
    "claimed_at" timestamp_with_timezone_text NOT NULL,
    "lease_expires_at" timestamp_with_timezone_text,
    CHECK (("agent_run_id" IS NULL AND "attempt_count" IS NULL AND "claim_count" IS NULL AND "lease_expires_at" IS NULL) OR ("agent_run_id" IS NOT NULL AND "attempt_count" IS NOT NULL AND "attempt_count" >= 1 AND ("claim_count" IS NOT NULL AND "claim_count" >= "attempt_count") AND ("lease_expires_at" IS NOT NULL AND "lease_expires_at" > "claimed_at")))
);

CREATE UNIQUE INDEX "idx_agent_run_claim_identity" ON "agent_run_claim" ("token", "agent_run_id", "attempt_count", "claim_count");
CREATE UNIQUE INDEX "idx_agent_run_claim_count" ON "agent_run_claim" ("agent_run_id", "claim_count");

CREATE TABLE "agent_run" (
    "id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "parent_id" uuid_text,
    "parent_depth" smallint,
    "spawn_call_id" uuid_text,
    "tier" varchar(16) NOT NULL,
    "execution_location" varchar(16) NOT NULL,
    "depth" smallint NOT NULL,
    "status" varchar(32) NOT NULL,
    "input" text,
    "model" varchar(512),
    "attempt_count" integer NOT NULL,
    "max_attempts" integer NOT NULL,
    "claim_count" integer NOT NULL,
    "checkin_grants" integer NOT NULL DEFAULT 0,
    "checkin_watermark" integer NOT NULL DEFAULT 0,
    "model_steps" integer NOT NULL DEFAULT 0,
    "input_tokens" integer NOT NULL DEFAULT 0,
    "output_tokens" integer NOT NULL DEFAULT 0,
    "cache_read_input_tokens" integer NOT NULL DEFAULT 0,
    "cache_creation_input_tokens" integer NOT NULL DEFAULT 0,
    "available_at" timestamp_with_timezone_text NOT NULL,
    "deadline_at" timestamp_with_timezone_text,
    "lease_token" uuid_text,
    "lease_expires_at" timestamp_with_timezone_text,
    "started_at" timestamp_with_timezone_text,
    "finished_at" timestamp_with_timezone_text,
    "last_error_code" varchar(128),
    "last_error_detail" varchar(4096),
    "origin_turn_id" uuid_text,
    "delegated_root_id" uuid_text,
    "delegated_relative_path" text,
    "admitted_at" timestamp_with_timezone_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL, CONSTRAINT "pk_agent_run" PRIMARY KEY ("id",
    "chat_id",
    "depth"),
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("parent_id",
    "chat_id",
    "parent_depth") REFERENCES "agent_run" ("id",
    "chat_id",
    "depth") ON DELETE RESTRICT,
    FOREIGN KEY ("lease_token",
    "id",
    "attempt_count",
    "claim_count") REFERENCES "agent_run_claim" ("token",
    "agent_run_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK (("tier" = 'foreground' AND "depth" = 0 AND "parent_id" IS NULL AND "parent_depth" IS NULL AND "spawn_call_id" IS NULL AND "input" IS NULL AND "attempt_count" = 0 AND "max_attempts" = 0 AND "claim_count" = 0 AND "available_at" = "created_at" AND "deadline_at" IS NULL AND "started_at" IS NULL AND "status" IN ('active', 'completed', 'failed', 'cancelled')) OR ("tier" = 'background' AND "depth" = 1 AND "parent_id" IS NOT NULL AND "parent_depth" = 0 AND "spawn_call_id" IS NOT NULL AND "input" IS NOT NULL AND "max_attempts" >= 1 AND "attempt_count" >= 0 AND "attempt_count" <= "max_attempts" AND "claim_count" >= "attempt_count" AND "claim_count" < 2147483647 AND "available_at" >= "created_at" AND "deadline_at" > "created_at" AND (LENGTH("input") BETWEEN 1 AND 65536) AND "status" IN ('queued', 'running', 'cancelling', 'waiting', 'retry_wait', 'needs_input', 'completed', 'failed', 'cancelled'))),
    CHECK (("admitted_at" IS NULL OR "origin_turn_id" IS NOT NULL) AND (("delegated_root_id" IS NULL AND "delegated_relative_path" IS NULL) OR ("delegated_root_id" IS NOT NULL AND "delegated_relative_path" IS NOT NULL))),
    CHECK (("status" IN ('running', 'cancelling') AND "lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL AND "attempt_count" >= 1 AND "started_at" IS NOT NULL) OR ("status" NOT IN ('running', 'cancelling') AND "lease_token" IS NULL AND "lease_expires_at" IS NULL)),
    CHECK (("status" IN ('completed', 'failed', 'cancelled') AND "finished_at" IS NOT NULL) OR ("status" IN ('active', 'queued', 'running', 'cancelling', 'waiting', 'retry_wait', 'needs_input') AND "finished_at" IS NULL)),
    CHECK (("status" IN ('retry_wait', 'failed') AND "last_error_code" IS NOT NULL) OR ("status" NOT IN ('retry_wait', 'failed') AND "last_error_code" IS NULL AND "last_error_detail" IS NULL)),
    CHECK ("model_steps" BETWEEN 0 AND 2147483647),
    CHECK ("input_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("output_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("cache_read_input_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("cache_creation_input_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("last_error_detail" IS NULL OR "last_error_code" IS NOT NULL),
    CHECK ("updated_at" >= "created_at"),
    CHECK ("execution_location" IN ('in_process', 'container'))
);

CREATE UNIQUE INDEX "idx_agent_run_id" ON "agent_run" ("id");
CREATE UNIQUE INDEX "idx_agent_run_id_parent_chat" ON "agent_run" ("id", "parent_id", "chat_id");
CREATE UNIQUE INDEX "idx_agent_run_admission_identity" ON "agent_run" ("id", "parent_id", "chat_id", "spawn_call_id");
CREATE UNIQUE INDEX "idx_agent_run_wait_owner" ON "agent_run" ("id", "origin_turn_id", "parent_id", "chat_id");
CREATE INDEX "idx_agent_run_admitted_outstanding" ON "agent_run" ("origin_turn_id", "admitted_at", "id");
CREATE UNIQUE INDEX "idx_agent_run_spawn_call" ON "agent_run" ("spawn_call_id");
CREATE UNIQUE INDEX "idx_agent_run_one_foreground" ON "agent_run" ("chat_id") WHERE "tier" = 'foreground';
CREATE INDEX "idx_agent_run_parent" ON "agent_run" ("parent_id", "created_at");
CREATE INDEX "idx_agent_run_chat_history" ON "agent_run" ("chat_id", "created_at", "id");
CREATE INDEX "idx_agent_run_claimable" ON "agent_run" ("status", "available_at", "created_at", "id");
CREATE INDEX "idx_agent_run_live_by_chat" ON "agent_run" ("status", "chat_id", "lease_expires_at");

CREATE TABLE "agent_run_result" (
    "agent_run_id" uuid_text NOT NULL PRIMARY KEY,
    "lease_token" uuid_text NOT NULL,
    "attempt_count" integer NOT NULL,
    "claim_count" integer NOT NULL,
    "text" text NOT NULL,
    "model_steps" integer NOT NULL DEFAULT 0,
    "input_tokens" integer NOT NULL DEFAULT 0,
    "output_tokens" integer NOT NULL DEFAULT 0,
    "cache_read_input_tokens" integer NOT NULL DEFAULT 0,
    "cache_creation_input_tokens" integer NOT NULL DEFAULT 0,
    "submitted_at" timestamp_with_timezone_text NOT NULL,
    "payload_kind" varchar(32) NOT NULL DEFAULT 'final_text',
    "payload_json" text NOT NULL DEFAULT '{\"text\":\"\"}',
    FOREIGN KEY ("agent_run_id") REFERENCES "agent_run" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("lease_token",
    "agent_run_id",
    "attempt_count",
    "claim_count") REFERENCES "agent_run_claim" ("token",
    "agent_run_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK ("attempt_count" >= 1),
    CHECK ("claim_count" >= "attempt_count"),
    CHECK ("model_steps" BETWEEN 0 AND 2147483647),
    CHECK ("input_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("output_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("cache_read_input_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("cache_creation_input_tokens" BETWEEN 0 AND 4294967295),
    CHECK (LENGTH("text") BETWEEN 1 AND 65536)
);

CREATE UNIQUE INDEX "idx_agent_run_result_identity" ON "agent_run_result" ("agent_run_id", "lease_token", "attempt_count", "claim_count");

CREATE TABLE "agent_run_cancellation" (
    "agent_run_id" uuid_text NOT NULL PRIMARY KEY,
    "lease_token" uuid_text NOT NULL,
    "attempt_count" integer NOT NULL,
    "claim_count" integer NOT NULL,
    "reason" varchar NOT NULL,
    "requested_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("agent_run_id") REFERENCES "agent_run" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("lease_token",
    "agent_run_id",
    "attempt_count",
    "claim_count") REFERENCES "agent_run_claim" ("token",
    "agent_run_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK ("attempt_count" >= 1),
    CHECK ("claim_count" >= "attempt_count"),
    CHECK ("reason" IN ('requested', 'parent_turn_cancelled', 'parent_turn_failed'))
);


CREATE TABLE "agent_run_progress" (
    "agent_run_id" uuid_text NOT NULL,
    "sequence" integer NOT NULL,
    "source_key" varchar(96) NOT NULL,
    "text" text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("agent_run_id",
    "sequence"),
    FOREIGN KEY ("agent_run_id") REFERENCES "agent_run" ("id") ON DELETE CASCADE,
    CHECK ("sequence" >= 1),
    CHECK (LENGTH("text") BETWEEN 1 AND 2048)
);

CREATE UNIQUE INDEX "idx_agent_run_progress_source" ON "agent_run_progress" ("agent_run_id", "source_key");

CREATE TABLE "turn_admission" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "fingerprint" blob(1) NOT NULL,
    "state" varchar(16) NOT NULL,
    "lease_token" uuid_text,
    "lease_expires_at" timestamp_with_timezone_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    CHECK ("state" IN ('pending', 'queued', 'accepted')),
    CHECK (("state" = 'pending' AND "lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL) OR ("state" <> 'pending' AND "lease_token" IS NULL AND "lease_expires_at" IS NULL))
);

CREATE INDEX "ix_turn_admission_pending_expiry" ON "turn_admission" ("state", "lease_expires_at");

CREATE TABLE "turn_run" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "agent_run_id" uuid_text NOT NULL,
    "agent_run_depth" smallint NOT NULL DEFAULT 0,
    "input_message_id" uuid_text NOT NULL,
    "output_message_id" uuid_text,
    "model" varchar(512) NOT NULL,
    "invoked_skills" jsonb_text NOT NULL,
    "voice_input_used" boolean NOT NULL DEFAULT FALSE,
    "status" varchar(32) NOT NULL DEFAULT 'queued',
    "attempt_count" integer NOT NULL DEFAULT 0,
    "max_attempts" integer NOT NULL DEFAULT 5,
    "claim_count" integer NOT NULL DEFAULT 0,
    "model_steps" integer NOT NULL DEFAULT 0,
    "input_tokens" integer NOT NULL DEFAULT 0,
    "output_tokens" integer NOT NULL DEFAULT 0,
    "cache_read_input_tokens" integer NOT NULL DEFAULT 0,
    "cache_creation_input_tokens" integer NOT NULL DEFAULT 0,
    "available_at" timestamp_with_timezone_text NOT NULL,
    "lease_token" uuid_text,
    "lease_expires_at" timestamp_with_timezone_text,
    "started_at" timestamp_with_timezone_text,
    "finished_at" timestamp_with_timezone_text,
    "last_error_code" varchar(128),
    "last_error_detail" varchar(4096),
    "steer_revision" integer NOT NULL DEFAULT 0,
    "last_steer_applied_at" timestamp_with_timezone_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("agent_run_id",
    "chat_id",
    "agent_run_depth") REFERENCES "agent_run" ("id",
    "chat_id",
    "depth") ON DELETE RESTRICT,
    FOREIGN KEY ("input_message_id",
    "chat_id",
    "id") REFERENCES "message" ("id",
    "chat_id",
    "turn_id") ON DELETE RESTRICT,
    FOREIGN KEY ("output_message_id",
    "chat_id",
    "id") REFERENCES "message" ("id",
    "chat_id",
    "turn_id") ON DELETE RESTRICT,
    FOREIGN KEY ("lease_token",
    "id",
    "attempt_count",
    "claim_count") REFERENCES "turn_claim" ("token",
    "turn_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK (LENGTH("model") BETWEEN 1 AND 512),
    CHECK ("agent_run_depth" = 0),
    CHECK ("status" IN ('queued', 'running', 'cancelling', 'waiting_for_client', 'waiting_for_agent_run', 'cancelling_client', 'resuming', 'retry_wait', 'completed', 'failed', 'cancelled')),
    CHECK ("attempt_count" >= 0 AND "max_attempts" >= 1 AND "attempt_count" <= "max_attempts"),
    CHECK ("claim_count" >= "attempt_count"),
    CHECK (("status" IN ('running', 'cancelling') AND "lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL) OR ("status" <> 'running' AND "status" <> 'cancelling' AND "lease_token" IS NULL AND "lease_expires_at" IS NULL)),
    CHECK (("status" = 'completed' AND "output_message_id" IS NOT NULL) OR "status" = 'cancelled' OR ("status" NOT IN ('completed', 'cancelled') AND "output_message_id" IS NULL)),
    CHECK (("status" IN ('completed', 'failed', 'cancelled') AND "finished_at" IS NOT NULL) OR ("status" IN ('queued', 'running', 'cancelling', 'waiting_for_client', 'waiting_for_agent_run', 'cancelling_client', 'resuming', 'retry_wait') AND "finished_at" IS NULL)),
    CHECK (("status" = 'queued' AND "attempt_count" = 0 AND "claim_count" = 0 AND "started_at" IS NULL) OR ("status" IN ('running', 'cancelling') AND "attempt_count" >= 1 AND "started_at" IS NOT NULL) OR ("status" IN ('waiting_for_client', 'waiting_for_agent_run', 'cancelling_client', 'resuming') AND "attempt_count" >= 1 AND "started_at" IS NOT NULL) OR ("status" = 'retry_wait' AND "attempt_count" >= 1 AND "attempt_count" < "max_attempts" AND "started_at" IS NOT NULL) OR ("status" IN ('completed', 'failed') AND "attempt_count" >= 1 AND "started_at" IS NOT NULL) OR ("status" = 'cancelled' AND (("attempt_count" = 0 AND "claim_count" = 0 AND "started_at" IS NULL) OR ("attempt_count" >= 1 AND "started_at" IS NOT NULL)))),
    CHECK (("status" IN ('retry_wait', 'failed') AND "last_error_code" IS NOT NULL) OR ("status" IN ('queued', 'running', 'cancelling', 'waiting_for_client', 'waiting_for_agent_run', 'cancelling_client', 'resuming', 'completed', 'cancelled') AND "last_error_code" IS NULL AND "last_error_detail" IS NULL)),
    CHECK ("last_error_code" IS NULL OR (LENGTH("last_error_code") BETWEEN 1 AND 128)),
    CHECK ("last_error_detail" IS NULL OR (LENGTH("last_error_detail") BETWEEN 1 AND 4096)),
    CHECK ("steer_revision" >= 0),
    CHECK (("steer_revision" = 0 AND "last_steer_applied_at" IS NULL) OR ("steer_revision" >= 1 AND "last_steer_applied_at" IS NOT NULL)),
    CHECK ("model_steps" >= 0 AND "model_steps" <= 2147483647 AND "input_tokens" >= 0 AND "input_tokens" <= 4294967295 AND "output_tokens" >= 0 AND "output_tokens" <= 4294967295 AND "cache_read_input_tokens" >= 0 AND "cache_read_input_tokens" <= 4294967295 AND "cache_creation_input_tokens" >= 0 AND "cache_creation_input_tokens" <= 4294967295),
    CHECK ("last_steer_applied_at" IS NULL OR ("last_steer_applied_at" >= "created_at" AND "last_steer_applied_at" <= "updated_at"))
);

CREATE UNIQUE INDEX "idx_turn_run_chat_identity" ON "turn_run" ("chat_id", "id");
CREATE UNIQUE INDEX "idx_turn_run_input_message" ON "turn_run" ("input_message_id");
CREATE UNIQUE INDEX "idx_turn_run_one_active" ON "turn_run" ("chat_id") WHERE "status" IN ('queued', 'running', 'cancelling', 'waiting_for_client', 'waiting_for_agent_run', 'cancelling_client', 'resuming', 'retry_wait');
CREATE INDEX "idx_turn_run_due" ON "turn_run" ("status", "available_at", "created_at");
CREATE UNIQUE INDEX "idx_turn_run_lease_token" ON "turn_run" ("lease_token");
CREATE INDEX "idx_turn_run_stale_lease" ON "turn_run" ("status", "lease_expires_at");
CREATE INDEX "idx_turn_run_history" ON "turn_run" ("chat_id", "created_at");
CREATE UNIQUE INDEX "idx_turn_run_admission_owner" ON "turn_run" ("id", "chat_id", "agent_run_id");

CREATE TABLE "queued_turn" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "content" text NOT NULL,
    "attachments_json" text NOT NULL DEFAULT '[]',
    "file_attachments_json" text NOT NULL DEFAULT '[]',
    "invoked_skills_json" text NOT NULL DEFAULT '[]',
    "voice_input_used" boolean NOT NULL DEFAULT FALSE,
    "position" integer NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    CHECK (LENGTH("content") > 0),
    CHECK ("position" >= 0)
);

CREATE INDEX "ix_queued_turn_chat_position" ON "queued_turn" ("chat_id", "position");

CREATE TABLE "event" (
    "chat_id" uuid_text NOT NULL,
    "seq" integer NOT NULL,
    "turn_id" uuid_text,
    "lease_token" uuid_text,
    "attempt_event_ordinal" integer,
    "scan_token" uuid_text,
    "terminal" boolean NOT NULL DEFAULT FALSE,
    "payload" jsonb_text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("chat_id",
    "seq"),
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id"),
    FOREIGN KEY ("chat_id",
    "turn_id") REFERENCES "turn_run" ("chat_id",
    "id") ON DELETE RESTRICT,
    FOREIGN KEY ("turn_id",
    "lease_token") REFERENCES "turn_claim" ("turn_id",
    "token") ON DELETE RESTRICT,
    CHECK ("terminal" = FALSE OR "turn_id" IS NOT NULL),
    CHECK (("lease_token" IS NULL AND "attempt_event_ordinal" IS NULL) OR ("lease_token" IS NOT NULL AND ("attempt_event_ordinal" IS NOT NULL AND "turn_id" IS NOT NULL))),
    CHECK ("attempt_event_ordinal" IS NULL OR "attempt_event_ordinal" >= 1),
    CHECK ("turn_id" IS NULL OR "terminal" = TRUE OR "lease_token" IS NOT NULL),
    CHECK ("scan_token" IS NULL OR "terminal" = TRUE)
);

CREATE UNIQUE INDEX "idx_event_attempt_ordinal" ON "event" ("lease_token", "attempt_event_ordinal");
CREATE UNIQUE INDEX "idx_event_scan_token" ON "event" ("scan_token");
CREATE UNIQUE INDEX "idx_event_one_terminal_per_turn" ON "event" ("turn_id") WHERE "terminal" = TRUE;

CREATE TABLE "agent_run_inbox" (
    "child_run_id" uuid_text NOT NULL PRIMARY KEY,
    "parent_run_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "parent_depth" smallint NOT NULL,
    "result_lease_token" uuid_text NOT NULL,
    "result_attempt_count" integer NOT NULL,
    "result_claim_count" integer NOT NULL,
    "status" varchar NOT NULL,
    "claim_count" integer NOT NULL,
    "lease_token" uuid_text NULL,
    "lease_expires_at" timestamp_with_timezone_text NULL,
    "consumed_lease_token" uuid_text NULL,
    "consumed_at" timestamp_with_timezone_text NULL,
    "delivered_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("child_run_id",
    "parent_run_id",
    "chat_id") REFERENCES "agent_run" ("id",
    "parent_id",
    "chat_id") ON DELETE RESTRICT,
    FOREIGN KEY ("parent_run_id",
    "chat_id",
    "parent_depth") REFERENCES "agent_run" ("id",
    "chat_id",
    "depth") ON DELETE RESTRICT,
    FOREIGN KEY ("child_run_id",
    "result_lease_token",
    "result_attempt_count",
    "result_claim_count") REFERENCES "agent_run_result" ("agent_run_id",
    "lease_token",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK ("parent_depth" = 0),
    CHECK ("result_attempt_count" >= 1),
    CHECK ("result_claim_count" >= "result_attempt_count"),
    CHECK ("status" IN ('pending', 'claimed', 'consumed', 'cancelled')),
    CHECK ("claim_count" >= 0),
    CHECK (("status" = 'pending' AND "claim_count" = 0 AND "lease_token" IS NULL AND "lease_expires_at" IS NULL AND "consumed_lease_token" IS NULL AND "consumed_at" IS NULL) OR ("status" = 'claimed' AND "claim_count" >= 1 AND "lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL AND "consumed_lease_token" IS NULL AND "consumed_at" IS NULL) OR ("status" = 'consumed' AND "claim_count" >= 1 AND "lease_token" IS NULL AND "lease_expires_at" IS NULL AND "consumed_lease_token" IS NOT NULL AND "consumed_at" IS NOT NULL) OR ("status" = 'cancelled' AND "lease_token" IS NULL AND "lease_expires_at" IS NULL AND "consumed_lease_token" IS NULL AND "consumed_at" IS NULL))
);


CREATE TABLE "turn_steer" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "turn_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "content" text NOT NULL,
    "invoked_skills" jsonb_text NOT NULL,
    "voice_input_used" boolean NOT NULL DEFAULT FALSE,
    "interrupt" boolean NOT NULL DEFAULT FALSE,
    "status" varchar(16) NOT NULL DEFAULT 'pending',
    "applied_lease_token" uuid_text,
    "message_id" uuid_text,
    "preceding_assistant_message_id" uuid_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "resolved_at" timestamp_with_timezone_text,
    FOREIGN KEY ("chat_id",
    "turn_id") REFERENCES "turn_run" ("chat_id",
    "id") ON DELETE CASCADE,
    FOREIGN KEY ("preceding_assistant_message_id",
    "chat_id",
    "turn_id") REFERENCES "message" ("id",
    "chat_id",
    "turn_id") ON DELETE RESTRICT,
    FOREIGN KEY ("applied_lease_token",
    "turn_id") REFERENCES "turn_claim" ("token",
    "turn_id") ON DELETE RESTRICT,
    FOREIGN KEY ("message_id",
    "chat_id",
    "turn_id") REFERENCES "message" ("id",
    "chat_id",
    "turn_id") ON DELETE RESTRICT,
    CHECK (LENGTH("content") BETWEEN 1 AND 65536),
    CHECK (("status" = 'pending' AND "applied_lease_token" IS NULL AND "message_id" IS NULL AND "preceding_assistant_message_id" IS NULL AND "resolved_at" IS NULL) OR ("status" = 'applied' AND "applied_lease_token" IS NOT NULL AND "message_id" IS NOT NULL AND "resolved_at" IS NOT NULL) OR ("status" = 'rejected' AND "applied_lease_token" IS NULL AND "message_id" IS NULL AND "preceding_assistant_message_id" IS NULL AND "resolved_at" IS NOT NULL)),
    CHECK ("message_id" IS NULL OR "message_id" = "id"),
    CHECK ("resolved_at" IS NULL OR "resolved_at" >= "created_at")
);

CREATE INDEX "idx_turn_steer_pending" ON "turn_steer" ("turn_id", "status", "created_at", "id");
CREATE UNIQUE INDEX "idx_turn_steer_message" ON "turn_steer" ("message_id");

CREATE TABLE "tool_call" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "turn_id" uuid_text NOT NULL,
    "provider_id" text NOT NULL,
    "history_order" integer NOT NULL,
    "name" text NOT NULL,
    "arguments" jsonb_text NOT NULL,
    "execution" text NOT NULL,
    "status" text NOT NULL,
    "result" text,
    "error_code" text,
    "error_detail" text,
    "approval_status" text,
    "approval_class" text,
    "approval_kind" text,
    "approval_reason" text,
    "approval_requested_at" timestamp_with_timezone_text,
    "approval_decided_at" timestamp_with_timezone_text,
    "approval_event_seq" integer,
    "client_executor_id" uuid_text,
    "client_lease_token" uuid_text,
    "client_lease_expires_at" timestamp_with_timezone_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "resolved_at" timestamp_with_timezone_text,
    "turn_lease_token" uuid_text,
    "resolution_turn_lease_token" uuid_text,
    "approval_grant_source_call_id" uuid_text,
    "result_preview" jsonb_text,
    "provider_replay" jsonb_text,
    "auto_judge_status" text,
    "raw_arguments" text,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id"),
    FOREIGN KEY ("chat_id",
    "approval_event_seq") REFERENCES "event" ("chat_id",
    "seq"),
    CHECK ("execution" IN ('server', 'client', 'orchestration')),
    CHECK ("status" IN ('pending', 'completed', 'failed', 'cancelled')),
    CHECK ("history_order" > 0),
    CHECK (LENGTH("provider_id") BETWEEN 1 AND 256),
    CHECK (LENGTH("name") BETWEEN 1 AND 256),
    CHECK ("result" IS NULL OR LENGTH("result") <= 524288),
    CHECK ("error_code" IS NULL OR (LENGTH("error_code") BETWEEN 1 AND 128)),
    CHECK ("error_detail" IS NULL OR (LENGTH("error_detail") BETWEEN 1 AND 4096)),
    CHECK ("resolved_at" IS NULL OR "resolved_at" >= "created_at"),
    CHECK ("client_lease_expires_at" IS NULL OR "client_lease_expires_at" > "created_at"),
    CHECK (("status" = 'pending' AND "result" IS NULL AND "error_code" IS NULL AND "error_detail" IS NULL AND "resolved_at" IS NULL) OR ("status" <> 'pending' AND "result" IS NOT NULL AND "resolved_at" IS NOT NULL AND "client_lease_expires_at" IS NULL)),
    CHECK (("status" = 'failed' AND "error_code" IS NOT NULL) OR ("status" <> 'failed' AND "error_code" IS NULL AND "error_detail" IS NULL)),
    CHECK (("execution" = 'server' AND "client_executor_id" IS NULL AND "client_lease_token" IS NULL AND "client_lease_expires_at" IS NULL) OR ("execution" = 'client' AND (("status" = 'pending' AND (("client_executor_id" IS NULL AND "client_lease_token" IS NULL AND "client_lease_expires_at" IS NULL) OR ("client_executor_id" IS NOT NULL AND "client_lease_token" IS NOT NULL AND "client_lease_expires_at" IS NOT NULL))) OR ("status" <> 'pending' AND "client_executor_id" IS NOT NULL AND "client_lease_token" IS NOT NULL AND "client_lease_expires_at" IS NULL) OR ("status" = 'cancelled' AND "client_executor_id" IS NULL AND "client_lease_token" IS NULL AND "client_lease_expires_at" IS NULL))) OR ("execution" = 'orchestration' AND "status" IN ('pending', 'completed', 'cancelled') AND "error_code" IS NULL AND "error_detail" IS NULL AND "client_executor_id" IS NULL AND "client_lease_token" IS NULL AND "client_lease_expires_at" IS NULL)),
    CHECK (("approval_status" IS NULL AND "approval_class" IS NULL AND "approval_kind" IS NULL AND "approval_reason" IS NULL AND "approval_requested_at" IS NULL AND "approval_decided_at" IS NULL AND "approval_event_seq" IS NULL) OR ("execution" = 'server' AND "approval_status" IN ('pending', 'approved', 'rejected') AND "approval_class" = 'sensitive' AND "approval_kind" IN ('search_may_share_query_and_excerpts', 'exec_may_run_networked_command', 'unsupported') AND "approval_requested_at" IS NOT NULL AND (("approval_status" = 'pending' AND "status" = 'pending' AND "approval_reason" IS NULL AND "approval_decided_at" IS NULL) OR ("approval_status" = 'approved' AND "approval_reason" IS NULL AND "approval_decided_at" IS NOT NULL) OR ("approval_status" = 'rejected' AND "approval_reason" IS NOT NULL AND "approval_decided_at" IS NOT NULL))) OR ("execution" = 'orchestration' AND "status" = 'completed' AND "approval_status" = 'approved' AND "approval_class" = 'sensitive' AND "approval_kind" = 'unsupported' AND "approval_reason" IS NULL AND "approval_requested_at" IS NOT NULL AND "approval_decided_at" IS NOT NULL)),
    CHECK ("approval_reason" IS NULL OR (LENGTH("approval_reason") BETWEEN 1 AND 512)),
    CHECK ("approval_decided_at" IS NULL OR "approval_decided_at" >= "approval_requested_at")
);

CREATE UNIQUE INDEX "idx_tool_call_chat_history" ON "tool_call" ("chat_id", "history_order");
CREATE UNIQUE INDEX "idx_tool_call_wait_identity" ON "tool_call" ("id", "chat_id", "turn_id");
CREATE UNIQUE INDEX "idx_tool_call_checkpoint_identity" ON "tool_call" ("id", "chat_id", "history_order");
CREATE INDEX "idx_tool_call_client_pending" ON "tool_call" ("chat_id", "execution", "status", "client_lease_expires_at");

CREATE TABLE "standing_tool_grant" (
    "source_call_id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text,
    "tool_name" text NOT NULL,
    "approval_kind" varchar(64) NOT NULL,
    "scope" jsonb_text NOT NULL,
    "granted_at" timestamp_with_timezone_text NOT NULL,
    "project_id" uuid_text,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("project_id") REFERENCES "project" ("id") ON DELETE CASCADE,
    CHECK (LENGTH("tool_name") BETWEEN 1 AND 256),
    CHECK ("approval_kind" IN ('search_may_share_query_and_excerpts', 'web_search_may_share_query', 'exec_may_run_networked_command', 'web_extract_may_fetch_url', 'workspace_may_modify_files', 'delegate_may_run_background_agent')),
    CHECK (("chat_id" IS NOT NULL AND "project_id" IS NULL) OR ("chat_id" IS NULL AND "project_id" IS NOT NULL))
);

CREATE INDEX "idx_standing_tool_grant_lookup" ON "standing_tool_grant" ("chat_id", "tool_name", "approval_kind", "granted_at");

CREATE TABLE "operation_log" (
    "run_id" uuid_text NOT NULL,
    "operation_id" uuid_text NOT NULL,
    "state" varchar(16) NOT NULL,
    "fingerprint" blob(1) NOT NULL,
    "external_effect" boolean NOT NULL,
    "owner_epoch" uuid_text NOT NULL,
    "body" blob(1) NULL,
    "retained" boolean NOT NULL DEFAULT TRUE,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL, CONSTRAINT "pk_operation_log" PRIMARY KEY ("run_id",
    "operation_id"),
    CHECK ("state" IN ('claimed', 'recorded', 'failed')),
    CHECK ("state" <> 'claimed' OR "body" IS NULL),
    CHECK ("retained" OR "body" IS NULL)
);


CREATE TABLE "plan_request" (
    "call_id" uuid_text NOT NULL PRIMARY KEY,
    "turn_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "status" varchar(16) NOT NULL,
    "event_seq" integer NOT NULL,
    "title" varchar(120) NOT NULL,
    "plan" text NOT NULL,
    "feedback" varchar(4000),
    "proposed_at" timestamp_with_timezone_text NOT NULL,
    "resolved_at" timestamp_with_timezone_text,
    FOREIGN KEY ("call_id") REFERENCES "tool_call" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("turn_id") REFERENCES "turn_run" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("chat_id",
    "event_seq") REFERENCES "event" ("chat_id",
    "seq") ON DELETE RESTRICT,
    CHECK ("status" IN ('pending', 'accepted', 'rejected', 'cancelled')),
    CHECK (("status" = 'pending' AND "resolved_at" IS NULL) OR ("status" <> 'pending' AND "resolved_at" IS NOT NULL)),
    CHECK ("resolved_at" IS NULL OR "resolved_at" >= "proposed_at")
);

CREATE INDEX "idx_plan_request_pending" ON "plan_request" ("chat_id", "proposed_at", "call_id") WHERE "status" = 'pending';
CREATE INDEX "idx_plan_request_turn" ON "plan_request" ("turn_id", "call_id");
CREATE UNIQUE INDEX "idx_plan_request_event" ON "plan_request" ("chat_id", "event_seq");

CREATE TABLE "task_plan" (
    "chat_id" uuid_text NOT NULL PRIMARY KEY,
    "turn_id" uuid_text NOT NULL,
    "call_id" uuid_text NOT NULL,
    "steps" text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("turn_id") REFERENCES "turn_run" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("call_id") REFERENCES "tool_call" ("id") ON DELETE RESTRICT,
    CHECK ("updated_at" >= "created_at")
);


CREATE TABLE "user_question_request" (
    "call_id" uuid_text NOT NULL PRIMARY KEY,
    "turn_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "status" varchar(16) NOT NULL,
    "event_seq" integer NOT NULL,
    "asked_at" timestamp_with_timezone_text NOT NULL,
    "resolved_at" timestamp_with_timezone_text,
    "additional_user_context" varchar(2000),
    FOREIGN KEY ("call_id") REFERENCES "tool_call" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("turn_id") REFERENCES "turn_run" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("chat_id",
    "event_seq") REFERENCES "event" ("chat_id",
    "seq") ON DELETE RESTRICT,
    CHECK ("status" IN ('pending', 'answered', 'cancelled')),
    CHECK (("status" = 'pending' AND "resolved_at" IS NULL) OR ("status" <> 'pending' AND "resolved_at" IS NOT NULL)),
    CHECK ("resolved_at" IS NULL OR "resolved_at" >= "asked_at")
);

CREATE INDEX "idx_user_question_request_pending" ON "user_question_request" ("chat_id", "asked_at", "call_id") WHERE "status" = 'pending';
CREATE INDEX "idx_user_question_request_chat" ON "user_question_request" ("chat_id", "call_id");
CREATE INDEX "idx_user_question_request_turn" ON "user_question_request" ("turn_id", "call_id");
CREATE UNIQUE INDEX "idx_user_question_request_event" ON "user_question_request" ("chat_id", "event_seq");

CREATE TABLE "user_question" (
    "call_id" uuid_text NOT NULL,
    "question_id" varchar(64) NOT NULL,
    "position" integer NOT NULL,
    "header" varchar(32) NOT NULL,
    "prompt" varchar(500) NOT NULL,
    "options" jsonb_text NOT NULL,
    "allow_free_form" boolean NOT NULL,
    "question_type" varchar(16) NOT NULL DEFAULT 'single_select',
    "answer_selected_option_ids" jsonb_text,
    "answer_custom_answer" varchar(2000),
    "response_recorded_at" timestamp_with_timezone_text, CONSTRAINT "pk_user_question" PRIMARY KEY ("call_id",
    "question_id"),
    FOREIGN KEY ("call_id") REFERENCES "user_question_request" ("call_id") ON DELETE RESTRICT,
    CHECK ("position" BETWEEN 0 AND 2)
);

CREATE UNIQUE INDEX "idx_user_question_order" ON "user_question" ("call_id", "position");

CREATE TABLE "turn_client_wait" (
    "call_id" uuid_text NOT NULL PRIMARY KEY,
    "turn_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "park_lease_token" uuid_text NOT NULL,
    "attempt_count" integer NOT NULL,
    "claim_count" integer NOT NULL,
    "model_steps" integer NOT NULL,
    "input_tokens" integer NOT NULL,
    "output_tokens" integer NOT NULL,
    "cache_read_input_tokens" integer NOT NULL,
    "cache_creation_input_tokens" integer NOT NULL,
    "status" text NOT NULL,
    "parked_at" timestamp_with_timezone_text NOT NULL,
    "closed_at" timestamp_with_timezone_text,
    FOREIGN KEY ("call_id",
    "chat_id",
    "turn_id") REFERENCES "tool_call" ("id",
    "chat_id",
    "turn_id") ON DELETE RESTRICT,
    FOREIGN KEY ("park_lease_token",
    "turn_id",
    "attempt_count",
    "claim_count") REFERENCES "turn_claim" ("token",
    "turn_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK ("status" IN ('waiting', 'resumed', 'cancelled')),
    CHECK (("status" = 'waiting' AND "closed_at" IS NULL) OR ("status" <> 'waiting' AND "closed_at" IS NOT NULL)),
    CHECK ("closed_at" IS NULL OR "closed_at" >= "parked_at"),
    CHECK ("model_steps" > 0 AND "model_steps" <= 2147483647 AND "input_tokens" >= 0 AND "input_tokens" <= 4294967295 AND "output_tokens" >= 0 AND "output_tokens" <= 4294967295 AND "cache_read_input_tokens" >= 0 AND "cache_read_input_tokens" <= 4294967295 AND "cache_creation_input_tokens" >= 0 AND "cache_creation_input_tokens" <= 4294967295)
);

CREATE UNIQUE INDEX "idx_turn_client_wait_one_open" ON "turn_client_wait" ("turn_id") WHERE "status" = 'waiting';
CREATE INDEX "idx_turn_client_wait_history" ON "turn_client_wait" ("turn_id", "parked_at", "call_id");

CREATE TABLE "context_checkpoint" (
    "chat_id" uuid_text NOT NULL PRIMARY KEY,
    "source_message_id" uuid_text NOT NULL,
    "source_message_seq" integer NOT NULL,
    "format_version" integer NOT NULL,
    "content" varchar(12288) NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "input_tokens" integer NOT NULL DEFAULT 0,
    "output_tokens" integer NOT NULL DEFAULT 0,
    "cache_read_input_tokens" integer NOT NULL DEFAULT 0,
    "cache_creation_input_tokens" integer NOT NULL DEFAULT 0,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("source_message_id") REFERENCES "message" ("id") ON DELETE RESTRICT,
    CHECK ("source_message_seq" > 0),
    CHECK ("format_version" IN (1, 2))
);


CREATE TABLE "document" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text,
    "project_id" uuid_text,
    "origin_uri" text,
    "media_type" text NOT NULL,
    "title" text,
    "source_blob_id" uuid_text,
    "source_sha256" blob(1),
    "source_byte_len" integer,
    "canonical_text" text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    "owner" text NOT NULL DEFAULT 'local',
    FOREIGN KEY ("project_id") REFERENCES "project" ("id") ON DELETE RESTRICT,
    CHECK (("source_blob_id" IS NULL AND "source_sha256" IS NULL AND "source_byte_len" IS NULL) OR ("source_blob_id" IS NOT NULL AND "source_sha256" IS NOT NULL AND "source_byte_len" IS NOT NULL AND (LENGTH(source_sha256) = 32) AND "source_byte_len" >= 0)),
    CHECK ("media_type" <> ''),
    CHECK ("origin_uri" IS NULL OR "origin_uri" <> '')
);

CREATE INDEX "idx_document_project_created" ON "document" ("project_id", "created_at");
CREATE INDEX "idx_document_source_blob" ON "document" ("source_blob_id");
CREATE INDEX "idx_document_chat_created" ON "document" ("chat_id", "created_at");

CREATE TABLE "output" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "filename" text NOT NULL,
    "media_type" text NOT NULL,
    "current_revision_id" uuid_text NOT NULL,
    "revision_count" integer NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    "deleted_at" timestamp_with_timezone_text,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    CHECK ("revision_count" BETWEEN 1 AND 100)
);

CREATE INDEX "idx_output_chat_created" ON "output" ("chat_id", "created_at", "id");
CREATE UNIQUE INDEX "idx_output_chat_live_filename" ON "output" ("chat_id", "filename") WHERE "deleted_at" IS NULL;

CREATE TABLE "output_revision" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "output_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "byte_len" integer NOT NULL,
    "sha256" blob(32) NOT NULL,
    "turn_id" uuid_text,
    "producing_run_id" uuid_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("output_id") REFERENCES "output" ("id") ON DELETE CASCADE,
    CHECK ("ordinal" BETWEEN 1 AND 100),
    CHECK ("byte_len" >= 0),
    CHECK ("byte_len" <= 16777216),
    CHECK ("turn_id" IS NULL OR "producing_run_id" IS NULL)
);

CREATE UNIQUE INDEX "idx_output_revision_ordinal" ON "output_revision" ("output_id", "ordinal");

CREATE TABLE "assistant_citation" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "message_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "document_id" uuid_text NOT NULL,
    "locator" jsonb_text NOT NULL, CONSTRAINT "idx_assistant_citation_light_message_ordinal" UNIQUE ("message_id",
    "ordinal"),
    FOREIGN KEY ("message_id") REFERENCES "message" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("document_id") REFERENCES "document" ("id") ON DELETE CASCADE
);


CREATE TABLE "chat_image_publication" (
    "chat_id" uuid_text NOT NULL,
    "blob_id" uuid_text NOT NULL,
    "media_type" varchar(64) NOT NULL,
    "width" integer NOT NULL,
    "height" integer NOT NULL,
    "byte_len" integer NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("chat_id",
    "blob_id"),
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    CHECK ("blob_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("media_type" IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif')),
    CHECK ("width" BETWEEN 1 AND 8000),
    CHECK ("height" BETWEEN 1 AND 8000),
    CHECK ("byte_len" BETWEEN 1 AND 16777216)
);

CREATE INDEX "idx_chat_image_publication_blob" ON "chat_image_publication" ("blob_id");

CREATE TABLE "message_attachment" (
    "message_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "blob_id" uuid_text NOT NULL,
    "media_type" varchar(64) NOT NULL,
    "width" integer NOT NULL,
    "height" integer NOT NULL,
    "byte_len" integer NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("message_id",
    "ordinal"),
    FOREIGN KEY ("message_id") REFERENCES "message" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    CHECK ("blob_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("ordinal" >= 0),
    CHECK ("ordinal" < 16),
    CHECK ("media_type" IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif')),
    CHECK ("width" BETWEEN 1 AND 8000),
    CHECK ("height" BETWEEN 1 AND 8000),
    CHECK ("byte_len" BETWEEN 1 AND 16777216)
);

CREATE INDEX "idx_message_attachment_blob" ON "message_attachment" ("blob_id");
CREATE INDEX "idx_message_attachment_chat" ON "message_attachment" ("chat_id", "message_id", "ordinal");

CREATE TABLE "message_document_attachment" (
    "message_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "document_id" uuid_text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("message_id",
    "ordinal"),
    FOREIGN KEY ("message_id") REFERENCES "message" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("document_id") REFERENCES "document" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE CASCADE,
    CHECK ("document_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("ordinal" >= 0),
    CHECK ("ordinal" < 16)
);

CREATE INDEX "idx_message_document_attachment_chat" ON "message_document_attachment" ("chat_id", "message_id", "ordinal");
CREATE INDEX "idx_message_document_attachment_document" ON "message_document_attachment" ("document_id");
CREATE UNIQUE INDEX "idx_message_document_attachment_unique" ON "message_document_attachment" ("message_id", "document_id");

CREATE TABLE "app" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "name" text NOT NULL,
    "current_revision_id" uuid_text NOT NULL,
    "revision_count" integer NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    "deleted_at" timestamp_with_timezone_text,
    CHECK ("revision_count" BETWEEN 1 AND 100)
);

CREATE INDEX "idx_app_updated" ON "app" ("updated_at", "id");
CREATE INDEX "idx_app_owner_updated" ON "app" ("owner", "updated_at", "id");

CREATE TABLE "app_revision" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "app_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "manifest_json" jsonb_text NOT NULL,
    "byte_len" integer NOT NULL,
    "sha256" blob(32) NOT NULL,
    "turn_id" uuid_text,
    "producing_run_id" uuid_text,
    "chat_id" uuid_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("app_id") REFERENCES "app" ("id") ON DELETE CASCADE,
    CHECK ("ordinal" BETWEEN 1 AND 100),
    CHECK ("byte_len" BETWEEN 1 AND 1048576),
    CHECK ("turn_id" IS NULL OR "producing_run_id" IS NULL)
);

CREATE UNIQUE INDEX "idx_app_revision_ordinal" ON "app_revision" ("app_id", "ordinal");

CREATE TABLE "app_grant" (
    "app_id" uuid_text NOT NULL PRIMARY KEY,
    "bindings_json" jsonb_text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("app_id") REFERENCES "app" ("id") ON DELETE CASCADE
);


CREATE TABLE "app_gateway_draft" (
    "app_id" uuid_text NOT NULL,
    "gateway_base_url" text NOT NULL,
    "shared_app_id" text NOT NULL,
    "gateway_revision_id" text NOT NULL,
    "synced_revision_id" uuid_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("app_id",
    "gateway_base_url"),
    FOREIGN KEY ("app_id") REFERENCES "app" ("id") ON DELETE CASCADE,
    CHECK ("gateway_base_url" <> ''),
    CHECK ("shared_app_id" <> ''),
    CHECK ("gateway_revision_id" <> '')
);


CREATE TABLE "connected_app" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "name" varchar NOT NULL,
    "kind" varchar NOT NULL,
    "definition_json" jsonb_text NOT NULL,
    "position" integer NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL
);

CREATE UNIQUE INDEX "idx_connected_app_kind_name" ON "connected_app" ("kind", "name");

CREATE TABLE "sandbox_provision" (
    "run_id" uuid_text NOT NULL PRIMARY KEY,
    "tag" varchar(64) NOT NULL UNIQUE,
    "state" varchar(16) NOT NULL,
    "handle" varchar NULL,
    "window_expires_at" timestamp_with_timezone_text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    "late_result_evidence" text NULL,
    "admission" varchar(16) NOT NULL DEFAULT 'attached_only',
    CHECK ("state" IN ('intended', 'committed', 'teardown', 'done')),
    CHECK ("state" <> 'intended' OR "handle" IS NULL),
    CHECK ("state" <> 'committed' OR "handle" IS NOT NULL)
);


CREATE TABLE "sandbox_tool_call" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "agent_run_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "agent_run_depth" smallint NOT NULL,
    "provider_id" text NOT NULL,
    "name" text NOT NULL,
    "arguments" jsonb_text NOT NULL,
    "status" text NOT NULL,
    "park_lease_token" uuid_text NOT NULL,
    "park_attempt_count" integer NOT NULL,
    "park_claim_count" integer NOT NULL,
    "batch_ordinal" smallint NOT NULL,
    "executor_lease_token" uuid_text,
    "executor_lease_expires_at" timestamp_with_timezone_text,
    "retry_at" timestamp_with_timezone_text,
    "resolution_lease_token" uuid_text,
    "result" text,
    "error_code" text,
    "error_detail" text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "resolved_at" timestamp_with_timezone_text,
    FOREIGN KEY ("agent_run_id",
    "chat_id",
    "agent_run_depth") REFERENCES "agent_run" ("id",
    "chat_id",
    "depth") ON DELETE RESTRICT,
    FOREIGN KEY ("park_lease_token",
    "agent_run_id",
    "park_attempt_count",
    "park_claim_count") REFERENCES "agent_run_claim" ("token",
    "agent_run_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK ("agent_run_depth" = 1),
    CHECK ("park_attempt_count" >= 1),
    CHECK ("batch_ordinal" >= 0),
    CHECK ("park_claim_count" >= "park_attempt_count"),
    CHECK ("status" IN ('accepted', 'claimed', 'retry_wait', 'completed', 'failed', 'cancelled')),
    CHECK ("resolved_at" IS NULL OR "resolved_at" >= "created_at"),
    CHECK (("result" IS NULL AND "resolution_lease_token" IS NULL AND "resolved_at" IS NULL) OR ("result" IS NOT NULL AND "resolution_lease_token" IS NOT NULL AND "resolved_at" IS NOT NULL)),
    CHECK (("status" = 'failed' AND "error_code" IS NOT NULL) OR ("status" NOT IN ('failed') AND "error_code" IS NULL AND "error_detail" IS NULL))
);

CREATE INDEX "idx_sandbox_tool_call_run" ON "sandbox_tool_call" ("agent_run_id", "created_at");
CREATE INDEX "idx_sandbox_tool_call_recovery" ON "sandbox_tool_call" ("status", "executor_lease_expires_at", "created_at", "id");
CREATE UNIQUE INDEX "idx_sandbox_tool_call_batch" ON "sandbox_tool_call" ("agent_run_id", "park_attempt_count", "park_claim_count", "batch_ordinal");

CREATE TABLE "sandbox_spawn_checkpoint" (
    "call_id" uuid_text NOT NULL PRIMARY KEY,
    "child_run_id" uuid_text NOT NULL UNIQUE,
    "parent_run_id" uuid_text NOT NULL,
    "origin_turn_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "lease_token" uuid_text NOT NULL UNIQUE,
    "attempt_count" integer NOT NULL,
    "claim_count" integer NOT NULL,
    "provider_id" text NOT NULL,
    "history_order" integer NOT NULL,
    "arguments" jsonb_text NOT NULL,
    "result" text NOT NULL,
    "remaining_requests" jsonb_text NOT NULL,
    "steer_revision" integer NOT NULL,
    "event_ordinal" integer NOT NULL,
    "model_steps" integer NOT NULL,
    "input_tokens" integer NOT NULL,
    "output_tokens" integer NOT NULL,
    "cache_read_input_tokens" integer NOT NULL,
    "cache_creation_input_tokens" integer NOT NULL,
    "event_seq" integer NOT NULL,
    "committed_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("child_run_id",
    "origin_turn_id",
    "parent_run_id",
    "chat_id") REFERENCES "agent_run" ("id",
    "origin_turn_id",
    "parent_id",
    "chat_id") ON DELETE RESTRICT,
    FOREIGN KEY ("lease_token",
    "origin_turn_id",
    "attempt_count",
    "claim_count") REFERENCES "turn_claim" ("token",
    "turn_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    FOREIGN KEY ("call_id",
    "chat_id",
    "history_order") REFERENCES "tool_call" ("id",
    "chat_id",
    "history_order") ON DELETE RESTRICT,
    FOREIGN KEY ("chat_id",
    "event_seq") REFERENCES "event" ("chat_id",
    "seq") ON DELETE RESTRICT,
    CHECK ("attempt_count" >= 1),
    CHECK ("claim_count" >= "attempt_count"),
    CHECK ("steer_revision" >= 0),
    CHECK ("event_ordinal" BETWEEN 2 AND 2147483646),
    CHECK ("model_steps" >= 0),
    CHECK ("history_order" > 0),
    CHECK ("input_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("output_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("cache_read_input_tokens" BETWEEN 0 AND 4294967295),
    CHECK ("cache_creation_input_tokens" BETWEEN 0 AND 4294967295),
    CHECK (LENGTH("provider_id") BETWEEN 1 AND 256),
    CHECK (LENGTH("result") <= 524288)
);

CREATE UNIQUE INDEX "idx_sandbox_spawn_checkpoint_event" ON "sandbox_spawn_checkpoint" ("chat_id", "event_seq");
CREATE UNIQUE INDEX "idx_sandbox_spawn_checkpoint_claim_segment" ON "sandbox_spawn_checkpoint" ("origin_turn_id", "attempt_count", "claim_count");

CREATE TABLE "agent_run_task_plan" (
    "agent_run_id" uuid_text NOT NULL PRIMARY KEY,
    "call_id" uuid_text NOT NULL,
    "steps" text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("agent_run_id") REFERENCES "agent_run" ("id") ON DELETE RESTRICT,
    FOREIGN KEY ("call_id") REFERENCES "sandbox_tool_call" ("id") ON DELETE RESTRICT,
    CHECK ("updated_at" >= "created_at")
);


CREATE TABLE "exec_file_change" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "chat_id" uuid_text NOT NULL,
    "turn_id" uuid_text NOT NULL,
    "classification" varchar(16) NOT NULL,
    "folder_path" text NOT NULL,
    "relative_path" text NOT NULL,
    "change_kind" varchar(32) NULL,
    "prior_blob_id" uuid_text NULL,
    "prior_byte_len" integer NULL,
    "new_sha256" varchar(64) NULL,
    "undo_state" varchar(32) NULL,
    "reason" varchar(32) NULL,
    "recorded_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("chat_id") REFERENCES "chat" ("id") ON DELETE RESTRICT,
    CHECK ("classification" IN ('applied', 'rejected')),
    CHECK (("classification" = 'applied' AND "change_kind" IS NOT NULL AND "undo_state" IS NOT NULL AND "reason" IS NULL) OR ("classification" = 'rejected' AND "reason" IS NOT NULL AND "change_kind" IS NULL AND "undo_state" IS NULL AND "prior_blob_id" IS NULL AND "prior_byte_len" IS NULL AND "new_sha256" IS NULL)),
    CHECK ("prior_blob_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("prior_byte_len" >= 0),
    CHECK ("change_kind" IN ('created', 'overwritten', 'deleted')),
    CHECK ("undo_state" IN ('available', 'prior_too_large', 'prior_unreadable')),
    CHECK ("reason" IN ('stale', 'snapshot_unavailable', 'staged_file_too_large', 'trash_unavailable', 'unavailable'))
);

CREATE INDEX "idx_exec_file_change_blob" ON "exec_file_change" ("prior_blob_id");
CREATE INDEX "idx_exec_file_change_chat_turn" ON "exec_file_change" ("chat_id", "recorded_at", "turn_id");

CREATE TABLE "turn_agent_run_wait_set" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "parent_run_id" uuid_text NOT NULL,
    "turn_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "condition" text NOT NULL,
    "park_lease_token" uuid_text NOT NULL,
    "expected_steer_revision" integer NOT NULL,
    "attempt_count" integer NOT NULL,
    "claim_count" integer NOT NULL,
    "model_steps" integer NOT NULL,
    "input_tokens" integer NOT NULL,
    "output_tokens" integer NOT NULL,
    "cache_read_input_tokens" integer NOT NULL,
    "cache_creation_input_tokens" integer NOT NULL,
    "event_ordinal" integer NOT NULL,
    "event_seq" integer,
    "status" text NOT NULL,
    "parked_at" timestamp_with_timezone_text NOT NULL,
    "closed_at" timestamp_with_timezone_text,
    "resume_token" uuid_text,
    FOREIGN KEY ("turn_id",
    "chat_id",
    "parent_run_id") REFERENCES "turn_run" ("id",
    "chat_id",
    "agent_run_id") ON DELETE RESTRICT,
    FOREIGN KEY ("id",
    "chat_id",
    "turn_id") REFERENCES "tool_call" ("id",
    "chat_id",
    "turn_id") ON DELETE RESTRICT,
    FOREIGN KEY ("chat_id",
    "event_seq") REFERENCES "event" ("chat_id",
    "seq") ON DELETE RESTRICT,
    FOREIGN KEY ("park_lease_token",
    "turn_id",
    "attempt_count",
    "claim_count") REFERENCES "turn_claim" ("token",
    "turn_id",
    "attempt_count",
    "claim_count") ON DELETE RESTRICT,
    CHECK ("condition" = 'all'),
    CHECK ("expected_steer_revision" >= 0),
    CHECK ("event_ordinal" BETWEEN 2 AND 2147483646),
    CHECK ("attempt_count" >= 1),
    CHECK ("claim_count" >= "attempt_count"),
    CHECK ("model_steps" > 0 AND "input_tokens" >= 0 AND "output_tokens" >= 0 AND "cache_read_input_tokens" >= 0 AND "cache_creation_input_tokens" >= 0),
    CHECK (("status" = 'waiting' AND "closed_at" IS NULL AND "resume_token" IS NULL AND "event_seq" IS NULL) OR ("status" = 'resumed' AND "closed_at" IS NOT NULL AND "resume_token" IS NOT NULL AND "event_seq" IS NOT NULL) OR ("status" = 'cancelled' AND "closed_at" IS NOT NULL AND "resume_token" IS NULL AND "event_seq" IS NOT NULL)),
    CHECK ("closed_at" IS NULL OR "closed_at" >= "parked_at")
);

CREATE UNIQUE INDEX "idx_turn_agent_run_wait_set_member_owner" ON "turn_agent_run_wait_set" ("id", "turn_id", "parent_run_id", "chat_id");
CREATE UNIQUE INDEX "idx_turn_agent_run_wait_set_one_open" ON "turn_agent_run_wait_set" ("turn_id") WHERE "status" = 'waiting';

CREATE TABLE "turn_agent_run_wait_member" (
    "wait_id" uuid_text NOT NULL,
    "position" smallint NOT NULL,
    "child_run_id" uuid_text NOT NULL,
    "parent_run_id" uuid_text NOT NULL,
    "origin_turn_id" uuid_text NOT NULL,
    "chat_id" uuid_text NOT NULL,
    "open" boolean NOT NULL, PRIMARY KEY ("wait_id",
    "position"),
    FOREIGN KEY ("wait_id",
    "origin_turn_id",
    "parent_run_id",
    "chat_id") REFERENCES "turn_agent_run_wait_set" ("id",
    "turn_id",
    "parent_run_id",
    "chat_id") ON DELETE CASCADE,
    FOREIGN KEY ("child_run_id",
    "origin_turn_id",
    "parent_run_id",
    "chat_id") REFERENCES "agent_run" ("id",
    "origin_turn_id",
    "parent_id",
    "chat_id") ON DELETE RESTRICT,
    CHECK ("position" >= 0),
    CHECK ("position" < 4)
);

CREATE UNIQUE INDEX "idx_turn_agent_run_wait_member_one_open_child" ON "turn_agent_run_wait_member" ("child_run_id") WHERE "open" = TRUE;

CREATE TABLE "code_repo" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "root_path" text NOT NULL,
    "display_name" text NOT NULL,
    "default_base_ref" text NOT NULL,
    "branch_prefix" text NOT NULL,
    "setup_script" text,
    "archive_script" text,
    "quick_actions" jsonb_text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "removed_at" timestamp_with_timezone_text,
    "cloned_from" text
);

CREATE UNIQUE INDEX "idx_code_repo_owner_root_path" ON "code_repo" ("owner", "root_path") WHERE "removed_at" IS NULL;

CREATE TABLE "code_workspace" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "repo_id" uuid_text NOT NULL,
    "title" text NOT NULL,
    "worktree_path" text NOT NULL UNIQUE,
    "branch_name" text NOT NULL,
    "base_ref" text NOT NULL,
    "status" text NOT NULL,
    "pr" jsonb_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "archived_at" timestamp_with_timezone_text,
    "released_at" timestamp_with_timezone_text,
    "released_tip" text,
    "bundle_bytes" integer,
    FOREIGN KEY ("repo_id") REFERENCES "code_repo" ("id"),
    CHECK ("status" IN ('creating', 'setup_failed', 'active', 'archived', 'released'))
);

CREATE UNIQUE INDEX "idx_code_workspace_repo_branch" ON "code_workspace" ("repo_id", "branch_name");
CREATE INDEX "idx_code_workspace_owner_created" ON "code_workspace" ("owner", "created_at");

CREATE TABLE "code_session" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "workspace_id" uuid_text NOT NULL,
    "kind" text NOT NULL DEFAULT 'interactive',
    "harness_kind" text NOT NULL,
    "harness_version" text,
    "harness_resume_ref" text,
    "permission_mode" text NOT NULL,
    "model" text,
    "reasoning_effort" text,
    "lifecycle" text NOT NULL,
    "fence_reason" jsonb_text,
    "child_pid" integer,
    "spawn_epoch" integer NOT NULL DEFAULT 0,
    "attention_state" jsonb_text NOT NULL,
    "attention_source" text NOT NULL,
    "unrecognized_event_count" integer NOT NULL DEFAULT 0,
    "subagents" jsonb_text,
    "created_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("workspace_id") REFERENCES "code_workspace" ("id"),
    CHECK ("lifecycle" IN ('created', 'idle', 'running', 'fenced', 'ended')),
    CHECK ("kind" IN ('interactive', 'watch')),
    CHECK ("permission_mode" IN ('plan', 'ask', 'auto', 'allow')),
    CHECK ("reasoning_effort" IS NULL OR "reasoning_effort" IN ('none', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra')),
    CHECK ("spawn_epoch" >= 0),
    CHECK ("unrecognized_event_count" >= 0)
);

CREATE INDEX "idx_code_session_workspace" ON "code_session" ("workspace_id");

CREATE TABLE "code_turn" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "session_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "status" text NOT NULL,
    "user_input" text NOT NULL,
    "user_input_blob_id" uuid_text,
    "checkpoint_ref" text,
    "diffstat" jsonb_text,
    "usage" jsonb_text,
    "narrative" text,
    "started_at" timestamp_with_timezone_text NOT NULL,
    "ended_at" timestamp_with_timezone_text,
    FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"),
    CHECK ("status" IN ('running', 'completed', 'failed', 'interrupted')),
    CHECK ("ordinal" >= 1)
);

CREATE UNIQUE INDEX "idx_code_turn_session_ordinal" ON "code_turn" ("session_id", "ordinal");

CREATE TABLE "code_turn_attachment" (
    "owner" text NOT NULL DEFAULT 'local',
    "turn_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "blob_id" uuid_text NOT NULL,
    "media_type" varchar(64) NOT NULL,
    "width" integer NOT NULL,
    "height" integer NOT NULL,
    "byte_len" integer NOT NULL, PRIMARY KEY ("turn_id",
    "ordinal"),
    FOREIGN KEY ("turn_id") REFERENCES "code_turn" ("id") ON DELETE CASCADE,
    CHECK ("blob_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("ordinal" >= 0),
    CHECK ("ordinal" < 16),
    CHECK ("width" BETWEEN 1 AND 8000),
    CHECK ("height" BETWEEN 1 AND 8000),
    CHECK ("media_type" IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif')),
    CHECK ("byte_len" BETWEEN 1 AND 16777216)
);

CREATE INDEX "idx_code_turn_attachment_blob" ON "code_turn_attachment" ("blob_id");

CREATE TABLE "code_session_image" (
    "session_id" uuid_text NOT NULL,
    "blob_id" uuid_text NOT NULL,
    "owner" text NOT NULL DEFAULT 'local',
    "media_type" varchar(64) NOT NULL,
    "width" integer NOT NULL,
    "height" integer NOT NULL,
    "byte_len" integer NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("session_id",
    "blob_id"),
    FOREIGN KEY ("session_id") REFERENCES "code_session" ("id") ON DELETE RESTRICT,
    CHECK ("blob_id" <> '00000000-0000-0000-0000-000000000000'),
    CHECK ("media_type" IN ('image/png', 'image/jpeg', 'image/webp', 'image/gif')),
    CHECK ("width" BETWEEN 1 AND 8000),
    CHECK ("height" BETWEEN 1 AND 8000),
    CHECK ("byte_len" BETWEEN 1 AND 16777216)
);

CREATE INDEX "idx_code_session_image_blob" ON "code_session_image" ("blob_id");

CREATE TABLE "code_event" (
    "owner" text NOT NULL DEFAULT 'local',
    "session_id" uuid_text NOT NULL,
    "seq" integer NOT NULL,
    "event" jsonb_text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("session_id",
    "seq"),
    FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"),
    CHECK ("seq" >= 1)
);


CREATE TABLE "code_approval" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "session_id" uuid_text NOT NULL,
    "turn_id" uuid_text NOT NULL,
    "kind" jsonb_text NOT NULL,
    "harness_raw" jsonb_text NOT NULL,
    "state" text NOT NULL,
    "feedback" text,
    "requested_at" timestamp_with_timezone_text NOT NULL,
    "decided_at" timestamp_with_timezone_text,
    FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"),
    FOREIGN KEY ("turn_id") REFERENCES "code_turn" ("id"),
    CHECK ("state" IN ('pending', 'approved', 'denied', 'abandoned'))
);

CREATE INDEX "idx_code_approval_session_state" ON "code_approval" ("session_id", "state");

CREATE TABLE "code_watch" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "workspace_id" uuid_text NOT NULL,
    "session_id" uuid_text NOT NULL,
    "pr_number" integer NOT NULL,
    "state" text NOT NULL,
    "detail" text,
    "last_fix_head" text,
    "cycles" integer NOT NULL DEFAULT 0,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("workspace_id") REFERENCES "code_workspace" ("id"),
    FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"),
    CHECK ("state" IN ('watching', 'fixing', 'blocked', 'done', 'stopped', 'failed')),
    CHECK ("pr_number" >= 1),
    CHECK ("cycles" >= 0)
);

CREATE INDEX "idx_code_watch_workspace" ON "code_watch" ("workspace_id");

CREATE TABLE "code_trigger" (
    "id" uuid_text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "repo_id" uuid_text NOT NULL,
    "condition" text NOT NULL,
    "action" text NOT NULL,
    "enabled" boolean NOT NULL DEFAULT TRUE,
    "created_at" timestamp_with_timezone_text NOT NULL,
    "updated_at" timestamp_with_timezone_text NOT NULL,
    FOREIGN KEY ("repo_id") REFERENCES "code_repo" ("id"),
    CHECK ("condition" IN ('checks_failed', 'conflicts', 'changes_requested', 'review_required', 'behind', 'ready_to_merge', 'merged', 'closed')),
    CHECK ("action" IN ('deliver', 'notify'))
);

CREATE INDEX "idx_code_trigger_repo" ON "code_trigger" ("repo_id");
CREATE UNIQUE INDEX "uq_code_trigger_rule" ON "code_trigger" ("owner", "repo_id", "condition");

CREATE TABLE "code_trigger_fire" (
    "owner" text NOT NULL DEFAULT 'local',
    "trigger_id" uuid_text NOT NULL,
    "workspace_id" uuid_text NOT NULL,
    "head_sha" text NOT NULL,
    "pr_number" integer NOT NULL,
    "fired_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("trigger_id",
    "workspace_id",
    "head_sha"),
    FOREIGN KEY ("trigger_id") REFERENCES "code_trigger" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("workspace_id") REFERENCES "code_workspace" ("id"),
    CHECK ("pr_number" >= 1)
);

CREATE INDEX "idx_code_trigger_fire_workspace" ON "code_trigger_fire" ("workspace_id");

INSERT INTO advisory_lock (name) VALUES ('turn_claim') ON CONFLICT DO NOTHING;
INSERT INTO advisory_lock (name) VALUES ('agent_run_claim') ON CONFLICT DO NOTHING;
INSERT INTO advisory_lock (name) VALUES ('turn_agent_run_wait') ON CONFLICT DO NOTHING;
