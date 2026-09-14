//! Map supervisor counters to the hosted turns whose input they consumed.
use sea_orm_migration::prelude::*;
pub(super) struct NativeTurnIdentity;
impl MigrationName for NativeTurnIdentity {
    fn name(&self) -> &str {
        "m20260914_000002_native_turn_identity"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for NativeTurnIdentity {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("code_native_turn_input"))
                    .col(
                        ColumnDef::new(Alias::new("turn_id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("incarnation_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("message_seq")).big_integer())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_native_input_turn")
                            .from(Alias::new("code_native_turn_input"), Alias::new("turn_id"))
                            .to(Alias::new("turn"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_native_input_incarnation")
                            .from(
                                Alias::new("code_native_turn_input"),
                                Alias::new("incarnation_id"),
                            )
                            .to(Alias::new("code_session_incarnation"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_native_input_sequence")
                            .col(Alias::new("incarnation_id"))
                            .col(Alias::new("message_seq"))
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("code_native_turn_observation"))
                    .col(
                        ColumnDef::new(Alias::new("incarnation_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("runtime_id")).uuid().not_null())
                    .col(
                        ColumnDef::new(Alias::new("native_turn"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("source")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("input_sequences"))
                            .json_binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("turn_id")).uuid())
                    .col(ColumnDef::new(Alias::new("terminal_status")).string())
                    .col(ColumnDef::new(Alias::new("assistant_record")).json_binary())
                    .col(
                        ColumnDef::new(Alias::new("start_journaled"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Alias::new("output_journaled"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("incarnation_id"))
                            .col(Alias::new("runtime_id"))
                            .col(Alias::new("native_turn")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_native_observation_incarnation")
                            .from(
                                Alias::new("code_native_turn_observation"),
                                Alias::new("incarnation_id"),
                            )
                            .to(Alias::new("code_session_incarnation"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_native_observation_turn")
                            .from(
                                Alias::new("code_native_turn_observation"),
                                Alias::new("turn_id"),
                            )
                            .to(Alias::new("turn"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq_native_observation_host_turn")
                            .col(Alias::new("runtime_id"))
                            .col(Alias::new("turn_id"))
                            .unique(),
                    )
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
