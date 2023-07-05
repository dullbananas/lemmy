use crate::structs::CommunityPersonBanView;
use diesel::{result::Error, ExpressionMethods, QueryDsl};
use lemmy_db_schema::{
  newtypes::{CommunityId, PersonId},
  schema::{community, community_person_ban, person},
  source::{community::Community, person::Person},
  utils::{GetConn, RunQueryDsl},
};

impl CommunityPersonBanView {
  pub async fn get(
    mut conn: impl GetConn,
    from_person_id: PersonId,
    from_community_id: CommunityId,
  ) -> Result<Self, Error> {
    let (community, person) = community_person_ban::table
      .inner_join(community::table)
      .inner_join(person::table)
      .select((community::all_columns, person::all_columns))
      .filter(community_person_ban::community_id.eq(from_community_id))
      .filter(community_person_ban::person_id.eq(from_person_id))
      .order_by(community_person_ban::published)
      .first::<(Community, Person)>(conn)
      .await?;

    Ok(CommunityPersonBanView { community, person })
  }
}
