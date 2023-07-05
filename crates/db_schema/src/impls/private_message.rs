use crate::{
  newtypes::{DbUrl, PersonId, PrivateMessageId},
  schema::private_message::dsl::{ap_id, private_message, read, recipient_id},
  source::private_message::{PrivateMessage, PrivateMessageInsertForm, PrivateMessageUpdateForm},
  traits::Crud,
  utils::GetConn,
};
use diesel::{dsl::insert_into, result::Error, ExpressionMethods, QueryDsl};
use lemmy_db_schema::utils::RunQueryDsl;
use lemmy_utils::error::LemmyError;
use url::Url;

#[async_trait]
impl Crud for PrivateMessage {
  type InsertForm = PrivateMessageInsertForm;
  type UpdateForm = PrivateMessageUpdateForm;
  type IdType = PrivateMessageId;
  async fn read(
    mut conn: impl GetConn,
    private_message_id: PrivateMessageId,
  ) -> Result<Self, Error> {
    private_message
      .find(private_message_id)
      .first::<Self>(conn)
      .await
  }

  async fn create(mut conn: impl GetConn, form: &Self::InsertForm) -> Result<Self, Error> {
    insert_into(private_message)
      .values(form)
      .on_conflict(ap_id)
      .do_update()
      .set(form)
      .get_result::<Self>(conn)
      .await
  }

  async fn update(
    mut conn: impl GetConn,
    private_message_id: PrivateMessageId,
    form: &Self::UpdateForm,
  ) -> Result<Self, Error> {
    diesel::update(private_message.find(private_message_id))
      .set(form)
      .get_result::<Self>(conn)
      .await
  }
  async fn delete(mut conn: impl GetConn, pm_id: Self::IdType) -> Result<usize, Error> {
    diesel::delete(private_message.find(pm_id))
      .execute(conn)
      .await
  }
}

impl PrivateMessage {
  pub async fn mark_all_as_read(
    mut conn: impl GetConn,
    for_recipient_id: PersonId,
  ) -> Result<Vec<PrivateMessage>, Error> {
    diesel::update(
      private_message
        .filter(recipient_id.eq(for_recipient_id))
        .filter(read.eq(false)),
    )
    .set(read.eq(true))
    .get_results::<Self>(conn)
    .await
  }

  pub async fn read_from_apub_id(
    mut conn: impl GetConn,
    object_id: Url,
  ) -> Result<Option<Self>, LemmyError> {
    let object_id: DbUrl = object_id.into();
    Ok(
      private_message
        .filter(ap_id.eq(object_id))
        .first::<PrivateMessage>(conn)
        .await
        .ok()
        .map(Into::into),
    )
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    source::{
      instance::Instance,
      person::{Person, PersonInsertForm},
      private_message::{PrivateMessage, PrivateMessageInsertForm, PrivateMessageUpdateForm},
    },
    traits::Crud,
    utils::build_db_conn_for_tests,
  };
  use serial_test::serial;

  #[tokio::test]
  #[serial]
  async fn test_crud() {
    let mut conn = build_db_conn_for_tests().await;

    let inserted_instance = Instance::read_or_create(conn, "my_domain.tld".to_string())
      .await
      .unwrap();

    let creator_form = PersonInsertForm::builder()
      .name("creator_pm".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_creator = Person::create(conn, &creator_form).await.unwrap();

    let recipient_form = PersonInsertForm::builder()
      .name("recipient_pm".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_recipient = Person::create(conn, &recipient_form).await.unwrap();

    let private_message_form = PrivateMessageInsertForm::builder()
      .content("A test private message".into())
      .creator_id(inserted_creator.id)
      .recipient_id(inserted_recipient.id)
      .build();

    let inserted_private_message = PrivateMessage::create(conn, &private_message_form)
      .await
      .unwrap();

    let expected_private_message = PrivateMessage {
      id: inserted_private_message.id,
      content: "A test private message".into(),
      creator_id: inserted_creator.id,
      recipient_id: inserted_recipient.id,
      deleted: false,
      read: false,
      updated: None,
      published: inserted_private_message.published,
      ap_id: inserted_private_message.ap_id.clone(),
      local: true,
    };

    let read_private_message = PrivateMessage::read(conn, inserted_private_message.id)
      .await
      .unwrap();

    let private_message_update_form = PrivateMessageUpdateForm::builder()
      .content(Some("A test private message".into()))
      .build();
    let updated_private_message = PrivateMessage::update(
      conn,
      inserted_private_message.id,
      &private_message_update_form,
    )
    .await
    .unwrap();

    let deleted_private_message = PrivateMessage::update(
      conn,
      inserted_private_message.id,
      &PrivateMessageUpdateForm::builder()
        .deleted(Some(true))
        .build(),
    )
    .await
    .unwrap();
    let marked_read_private_message = PrivateMessage::update(
      conn,
      inserted_private_message.id,
      &PrivateMessageUpdateForm::builder().read(Some(true)).build(),
    )
    .await
    .unwrap();
    Person::delete(conn, inserted_creator.id)
      .await
      .unwrap();
    Person::delete(conn, inserted_recipient.id)
      .await
      .unwrap();
    Instance::delete(conn, inserted_instance.id)
      .await
      .unwrap();

    assert_eq!(expected_private_message, read_private_message);
    assert_eq!(expected_private_message, updated_private_message);
    assert_eq!(expected_private_message, inserted_private_message);
    assert!(deleted_private_message.deleted);
    assert!(marked_read_private_message.read);
  }
}
