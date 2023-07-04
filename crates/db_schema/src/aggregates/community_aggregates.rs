use crate::{
  aggregates::structs::CommunityAggregates,
  newtypes::CommunityId,
  schema::community_aggregates,
  utils::DbConn,
};
use diesel::{result::Error, ExpressionMethods, QueryDsl};
use diesel_async::RunQueryDsl;

impl CommunityAggregates {
  pub async fn read(mut conn: impl DbConn, community_id: CommunityId) -> Result<Self, Error> {
    community_aggregates::table
      .filter(community_aggregates::community_id.eq(community_id))
      .first::<Self>(&mut *conn)
      .await
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    aggregates::community_aggregates::CommunityAggregates,
    source::{
      comment::{Comment, CommentInsertForm},
      community::{Community, CommunityFollower, CommunityFollowerForm, CommunityInsertForm},
      instance::Instance,
      person::{Person, PersonInsertForm},
      post::{Post, PostInsertForm},
    },
    traits::{Crud, Followable},
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
      .name("thommy_community_agg".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_person = Person::create(&mut *conn, &new_person).await.unwrap();

    let another_person = PersonInsertForm::builder()
      .name("jerry_community_agg".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let another_inserted_person = Person::create(&mut *conn, &another_person).await.unwrap();

    let new_community = CommunityInsertForm::builder()
      .name("TIL_community_agg".into())
      .title("nada".to_owned())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_community = Community::create(&mut *conn, &new_community).await.unwrap();

    let another_community = CommunityInsertForm::builder()
      .name("TIL_community_agg_2".into())
      .title("nada".to_owned())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let another_inserted_community = Community::create(&mut *conn, &another_community)
      .await
      .unwrap();

    let first_person_follow = CommunityFollowerForm {
      community_id: inserted_community.id,
      person_id: inserted_person.id,
      pending: false,
    };

    CommunityFollower::follow(&mut *conn, &first_person_follow)
      .await
      .unwrap();

    let second_person_follow = CommunityFollowerForm {
      community_id: inserted_community.id,
      person_id: another_inserted_person.id,
      pending: false,
    };

    CommunityFollower::follow(&mut *conn, &second_person_follow)
      .await
      .unwrap();

    let another_community_follow = CommunityFollowerForm {
      community_id: another_inserted_community.id,
      person_id: inserted_person.id,
      pending: false,
    };

    CommunityFollower::follow(&mut *conn, &another_community_follow)
      .await
      .unwrap();

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

    let child_comment_form = CommentInsertForm::builder()
      .content("A test comment".into())
      .creator_id(inserted_person.id)
      .post_id(inserted_post.id)
      .build();

    let _inserted_child_comment = Comment::create(
      &mut *conn,
      &child_comment_form,
      Some(&inserted_comment.path),
    )
    .await
    .unwrap();

    let community_aggregates_before_delete =
      CommunityAggregates::read(&mut *conn, inserted_community.id)
        .await
        .unwrap();

    assert_eq!(2, community_aggregates_before_delete.subscribers);
    assert_eq!(1, community_aggregates_before_delete.posts);
    assert_eq!(2, community_aggregates_before_delete.comments);

    // Test the other community
    let another_community_aggs =
      CommunityAggregates::read(&mut *conn, another_inserted_community.id)
        .await
        .unwrap();
    assert_eq!(1, another_community_aggs.subscribers);
    assert_eq!(0, another_community_aggs.posts);
    assert_eq!(0, another_community_aggs.comments);

    // Unfollow test
    CommunityFollower::unfollow(&mut *conn, &second_person_follow)
      .await
      .unwrap();
    let after_unfollow = CommunityAggregates::read(&mut *conn, inserted_community.id)
      .await
      .unwrap();
    assert_eq!(1, after_unfollow.subscribers);

    // Follow again just for the later tests
    CommunityFollower::follow(&mut *conn, &second_person_follow)
      .await
      .unwrap();
    let after_follow_again = CommunityAggregates::read(&mut *conn, inserted_community.id)
      .await
      .unwrap();
    assert_eq!(2, after_follow_again.subscribers);

    // Remove a parent comment (the comment count should also be 0)
    Post::delete(&mut *conn, inserted_post.id).await.unwrap();
    let after_parent_post_delete = CommunityAggregates::read(&mut *conn, inserted_community.id)
      .await
      .unwrap();
    assert_eq!(0, after_parent_post_delete.comments);
    assert_eq!(0, after_parent_post_delete.posts);

    // Remove the 2nd person
    Person::delete(&mut *conn, another_inserted_person.id)
      .await
      .unwrap();
    let after_person_delete = CommunityAggregates::read(&mut *conn, inserted_community.id)
      .await
      .unwrap();
    assert_eq!(1, after_person_delete.subscribers);

    // This should delete all the associated rows, and fire triggers
    let person_num_deleted = Person::delete(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    assert_eq!(1, person_num_deleted);

    // Delete the community
    let community_num_deleted = Community::delete(&mut *conn, inserted_community.id)
      .await
      .unwrap();
    assert_eq!(1, community_num_deleted);

    let another_community_num_deleted =
      Community::delete(&mut *conn, another_inserted_community.id)
        .await
        .unwrap();
    assert_eq!(1, another_community_num_deleted);

    // Should be none found, since the creator was deleted
    let after_delete = CommunityAggregates::read(&mut *conn, inserted_community.id).await;
    assert!(after_delete.is_err());
  }
}
