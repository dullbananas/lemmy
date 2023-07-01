use crate::{
  api::PerformApub,
  fetcher::search::{search_query_to_object_id, SearchableObjects},
};
use activitypub_federation::config::Data;
use diesel::NotFound;
use lemmy_api_common::{
  context::LemmyContext,
  site::{ResolveObject, ResolveObjectResponse},
  utils::{check_private_instance, local_user_view_from_jwt},
};
use lemmy_db_schema::{newtypes::PersonId, source::local_site::LocalSite, utils::DbConn};
use lemmy_db_views::structs::{CommentView, PostView};
use lemmy_db_views_actor::structs::{CommunityView, PersonView};
use lemmy_utils::error::LemmyError;

#[async_trait::async_trait]
impl PerformApub for ResolveObject {
  type Response = ResolveObjectResponse;

  #[tracing::instrument(skip(context))]
  async fn perform(
    &self,
    context: &Data<LemmyContext>,
  ) -> Result<ResolveObjectResponse, LemmyError> {
    let local_user_view = local_user_view_from_jwt(&self.auth, context).await?;
    let local_site = LocalSite::read(context.conn().await?).await?;
    let person_id = local_user_view.person.id;
    check_private_instance(&Some(local_user_view), &local_site)?;

    let res = search_query_to_object_id(&self.q, context)
      .await
      .map_err(|e| e.with_message("couldnt_find_object"))?;
    convert_response(res, person_id, context.conn().await?)
      .await
      .map_err(|e| e.with_message("couldnt_find_object"))
  }
}

async fn convert_response(
  object: SearchableObjects,
  user_id: PersonId,
  mut conn: impl DbConn,
) -> Result<ResolveObjectResponse, LemmyError> {
  use SearchableObjects::*;
  let removed_or_deleted;
  let mut res = ResolveObjectResponse::default();
  match object {
    Person(p) => {
      removed_or_deleted = p.deleted;
      res.person = Some(PersonView::read(&mut *conn, p.id).await?)
    }
    Community(c) => {
      removed_or_deleted = c.deleted || c.removed;
      res.community = Some(CommunityView::read(&mut *conn, c.id, Some(user_id), None).await?)
    }
    Post(p) => {
      removed_or_deleted = p.deleted || p.removed;
      res.post = Some(PostView::read(&mut *conn, p.id, Some(user_id), None).await?)
    }
    Comment(c) => {
      removed_or_deleted = c.deleted || c.removed;
      res.comment = Some(CommentView::read(&mut *conn, c.id, Some(user_id)).await?)
    }
  };
  // if the object was deleted from database, dont return it
  if removed_or_deleted {
    return Err(NotFound {}.into());
  }
  Ok(res)
}
