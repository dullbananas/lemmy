use crate::{
  newtypes::LocalUserId,
  schema::email_verification::dsl::{
    email_verification,
    local_user_id,
    published,
    verification_token,
  },
  source::email_verification::{EmailVerification, EmailVerificationForm},
  utils::DbConn,
};
use diesel::{
  dsl::{now, IntervalDsl},
  insert_into,
  result::Error,
  ExpressionMethods,
  QueryDsl,
};
use diesel_async::RunQueryDsl;

impl EmailVerification {
  pub async fn create(mut conn: impl DbConn, form: &EmailVerificationForm) -> Result<Self, Error> {
    insert_into(email_verification)
      .values(form)
      .get_result::<Self>(&mut *conn)
      .await
  }

  pub async fn read_for_token(mut conn: impl DbConn, token: &str) -> Result<Self, Error> {
    email_verification
      .filter(verification_token.eq(token))
      .filter(published.gt(now - 7.days()))
      .first::<Self>(&mut *conn)
      .await
  }
  pub async fn delete_old_tokens_for_local_user(
    mut conn: impl DbConn,
    local_user_id_: LocalUserId,
  ) -> Result<usize, Error> {
    diesel::delete(email_verification.filter(local_user_id.eq(local_user_id_)))
      .execute(&mut *conn)
      .await
  }
}
