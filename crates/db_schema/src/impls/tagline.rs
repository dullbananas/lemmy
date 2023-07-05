use crate::{
  newtypes::LocalSiteId,
  schema::tagline::dsl::{local_site_id, tagline},
  source::tagline::{Tagline, TaglineForm},
  utils::GetConn,
};
use diesel::{insert_into, result::Error, ExpressionMethods, QueryDsl};
use lemmy_db_schema::utils::RunQueryDsl;

impl Tagline {
  pub async fn replace(
    mut conn: impl GetConn,
    for_local_site_id: LocalSiteId,
    list_content: Option<Vec<String>>,
  ) -> Result<Vec<Self>, Error> {
    if let Some(list) = list_content {
      conn
        .build_transaction()
        .run(|conn| {
          Box::pin(async move {
            Self::clear(conn).await?;

            for item in list {
              let form = TaglineForm {
                local_site_id: for_local_site_id,
                content: item,
                updated: None,
              };
              insert_into(tagline)
                .values(form)
                .get_result::<Self>(conn)
                .await?;
            }
            Self::get_all_conn(conn, for_local_site_id).await
          }) as _
        })
        .await
    } else {
      Self::get_all_conn(conn, for_local_site_id).await
    }
  }

  async fn clear(mut conn: impl GetConn) -> Result<usize, Error> {
    diesel::delete(tagline).execute(conn).await
  }

  async fn get_all_conn(
    mut conn: impl GetConn,
    for_local_site_id: LocalSiteId,
  ) -> Result<Vec<Self>, Error> {
    tagline
      .filter(local_site_id.eq(for_local_site_id))
      .get_results::<Self>(conn)
      .await
  }
  pub async fn get_all(
    mut conn: impl GetConn,
    for_local_site_id: LocalSiteId,
  ) -> Result<Vec<Self>, Error> {
    Self::get_all_conn(conn, for_local_site_id).await
  }
}
