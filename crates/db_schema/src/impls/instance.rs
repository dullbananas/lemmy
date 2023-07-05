use crate::{
  newtypes::InstanceId,
  schema::{federation_allowlist, federation_blocklist, instance},
  source::instance::{Instance, InstanceForm},
  utils::{naive_now, DbPool, DbPoolRef},
};
use diesel::{dsl::insert_into, result::Error, ExpressionMethods, QueryDsl};
use diesel_async::AsyncPgConnection;

impl Instance {
  pub(crate) async fn read_or_create_with_conn(
    conn: DbPoolRef<'_>,
    domain_: String,
  ) -> Result<Self, Error> {
    use crate::schema::instance::domain;
    // First try to read the instance row and return directly if found
    let instance = instance::table
      .filter(domain.eq(&domain_))
      .first::<Self>(conn)
      .await;
    match instance {
      Ok(i) => Ok(i),
      Err(diesel::NotFound) => {
        // Instance not in database yet, insert it
        let form = InstanceForm::builder()
          .domain(domain_)
          .updated(Some(naive_now()))
          .build();
        insert_into(instance::table)
          .values(&form)
          // Necessary because this method may be called concurrently for the same domain. This
          // could be handled with a transaction, but nested transactions arent allowed
          .on_conflict(instance::domain)
          .do_update()
          .set(&form)
          .get_result::<Self>(conn)
          .await
      }
      e => e,
    }
  }

  /// Attempt to read Instance column for the given domain. If it doesnt exist, insert a new one.
  /// There is no need for update as the domain of an existing instance cant change.
  pub async fn read_or_create(pool: DbPoolRef<'_>, domain: String) -> Result<Self, Error> {
    let conn = pool;
    Self::read_or_create_with_conn(conn, domain).await
  }
  pub async fn delete(pool: DbPoolRef<'_>, instance_id: InstanceId) -> Result<usize, Error> {
    let conn = pool;
    diesel::delete(instance::table.find(instance_id))
      .execute(conn)
      .await
  }
  #[cfg(test)]
  pub async fn delete_all(pool: DbPoolRef<'_>) -> Result<usize, Error> {
    let conn = pool;
    diesel::delete(instance::table).execute(conn).await
  }
  pub async fn allowlist(pool: DbPoolRef<'_>) -> Result<Vec<Self>, Error> {
    let conn = pool;
    instance::table
      .inner_join(federation_allowlist::table)
      .select(instance::all_columns)
      .get_results(conn)
      .await
  }

  pub async fn blocklist(pool: DbPoolRef<'_>) -> Result<Vec<Self>, Error> {
    let conn = pool;
    instance::table
      .inner_join(federation_blocklist::table)
      .select(instance::all_columns)
      .get_results(conn)
      .await
  }

  pub async fn linked(pool: DbPoolRef<'_>) -> Result<Vec<Self>, Error> {
    let conn = pool;
    instance::table
      .left_join(federation_blocklist::table)
      .filter(federation_blocklist::id.is_null())
      .select(instance::all_columns)
      .get_results(conn)
      .await
  }
}
