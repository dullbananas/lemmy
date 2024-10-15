use activitypub_federation::config::Data;
use actix_web::web::Json;
use lemmy_api_common::{
  community::{AddModToCommunity, AddModToCommunityResponse},
  context::LemmyContext,
  send_activity::{ActivityChannel, SendActivityData},
  utils::check_community_mod_action,
};
use lemmy_db_schema::{
  source::{
    community::{Community, CommunityModerator, CommunityModeratorForm},
    local_user::LocalUser,
    moderator::{ModAddCommunity, ModAddCommunityForm},
  },
  traits::{Crud, Joinable},
};
use lemmy_db_views::structs::LocalUserView;
use lemmy_db_views_actor::structs::CommunityModeratorView;
use lemmy_utils::error::{LemmyErrorExt, LemmyErrorType, LemmyResult};

#[tracing::instrument(skip(context))]
pub async fn add_mod_to_community(
  data: Json<AddModToCommunity>,
  context: Data<LemmyContext>,
  local_user_view: LocalUserView,
) -> LemmyResult<Json<AddModToCommunityResponse>> {
  let community_id = data.community_id;

  // Verify that only mods or admins can add mod
  check_community_mod_action(
    &local_user_view.person,
    community_id,
    false,
    &mut context.pool(),
  )
  .await?;

  // If its a mod removal, also check that you're a higher mod.
  if !data.added {
    LocalUser::is_higher_mod_or_admin_check(
      &mut context.pool(),
      community_id,
      local_user_view.person.id,
      vec![data.person_id],
    )
    .await?;
  }

  let community = Community::read(&mut context.pool(), community_id).await?;

  // If user is admin and community is remote, explicitly check that he is a
  // moderator. This is necessary because otherwise the action would be rejected
  // by the community's home instance.
  if local_user_view.local_user.admin && !community.local {
    CommunityModeratorView::check_is_community_moderator(
      &mut context.pool(),
      community.id,
      local_user_view.person.id,
    )
    .await?;
  }

  // Update in local database
  let community_moderator_form = CommunityModeratorForm {
    community_id: data.community_id,
    person_id: data.person_id,
  };
  if data.added {
    CommunityModerator::join(&mut context.pool(), &community_moderator_form)
      .await
      .with_lemmy_type(LemmyErrorType::CommunityModeratorAlreadyExists)?;
  } else {
    CommunityModerator::leave(&mut context.pool(), &community_moderator_form)
      .await
      .with_lemmy_type(LemmyErrorType::CommunityModeratorAlreadyExists)?;
  }

  // Mod tables
  let form = ModAddCommunityForm {
    mod_person_id: local_user_view.person.id,
    other_person_id: data.person_id,
    community_id: data.community_id,
    removed: Some(!data.added),
  };

  ModAddCommunity::create(&mut context.pool(), &form).await?;

  // Note: in case a remote mod is added, this returns the old moderators list, it will only get
  //       updated once we receive an activity from the community (like `Announce/Add/Moderator`)
  let community_id = data.community_id;
  let moderators = CommunityModeratorView::for_community(&mut context.pool(), community_id).await?;

  ActivityChannel::submit_activity(
    SendActivityData::AddModToCommunity {
      moderator: local_user_view.person,
      community_id: data.community_id,
      target: data.person_id,
      added: data.added,
    },
    &context,
  )
  .await?;

  Ok(Json(AddModToCommunityResponse { moderators }))
}
