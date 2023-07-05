use crate::{
  newtypes::LocalUserId,
  schema::local_user::dsl::{
    accepted_application,
    email,
    email_verified,
    local_user,
    password_encrypted,
    validator_time,
  },
  source::{
    actor_language::{LocalUserLanguage, SiteLanguage},
    local_user::{LocalUser, LocalUserInsertForm, LocalUserUpdateForm},
  },
  traits::Crud,
  utils::{naive_now, DbPool, GetConn},
};
use bcrypt::{hash, DEFAULT_COST};
use diesel::{dsl::insert_into, result::Error, ExpressionMethods, QueryDsl};
use diesel_async::RunQueryDsl;

impl LocalUser {
  pub async fn update_password(
    mut pool: &mut impl GetConn,
    local_user_id: LocalUserId,
    new_password: &str,
  ) -> Result<Self, Error> {
    let conn = &mut *pool.get_conn().await?;
    let password_hash = hash(new_password, DEFAULT_COST).expect("Couldn't hash password");

    diesel::update(local_user.find(local_user_id))
      .set((
        password_encrypted.eq(password_hash),
        validator_time.eq(naive_now()),
      ))
      .get_result::<Self>(conn)
      .await
  }

  pub async fn set_all_users_email_verified(
    mut pool: &mut impl GetConn,
  ) -> Result<Vec<Self>, Error> {
    let conn = &mut *pool.get_conn().await?;
    diesel::update(local_user)
      .set(email_verified.eq(true))
      .get_results::<Self>(conn)
      .await
  }

  pub async fn set_all_users_registration_applications_accepted(
    mut pool: &mut impl GetConn,
  ) -> Result<Vec<Self>, Error> {
    let conn = &mut *pool.get_conn().await?;
    diesel::update(local_user)
      .set(accepted_application.eq(true))
      .get_results::<Self>(conn)
      .await
  }

  pub async fn is_email_taken(mut pool: &mut impl GetConn, email_: &str) -> Result<bool, Error> {
    use diesel::dsl::{exists, select};
    let conn = &mut *pool.get_conn().await?;
    select(exists(local_user.filter(email.eq(email_))))
      .get_result(conn)
      .await
  }
}

#[async_trait]
impl Crud for LocalUser {
  type InsertForm = LocalUserInsertForm;
  type UpdateForm = LocalUserUpdateForm;
  type IdType = LocalUserId;
  async fn read(mut pool: &mut impl GetConn, local_user_id: LocalUserId) -> Result<Self, Error> {
    let conn = &mut *pool.get_conn().await?;
    local_user.find(local_user_id).first::<Self>(conn).await
  }
  async fn delete(mut pool: &mut impl GetConn, local_user_id: LocalUserId) -> Result<usize, Error> {
    let conn = &mut *pool.get_conn().await?;
    diesel::delete(local_user.find(local_user_id))
      .execute(conn)
      .await
  }
  async fn create(mut pool: &mut impl GetConn, form: &Self::InsertForm) -> Result<Self, Error> {
    let conn = &mut *pool.get_conn().await?;
    let mut form_with_encrypted_password = form.clone();
    let password_hash =
      hash(&form.password_encrypted, DEFAULT_COST).expect("Couldn't hash password");
    form_with_encrypted_password.password_encrypted = password_hash;

    let local_user_ = insert_into(local_user)
      .values(form_with_encrypted_password)
      .get_result::<Self>(conn)
      .await?;

    let site_languages = SiteLanguage::read_local_raw(pool).await;
    if let Ok(langs) = site_languages {
      // if site exists, init user with site languages
      LocalUserLanguage::update(pool, langs, local_user_.id).await?;
    } else {
      // otherwise, init with all languages (this only happens during tests and
      // for first admin user, which is created before site)
      LocalUserLanguage::update(pool, vec![], local_user_.id).await?;
    }

    Ok(local_user_)
  }
  async fn update(
    mut pool: &mut impl GetConn,
    local_user_id: LocalUserId,
    form: &Self::UpdateForm,
  ) -> Result<Self, Error> {
    let conn = &mut *pool.get_conn().await?;
    diesel::update(local_user.find(local_user_id))
      .set(form)
      .get_result::<Self>(conn)
      .await
  }
}
