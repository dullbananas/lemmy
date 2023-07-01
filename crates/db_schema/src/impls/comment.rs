use crate::{
  newtypes::{CommentId, DbUrl, PersonId},
  schema::comment::dsl::{ap_id, comment, content, creator_id, deleted, path, removed, updated},
  source::comment::{
    Comment,
    CommentInsertForm,
    CommentLike,
    CommentLikeForm,
    CommentSaved,
    CommentSavedForm,
    CommentUpdateForm,
  },
  traits::{Crud, Likeable, Saveable},
  utils::{naive_now, DbConn, DELETED_REPLACEMENT_TEXT},
};
use diesel::{
  dsl::{insert_into, sql_query},
  result::Error,
  ExpressionMethods,
  QueryDsl,
};
use diesel_async::RunQueryDsl;
use diesel_ltree::Ltree;
use url::Url;

impl Comment {
  pub async fn permadelete_for_creator(
    mut conn: impl DbConn,
    for_creator_id: PersonId,
  ) -> Result<Vec<Self>, Error> {
    diesel::update(comment.filter(creator_id.eq(for_creator_id)))
      .set((
        content.eq(DELETED_REPLACEMENT_TEXT),
        deleted.eq(true),
        updated.eq(naive_now()),
      ))
      .get_results::<Self>(&mut *conn)
      .await
  }

  pub async fn update_removed_for_creator(
    mut conn: impl DbConn,
    for_creator_id: PersonId,
    new_removed: bool,
  ) -> Result<Vec<Self>, Error> {
    diesel::update(comment.filter(creator_id.eq(for_creator_id)))
      .set((removed.eq(new_removed), updated.eq(naive_now())))
      .get_results::<Self>(&mut *conn)
      .await
  }

  pub async fn create(
    mut conn: impl DbConn,
    comment_form: &CommentInsertForm,
    parent_path: Option<&Ltree>,
  ) -> Result<Comment, Error> {
    // Insert, to get the id
    let inserted_comment = insert_into(comment)
      .values(comment_form)
      .on_conflict(ap_id)
      .do_update()
      .set(comment_form)
      .get_result::<Self>(&mut *conn)
      .await;

    if let Ok(comment_insert) = inserted_comment {
      let comment_id = comment_insert.id;

      // You need to update the ltree column
      let ltree = Ltree(if let Some(parent_path) = parent_path {
        // The previous parent will already have 0 in it
        // Append this comment id
        format!("{}.{}", parent_path.0, comment_id)
      } else {
        // '0' is always the first path, append to that
        format!("{}.{}", 0, comment_id)
      });

      let updated_comment = diesel::update(comment.find(comment_id))
        .set(path.eq(ltree))
        .get_result::<Self>(&mut *conn)
        .await;

      // Update the child count for the parent comment_aggregates
      // You could do this with a trigger, but since you have to do this manually anyway,
      // you can just have it here
      if let Some(parent_path) = parent_path {
        // You have to update counts for all parents, not just the immediate one
        // TODO if the performance of this is terrible, it might be better to do this as part of a
        // scheduled query... although the counts would often be wrong.
        //
        // The child_count query for reference:
        // select c.id, c.path, count(c2.id) as child_count from comment c
        // left join comment c2 on c2.path <@ c.path and c2.path != c.path
        // group by c.id

        let parent_id = parent_path.0.split('.').nth(1);

        if let Some(parent_id) = parent_id {
          let top_parent = format!("0.{}", parent_id);
          let update_child_count_stmt = format!(
            "
update comment_aggregates ca set child_count = c.child_count
from (
  select c.id, c.path, count(c2.id) as child_count from comment c
  join comment c2 on c2.path <@ c.path and c2.path != c.path
  and c.path <@ '{top_parent}'
  group by c.id
) as c
where ca.comment_id = c.id"
          );

          sql_query(update_child_count_stmt)
            .execute(&mut *conn)
            .await?;
        }
      }
      updated_comment
    } else {
      inserted_comment
    }
  }
  pub async fn read_from_apub_id(
    mut conn: impl DbConn,
    object_id: Url,
  ) -> Result<Option<Self>, Error> {
    let object_id: DbUrl = object_id.into();
    Ok(
      comment
        .filter(ap_id.eq(object_id))
        .first::<Comment>(&mut *conn)
        .await
        .ok()
        .map(Into::into),
    )
  }

  pub fn parent_comment_id(&self) -> Option<CommentId> {
    let mut ltree_split: Vec<&str> = self.path.0.split('.').collect();
    ltree_split.remove(0); // The first is always 0
    if ltree_split.len() > 1 {
      let parent_comment_id = ltree_split.get(ltree_split.len() - 2);
      parent_comment_id.and_then(|p| p.parse::<i32>().map(CommentId).ok())
    } else {
      None
    }
  }
}

#[async_trait]
impl Crud for Comment {
  type InsertForm = CommentInsertForm;
  type UpdateForm = CommentUpdateForm;
  type IdType = CommentId;
  async fn read(mut conn: impl DbConn, comment_id: CommentId) -> Result<Self, Error> {
    comment.find(comment_id).first::<Self>(&mut *conn).await
  }

  async fn delete(mut conn: impl DbConn, comment_id: CommentId) -> Result<usize, Error> {
    diesel::delete(comment.find(comment_id))
      .execute(&mut *conn)
      .await
  }

  /// This is unimplemented, use [[Comment::create]]
  async fn create(_conn: impl DbConn, _comment_form: &Self::InsertForm) -> Result<Self, Error> {
    unimplemented!();
  }

  async fn update(
    mut conn: impl DbConn,
    comment_id: CommentId,
    comment_form: &Self::UpdateForm,
  ) -> Result<Self, Error> {
    diesel::update(comment.find(comment_id))
      .set(comment_form)
      .get_result::<Self>(&mut *conn)
      .await
  }
}

#[async_trait]
impl Likeable for CommentLike {
  type Form = CommentLikeForm;
  type IdType = CommentId;
  async fn like(mut conn: impl DbConn, comment_like_form: &CommentLikeForm) -> Result<Self, Error> {
    use crate::schema::comment_like::dsl::{comment_id, comment_like, person_id};
    insert_into(comment_like)
      .values(comment_like_form)
      .on_conflict((comment_id, person_id))
      .do_update()
      .set(comment_like_form)
      .get_result::<Self>(&mut *conn)
      .await
  }
  async fn remove(
    mut conn: impl DbConn,
    person_id_: PersonId,
    comment_id_: CommentId,
  ) -> Result<usize, Error> {
    use crate::schema::comment_like::dsl::{comment_id, comment_like, person_id};
    diesel::delete(
      comment_like
        .filter(comment_id.eq(comment_id_))
        .filter(person_id.eq(person_id_)),
    )
    .execute(&mut *conn)
    .await
  }
}

#[async_trait]
impl Saveable for CommentSaved {
  type Form = CommentSavedForm;
  async fn save(
    mut conn: impl DbConn,
    comment_saved_form: &CommentSavedForm,
  ) -> Result<Self, Error> {
    use crate::schema::comment_saved::dsl::{comment_id, comment_saved, person_id};
    insert_into(comment_saved)
      .values(comment_saved_form)
      .on_conflict((comment_id, person_id))
      .do_update()
      .set(comment_saved_form)
      .get_result::<Self>(&mut *conn)
      .await
  }
  async fn unsave(
    mut conn: impl DbConn,
    comment_saved_form: &CommentSavedForm,
  ) -> Result<usize, Error> {
    use crate::schema::comment_saved::dsl::{comment_id, comment_saved, person_id};
    diesel::delete(
      comment_saved
        .filter(comment_id.eq(comment_saved_form.comment_id))
        .filter(person_id.eq(comment_saved_form.person_id)),
    )
    .execute(&mut *conn)
    .await
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    newtypes::LanguageId,
    source::{
      comment::{
        Comment,
        CommentInsertForm,
        CommentLike,
        CommentLikeForm,
        CommentSaved,
        CommentSavedForm,
        CommentUpdateForm,
      },
      community::{Community, CommunityInsertForm},
      instance::Instance,
      person::{Person, PersonInsertForm},
      post::{Post, PostInsertForm},
    },
    traits::{Crud, Likeable, Saveable},
    utils::build_db_conn_for_tests,
  };
  use diesel_ltree::Ltree;
  use serial_test::serial;

  #[tokio::test]
  #[serial]
  async fn test_crud() {
    let mut conn = build_db_conn_for_tests().await;

    let inserted_instance = Instance::read_or_create(&mut *conn, "my_domain.tld".to_string())
      .await
      .unwrap();

    let new_person = PersonInsertForm::builder()
      .name("terry".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_person = Person::create(&mut *conn, &new_person).await.unwrap();

    let new_community = CommunityInsertForm::builder()
      .name("test community".to_string())
      .title("nada".to_owned())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_community = Community::create(&mut *conn, &new_community).await.unwrap();

    let new_post = PostInsertForm::builder()
      .name("A test post".into())
      .creator_id(inserted_person.id)
      .community_id(inserted_community.id)
      .build();

    let inserted_post = Post::create(&mut *conn, &new_post).await.unwrap();

    let comment_form = CommentInsertForm::builder()
      .content("A test comment".into())
      .creator_id(inserted_person.id)
      .post_id(inserted_post.id)
      .build();

    let inserted_comment = Comment::create(&mut *conn, &comment_form, None)
      .await
      .unwrap();

    let expected_comment = Comment {
      id: inserted_comment.id,
      content: "A test comment".into(),
      creator_id: inserted_person.id,
      post_id: inserted_post.id,
      removed: false,
      deleted: false,
      path: Ltree(format!("0.{}", inserted_comment.id)),
      published: inserted_comment.published,
      updated: None,
      ap_id: inserted_comment.ap_id.clone(),
      distinguished: false,
      local: true,
      language_id: LanguageId::default(),
    };

    let child_comment_form = CommentInsertForm::builder()
      .content("A child comment".into())
      .creator_id(inserted_person.id)
      .post_id(inserted_post.id)
      .build();

    let inserted_child_comment = Comment::create(
      &mut *conn,
      &child_comment_form,
      Some(&inserted_comment.path),
    )
    .await
    .unwrap();

    // Comment Like
    let comment_like_form = CommentLikeForm {
      comment_id: inserted_comment.id,
      post_id: inserted_post.id,
      person_id: inserted_person.id,
      score: 1,
    };

    let inserted_comment_like = CommentLike::like(&mut *conn, &comment_like_form)
      .await
      .unwrap();

    let expected_comment_like = CommentLike {
      id: inserted_comment_like.id,
      comment_id: inserted_comment.id,
      post_id: inserted_post.id,
      person_id: inserted_person.id,
      published: inserted_comment_like.published,
      score: 1,
    };

    // Comment Saved
    let comment_saved_form = CommentSavedForm {
      comment_id: inserted_comment.id,
      person_id: inserted_person.id,
    };

    let inserted_comment_saved = CommentSaved::save(&mut *conn, &comment_saved_form)
      .await
      .unwrap();

    let expected_comment_saved = CommentSaved {
      id: inserted_comment_saved.id,
      comment_id: inserted_comment.id,
      person_id: inserted_person.id,
      published: inserted_comment_saved.published,
    };

    let comment_update_form = CommentUpdateForm::builder()
      .content(Some("A test comment".into()))
      .build();

    let updated_comment = Comment::update(&mut *conn, inserted_comment.id, &comment_update_form)
      .await
      .unwrap();

    let read_comment = Comment::read(&mut *conn, inserted_comment.id)
      .await
      .unwrap();
    let like_removed = CommentLike::remove(&mut *conn, inserted_person.id, inserted_comment.id)
      .await
      .unwrap();
    let saved_removed = CommentSaved::unsave(&mut *conn, &comment_saved_form)
      .await
      .unwrap();
    let num_deleted = Comment::delete(&mut *conn, inserted_comment.id)
      .await
      .unwrap();
    Comment::delete(&mut *conn, inserted_child_comment.id)
      .await
      .unwrap();
    Post::delete(&mut *conn, inserted_post.id).await.unwrap();
    Community::delete(&mut *conn, inserted_community.id)
      .await
      .unwrap();
    Person::delete(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    Instance::delete(&mut *conn, inserted_instance.id)
      .await
      .unwrap();

    assert_eq!(expected_comment, read_comment);
    assert_eq!(expected_comment, inserted_comment);
    assert_eq!(expected_comment, updated_comment);
    assert_eq!(expected_comment_like, inserted_comment_like);
    assert_eq!(expected_comment_saved, inserted_comment_saved);
    assert_eq!(
      format!("0.{}.{}", expected_comment.id, inserted_child_comment.id),
      inserted_child_comment.path.0,
    );
    assert_eq!(1, like_removed);
    assert_eq!(1, saved_removed);
    assert_eq!(1, num_deleted);
  }
}
