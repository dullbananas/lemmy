use crate::structs::PrivateMessageView;
use diesel::{
  debug_query,
  pg::Pg,
  result::Error,
  BoolExpressionMethods,
  ExpressionMethods,
  JoinOnDsl,
  QueryDsl,
};
use lemmy_db_schema::{
  newtypes::{PersonId, PrivateMessageId},
  schema::{person, private_message},
  source::{person::Person, private_message::PrivateMessage},
  traits::JoinView,
  utils::{limit_and_offset, GetConn, RunQueryDsl},
};
use tracing::debug;
use typed_builder::TypedBuilder;

type PrivateMessageViewTuple = (PrivateMessage, Person, Person);

impl PrivateMessageView {
  pub async fn read(
    mut conn: impl GetConn,
    private_message_id: PrivateMessageId,
  ) -> Result<Self, Error> {
    let person_alias_1 = diesel::alias!(person as person1);

    let (private_message, creator, recipient) = private_message::table
      .find(private_message_id)
      .inner_join(person::table.on(private_message::creator_id.eq(person::id)))
      .inner_join(
        person_alias_1.on(private_message::recipient_id.eq(person_alias_1.field(person::id))),
      )
      .order_by(private_message::published.desc())
      .select((
        private_message::all_columns,
        person::all_columns,
        person_alias_1.fields(person::all_columns),
      ))
      .first::<PrivateMessageViewTuple>(conn)
      .await?;

    Ok(PrivateMessageView {
      private_message,
      creator,
      recipient,
    })
  }

  /// Gets the number of unread messages
  pub async fn get_unread_messages(
    mut conn: impl GetConn,
    my_person_id: PersonId,
  ) -> Result<i64, Error> {
    use diesel::dsl::count;
    private_message::table
      .filter(private_message::read.eq(false))
      .filter(private_message::recipient_id.eq(my_person_id))
      .filter(private_message::deleted.eq(false))
      .select(count(private_message::id))
      .first::<i64>(conn)
      .await
  }
}

#[derive(TypedBuilder)]
#[builder(field_defaults(default))]
pub struct PrivateMessageQuery<Conn> {
  #[builder(!default)]
  conn: Conn,
  #[builder(!default)]
  recipient_id: PersonId,
  unread_only: Option<bool>,
  page: Option<i64>,
  limit: Option<i64>,
}

impl<Conn: GetConn> PrivateMessageQuery<Conn> {
  pub async fn list(self) -> Result<Vec<PrivateMessageView>, Error> {
    let mut conn = self.conn;
    let person_alias_1 = diesel::alias!(person as person1);

    let mut query = private_message::table
      .inner_join(person::table.on(private_message::creator_id.eq(person::id)))
      .inner_join(
        person_alias_1.on(private_message::recipient_id.eq(person_alias_1.field(person::id))),
      )
      .select((
        private_message::all_columns,
        person::all_columns,
        person_alias_1.fields(person::all_columns),
      ))
      .into_boxed();

    // If its unread, I only want the ones to me
    if self.unread_only.unwrap_or(false) {
      query = query
        .filter(private_message::read.eq(false))
        .filter(private_message::recipient_id.eq(self.recipient_id));
    }
    // Otherwise, I want the ALL view to show both sent and received
    else {
      query = query.filter(
        private_message::recipient_id
          .eq(self.recipient_id)
          .or(private_message::creator_id.eq(self.recipient_id)),
      )
    }

    let (limit, offset) = limit_and_offset(self.page, self.limit)?;

    query = query
      .filter(private_message::deleted.eq(false))
      .limit(limit)
      .offset(offset)
      .order_by(private_message::published.desc());

    debug!(
      "Private Message View Query: {:?}",
      debug_query::<Pg, _>(&query)
    );

    let res = query.load::<PrivateMessageViewTuple>(conn).await?;

    Ok(
      res
        .into_iter()
        .map(PrivateMessageView::from_tuple)
        .collect(),
    )
  }
}

impl JoinView for PrivateMessageView {
  type JoinTuple = PrivateMessageViewTuple;
  fn from_tuple(a: Self::JoinTuple) -> Self {
    Self {
      private_message: a.0,
      creator: a.1,
      recipient: a.2,
    }
  }
}
