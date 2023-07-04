use crate::{
  aggregates::structs::PersonAggregates,
  newtypes::PersonId,
  schema::person_aggregates,
  utils::DbConn,
};
use diesel::{result::Error, ExpressionMethods, QueryDsl};
use diesel_async::RunQueryDsl;

impl PersonAggregates {
  pub async fn read(mut conn: impl DbConn, person_id: PersonId) -> Result<Self, Error> {
    person_aggregates::table
      .filter(person_aggregates::person_id.eq(person_id))
      .first::<Self>(&mut *conn)
      .await
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    aggregates::person_aggregates::PersonAggregates,
    source::{
      comment::{Comment, CommentInsertForm, CommentLike, CommentLikeForm, CommentUpdateForm},
      community::{Community, CommunityInsertForm},
      instance::Instance,
      person::{Person, PersonInsertForm},
      post::{Post, PostInsertForm, PostLike, PostLikeForm},
    },
    traits::{Crud, Likeable},
    utils::build_db_conn_for_tests,
  };
  use serial_test::serial;

  #[tokio::test]
  #[serial]
  async fn test_crud() {
    let mut conn = build_db_conn_for_tests().await;

    let inserted_instance = Instance::read_or_create(&mut *conn, "my_domain.tld".to_string())
      .await
      .unwrap();

    let new_person = PersonInsertForm::builder()
      .name("thommy_user_agg".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_person = Person::create(&mut *conn, &new_person).await.unwrap();

    let another_person = PersonInsertForm::builder()
      .name("jerry_user_agg".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let another_inserted_person = Person::create(&mut *conn, &another_person).await.unwrap();

    let new_community = CommunityInsertForm::builder()
      .name("TIL_site_agg".into())
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

    let post_like = PostLikeForm {
      post_id: inserted_post.id,
      person_id: inserted_person.id,
      score: 1,
    };

    let _inserted_post_like = PostLike::like(&mut *conn, &post_like).await.unwrap();

    let comment_form = CommentInsertForm::builder()
      .content("A test comment".into())
      .creator_id(inserted_person.id)
      .post_id(inserted_post.id)
      .build();

    let inserted_comment = Comment::create(&mut *conn, &comment_form, None)
      .await
      .unwrap();

    let mut comment_like = CommentLikeForm {
      comment_id: inserted_comment.id,
      person_id: inserted_person.id,
      post_id: inserted_post.id,
      score: 1,
    };

    let _inserted_comment_like = CommentLike::like(&mut *conn, &comment_like).await.unwrap();

    let child_comment_form = CommentInsertForm::builder()
      .content("A test comment".into())
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

    let child_comment_like = CommentLikeForm {
      comment_id: inserted_child_comment.id,
      person_id: another_inserted_person.id,
      post_id: inserted_post.id,
      score: 1,
    };

    let _inserted_child_comment_like = CommentLike::like(&mut *conn, &child_comment_like)
      .await
      .unwrap();

    let person_aggregates_before_delete = PersonAggregates::read(&mut *conn, inserted_person.id)
      .await
      .unwrap();

    assert_eq!(1, person_aggregates_before_delete.post_count);
    assert_eq!(1, person_aggregates_before_delete.post_score);
    assert_eq!(2, person_aggregates_before_delete.comment_count);
    assert_eq!(2, person_aggregates_before_delete.comment_score);

    // Remove a post like
    PostLike::remove(&mut *conn, inserted_person.id, inserted_post.id)
      .await
      .unwrap();
    let after_post_like_remove = PersonAggregates::read(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    assert_eq!(0, after_post_like_remove.post_score);

    Comment::update(
      &mut *conn,
      inserted_comment.id,
      &CommentUpdateForm::builder().removed(Some(true)).build(),
    )
    .await
    .unwrap();
    Comment::update(
      &mut *conn,
      inserted_child_comment.id,
      &CommentUpdateForm::builder().removed(Some(true)).build(),
    )
    .await
    .unwrap();

    let after_parent_comment_removed = PersonAggregates::read(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    assert_eq!(0, after_parent_comment_removed.comment_count);
    assert_eq!(0, after_parent_comment_removed.comment_score);

    // Remove a parent comment (the scores should also be removed)
    Comment::delete(&mut *conn, inserted_comment.id)
      .await
      .unwrap();
    Comment::delete(&mut *conn, inserted_child_comment.id)
      .await
      .unwrap();
    let after_parent_comment_delete = PersonAggregates::read(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    assert_eq!(0, after_parent_comment_delete.comment_count);
    assert_eq!(0, after_parent_comment_delete.comment_score);

    // Add in the two comments again, then delete the post.
    let new_parent_comment = Comment::create(&mut *conn, &comment_form, None)
      .await
      .unwrap();
    let _new_child_comment = Comment::create(
      &mut *conn,
      &child_comment_form,
      Some(&new_parent_comment.path),
    )
    .await
    .unwrap();
    comment_like.comment_id = new_parent_comment.id;
    CommentLike::like(&mut *conn, &comment_like).await.unwrap();
    let after_comment_add = PersonAggregates::read(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    assert_eq!(2, after_comment_add.comment_count);
    assert_eq!(1, after_comment_add.comment_score);

    Post::delete(&mut *conn, inserted_post.id).await.unwrap();
    let after_post_delete = PersonAggregates::read(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    assert_eq!(0, after_post_delete.comment_score);
    assert_eq!(0, after_post_delete.comment_count);
    assert_eq!(0, after_post_delete.post_score);
    assert_eq!(0, after_post_delete.post_count);

    // This should delete all the associated rows, and fire triggers
    let person_num_deleted = Person::delete(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    assert_eq!(1, person_num_deleted);
    Person::delete(&mut *conn, another_inserted_person.id)
      .await
      .unwrap();

    // Delete the community
    let community_num_deleted = Community::delete(&mut *conn, inserted_community.id)
      .await
      .unwrap();
    assert_eq!(1, community_num_deleted);

    // Should be none found
    let after_delete = PersonAggregates::read(&mut *conn, inserted_person.id).await;
    assert!(after_delete.is_err());

    Instance::delete(&mut *conn, inserted_instance.id)
      .await
      .unwrap();
  }
}
