pub mod announcement_video_stage {
 use crate::tenant::prelude::*;
 #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
 #[sea_orm(table_name = "announcement_video_stage")]
 pub struct Model {
  #[sea_orm(primary_key, auto_increment = false)]
  pub id: Uuid,
  pub tenant_id: Uuid,
  pub created_by: Uuid,
  pub file_storage_id: Uuid,
  pub purpose: String,
  pub uploaded_at: Option<DateTimeUtc>,
  pub claimed_announcement_id: Option<Uuid>,
  pub expires_at: DateTimeUtc,
  pub created_at: DateTimeUtc,
 }
 impl ActiveModelBehavior for ActiveModel {}
 #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
 pub enum Relation {}
}
