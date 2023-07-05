use crate::{
  newtypes::{DbUrl, SiteId},
  schema::site::dsl::{actor_id, id, site},
  source::{
    actor_language::SiteLanguage,
    site::{Site, SiteInsertForm, SiteUpdateForm},
  },
  traits::Crud,
  utils::GetConn,
};
use diesel::{dsl::insert_into, result::Error, ExpressionMethods, QueryDsl};
use lemmy_db_schema::utils::RunQueryDsl;
use url::Url;

#[async_trait]
impl Crud for Site {
  type InsertForm = SiteInsertForm;
  type UpdateForm = SiteUpdateForm;
  type IdType = SiteId;

  /// Use SiteView::read_local, or Site::read_from_apub_id instead
  async fn read(_conn: impl GetConn, _site_id: SiteId) -> Result<Self, Error> {
    unimplemented!()
  }

  async fn create(mut conn: impl GetConn, form: &Self::InsertForm) -> Result<Self, Error> {
    let is_new_site = match &form.actor_id {
      Some(id_) => Site::read_from_apub_id(conn, id_).await?.is_none(),
      None => true,
    };

    // Can't do separate insert/update commands because InsertForm/UpdateForm aren't convertible
    let site_ = insert_into(site)
      .values(form)
      .on_conflict(actor_id)
      .do_update()
      .set(form)
      .get_result::<Self>(conn)
      .await?;

    // initialize languages if site is newly created
    if is_new_site {
      // initialize with all languages
      SiteLanguage::update(conn, vec![], &site_).await?;
    }
    Ok(site_)
  }

  async fn update(
    mut conn: impl GetConn,
    site_id: SiteId,
    new_site: &Self::UpdateForm,
  ) -> Result<Self, Error> {
    diesel::update(site.find(site_id))
      .set(new_site)
      .get_result::<Self>(conn)
      .await
  }

  async fn delete(mut conn: impl GetConn, site_id: SiteId) -> Result<usize, Error> {
    diesel::delete(site.find(site_id)).execute(conn).await
  }
}

impl Site {
  pub async fn read_from_apub_id(
    mut conn: impl GetConn,
    object_id: &DbUrl,
  ) -> Result<Option<Self>, Error> {
    Ok(
      site
        .filter(actor_id.eq(object_id))
        .first::<Site>(conn)
        .await
        .ok()
        .map(Into::into),
    )
  }

  // TODO this needs fixed
  pub async fn read_remote_sites(mut conn: impl GetConn) -> Result<Vec<Self>, Error> {
    site
      .order_by(id)
      .offset(1)
      .get_results::<Self>(conn)
      .await
  }

  /// Instance actor is at the root path, so we simply need to clear the path and other unnecessary
  /// parts of the url.
  pub fn instance_actor_id_from_url(mut url: Url) -> Url {
    url.set_fragment(None);
    url.set_path("");
    url.set_query(None);
    url
  }
}
