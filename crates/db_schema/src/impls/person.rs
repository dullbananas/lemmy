use crate::{
  newtypes::{CommunityId, DbUrl, PersonId},
  schema::{instance, local_user, person, person_follower},
  source::person::{
    Person,
    PersonFollower,
    PersonFollowerForm,
    PersonInsertForm,
    PersonUpdateForm,
  },
  traits::{ApubActor, Crud, Followable},
  utils::{functions::lower, naive_now, DbConn},
};
use diesel::{dsl::insert_into, result::Error, ExpressionMethods, JoinOnDsl, QueryDsl};
use diesel_async::RunQueryDsl;

#[async_trait]
impl Crud for Person {
  type InsertForm = PersonInsertForm;
  type UpdateForm = PersonUpdateForm;
  type IdType = PersonId;
  async fn read(mut conn: impl DbConn, person_id: PersonId) -> Result<Self, Error> {
    person::table
      .filter(person::deleted.eq(false))
      .find(person_id)
      .first::<Self>(&mut *conn)
      .await
  }
  async fn delete(mut conn: impl DbConn, person_id: PersonId) -> Result<usize, Error> {
    diesel::delete(person::table.find(person_id))
      .execute(&mut *conn)
      .await
  }
  async fn create(mut conn: impl DbConn, form: &PersonInsertForm) -> Result<Self, Error> {
    insert_into(person::table)
      .values(form)
      .get_result::<Self>(&mut *conn)
      .await
  }
  async fn update(
    mut conn: impl DbConn,
    person_id: PersonId,
    form: &PersonUpdateForm,
  ) -> Result<Self, Error> {
    diesel::update(person::table.find(person_id))
      .set(form)
      .get_result::<Self>(&mut *conn)
      .await
  }
}

impl Person {
  /// Update or insert the person.
  ///
  /// This is necessary for federation, because Activitypub doesnt distinguish between these actions.
  pub async fn upsert(mut conn: impl DbConn, form: &PersonInsertForm) -> Result<Self, Error> {
    insert_into(person::table)
      .values(form)
      .on_conflict(person::actor_id)
      .do_update()
      .set(form)
      .get_result::<Self>(&mut *conn)
      .await
  }
  pub async fn delete_account(mut conn: impl DbConn, person_id: PersonId) -> Result<Person, Error> {
    // Set the local user info to none
    diesel::update(local_user::table.filter(local_user::person_id.eq(person_id)))
      .set((
        local_user::email.eq::<Option<String>>(None),
        local_user::validator_time.eq(naive_now()),
      ))
      .execute(&mut *conn)
      .await?;

    diesel::update(person::table.find(person_id))
      .set((
        person::display_name.eq::<Option<String>>(None),
        person::avatar.eq::<Option<String>>(None),
        person::banner.eq::<Option<String>>(None),
        person::bio.eq::<Option<String>>(None),
        person::matrix_user_id.eq::<Option<String>>(None),
        person::deleted.eq(true),
        person::updated.eq(naive_now()),
      ))
      .get_result::<Self>(&mut *conn)
      .await
  }
}

pub fn is_banned(banned_: bool, expires: Option<chrono::NaiveDateTime>) -> bool {
  if let Some(expires) = expires {
    banned_ && expires.gt(&naive_now())
  } else {
    banned_
  }
}

#[async_trait]
impl ApubActor for Person {
  async fn read_from_apub_id(
    mut conn: impl DbConn,
    object_id: &DbUrl,
  ) -> Result<Option<Self>, Error> {
    Ok(
      person::table
        .filter(person::deleted.eq(false))
        .filter(person::actor_id.eq(object_id))
        .first::<Person>(&mut *conn)
        .await
        .ok()
        .map(Into::into),
    )
  }

  async fn read_from_name(
    mut conn: impl DbConn,
    from_name: &str,
    include_deleted: bool,
  ) -> Result<Person, Error> {
    let mut q = person::table
      .into_boxed()
      .filter(person::local.eq(true))
      .filter(lower(person::name).eq(from_name.to_lowercase()));
    if !include_deleted {
      q = q.filter(person::deleted.eq(false))
    }
    q.first::<Self>(&mut *conn).await
  }

  async fn read_from_name_and_domain(
    mut conn: impl DbConn,
    person_name: &str,
    for_domain: &str,
  ) -> Result<Person, Error> {
    person::table
      .inner_join(instance::table)
      .filter(lower(person::name).eq(person_name.to_lowercase()))
      .filter(instance::domain.eq(for_domain))
      .select(person::all_columns)
      .first::<Self>(&mut *conn)
      .await
  }
}

#[async_trait]
impl Followable for PersonFollower {
  type Form = PersonFollowerForm;
  async fn follow(mut conn: impl DbConn, form: &PersonFollowerForm) -> Result<Self, Error> {
    use crate::schema::person_follower::dsl::{follower_id, person_follower, person_id};
    insert_into(person_follower)
      .values(form)
      .on_conflict((follower_id, person_id))
      .do_update()
      .set(form)
      .get_result::<Self>(&mut *conn)
      .await
  }
  async fn follow_accepted(_: impl DbConn, _: CommunityId, _: PersonId) -> Result<Self, Error> {
    unimplemented!()
  }
  async fn unfollow(mut conn: impl DbConn, form: &PersonFollowerForm) -> Result<usize, Error> {
    use crate::schema::person_follower::dsl::{follower_id, person_follower, person_id};
    diesel::delete(
      person_follower
        .filter(follower_id.eq(&form.follower_id))
        .filter(person_id.eq(&form.person_id)),
    )
    .execute(&mut *conn)
    .await
  }
}

impl PersonFollower {
  pub async fn list_followers(
    mut conn: impl DbConn,
    for_person_id: PersonId,
  ) -> Result<Vec<Person>, Error> {
    person_follower::table
      .inner_join(person::table.on(person_follower::follower_id.eq(person::id)))
      .filter(person_follower::person_id.eq(for_person_id))
      .select(person::all_columns)
      .load(&mut *conn)
      .await
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    source::{
      instance::Instance,
      person::{Person, PersonFollower, PersonFollowerForm, PersonInsertForm, PersonUpdateForm},
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
      .name("holly".into())
      .public_key("nada".to_owned())
      .instance_id(inserted_instance.id)
      .build();

    let inserted_person = Person::create(&mut *conn, &new_person).await.unwrap();

    let expected_person = Person {
      id: inserted_person.id,
      name: "holly".into(),
      display_name: None,
      avatar: None,
      banner: None,
      banned: false,
      deleted: false,
      published: inserted_person.published,
      updated: None,
      actor_id: inserted_person.actor_id.clone(),
      bio: None,
      local: true,
      bot_account: false,
      admin: false,
      private_key: None,
      public_key: "nada".to_owned(),
      last_refreshed_at: inserted_person.published,
      inbox_url: inserted_person.inbox_url.clone(),
      shared_inbox_url: None,
      matrix_user_id: None,
      ban_expires: None,
      instance_id: inserted_instance.id,
    };

    let read_person = Person::read(&mut *conn, inserted_person.id).await.unwrap();

    let update_person_form = PersonUpdateForm::builder()
      .actor_id(Some(inserted_person.actor_id.clone()))
      .build();
    let updated_person = Person::update(&mut *conn, inserted_person.id, &update_person_form)
      .await
      .unwrap();

    let num_deleted = Person::delete(&mut *conn, inserted_person.id)
      .await
      .unwrap();
    Instance::delete(&mut *conn, inserted_instance.id)
      .await
      .unwrap();

    assert_eq!(expected_person, read_person);
    assert_eq!(expected_person, inserted_person);
    assert_eq!(expected_person, updated_person);
    assert_eq!(1, num_deleted);
  }

  #[tokio::test]
  #[serial]
  async fn follow() {
    let mut conn = build_db_conn_for_tests().await;
    let inserted_instance = Instance::read_or_create(&mut *conn, "my_domain.tld".to_string())
      .await
      .unwrap();

    let person_form_1 = PersonInsertForm::builder()
      .name("erich".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();
    let person_1 = Person::create(&mut *conn, &person_form_1).await.unwrap();
    let person_form_2 = PersonInsertForm::builder()
      .name("michele".into())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build();
    let person_2 = Person::create(&mut *conn, &person_form_2).await.unwrap();

    let follow_form = PersonFollowerForm {
      person_id: person_1.id,
      follower_id: person_2.id,
      pending: false,
    };
    let person_follower = PersonFollower::follow(&mut *conn, &follow_form)
      .await
      .unwrap();
    assert_eq!(person_1.id, person_follower.person_id);
    assert_eq!(person_2.id, person_follower.follower_id);
    assert!(!person_follower.pending);

    let followers = PersonFollower::list_followers(&mut *conn, person_1.id)
      .await
      .unwrap();
    assert_eq!(vec![person_2], followers);

    let unfollow = PersonFollower::unfollow(&mut *conn, &follow_form)
      .await
      .unwrap();
    assert_eq!(1, unfollow);
  }
}
