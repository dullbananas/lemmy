use crate::{
  newtypes::LocalUserId,
  schema::registration_application::dsl::{local_user_id, registration_application},
  source::registration_application::{
    RegistrationApplication,
    RegistrationApplicationInsertForm,
    RegistrationApplicationUpdateForm,
  },
  traits::Crud,
  utils::{DbPool, DbPoolRef, RunQueryDsl},
};
use diesel::{insert_into, result::Error, ExpressionMethods, QueryDsl};

#[async_trait]
impl Crud for RegistrationApplication {
  type InsertForm = RegistrationApplicationInsertForm;
  type UpdateForm = RegistrationApplicationUpdateForm;
  type IdType = i32;

  async fn create(pool: DbPoolRef<'_>, form: &Self::InsertForm) -> Result<Self, Error> {
    let conn = pool;
    insert_into(registration_application)
      .values(form)
      .get_result::<Self>(conn)
      .await
  }

  async fn read(pool: DbPoolRef<'_>, id_: Self::IdType) -> Result<Self, Error> {
    let conn = pool;
    registration_application.find(id_).first::<Self>(conn).await
  }

  async fn update(
    pool: DbPoolRef<'_>,
    id_: Self::IdType,
    form: &Self::UpdateForm,
  ) -> Result<Self, Error> {
    let conn = pool;
    diesel::update(registration_application.find(id_))
      .set(form)
      .get_result::<Self>(conn)
      .await
  }

  async fn delete(pool: DbPoolRef<'_>, id_: Self::IdType) -> Result<usize, Error> {
    let conn = pool;
    diesel::delete(registration_application.find(id_))
      .execute(conn)
      .await
  }
}

impl RegistrationApplication {
  pub async fn find_by_local_user_id(
    pool: DbPoolRef<'_>,
    local_user_id_: LocalUserId,
  ) -> Result<Self, Error> {
    let conn = pool;
    registration_application
      .filter(local_user_id.eq(local_user_id_))
      .first::<Self>(conn)
      .await
  }
}
