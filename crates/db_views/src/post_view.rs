use crate::structs::{LocalUserView, PaginationCursor, PostView};
use diesel::{
  data_types::PgInterval,
  debug_query,
  dsl::{self, exists, not, InnerJoin, InnerJoinQuerySource, IntervalDsl},
  expression::AsExpression,
  pg::Pg,
  result::Error,
  sql_function,
  sql_types::{self as st, SingleValue, SqlType},
  BoolExpressionMethods,
  Expression,
  ExpressionMethods,
  NullableExpressionMethods,
  OptionalExtension,
  PgTextExpressionMethods,
  QueryDsl,
};
use diesel_async::RunQueryDsl;
use lemmy_db_schema::{
  aggregates::structs::PostAggregates,
  newtypes::{CommunityId, PersonId, PostId},
  schema::{
    community,
    community_aggregates,
    community_block,
    community_follower,
    community_moderator,
    community_person_ban,
    instance_block,
    local_user,
    local_user_language,
    person,
    person_block,
    person_post_aggregates,
    post,
    post_aggregates,
    post_like,
    post_read,
    post_saved,
  },
  utils::{
    and_then,
    boxed_meth,
    fuzzy_search,
    get_conn,
    is_some_and,
    limit_and_offset,
    now,
    BoxExpr,
    DbPool,
    FilterVarEq,
    FirstOrLoad,
  },
  ListingType,
  SortType,
};
use lemmy_utils::type_chain;
use tracing::debug;

#[derive(Clone, Copy)]
enum Ord {
  Desc,
  Asc,
}

trait PaginationCursorField {
  fn order_and_page_filter<'a>(
    &self,
    query: BoxedQuery<'a>,
    order: Ord,
    first: &Option<PaginationCursorData>,
    last: &Option<PaginationCursorData>,
  ) -> BoxedQuery<'a>;
}

impl<C, T, F> PaginationCursorField for (C, F)
where
  for<'a> BoxedQuery<'a>: boxed_meth::ThenOrderDsl<dsl::Desc<C>>
    + boxed_meth::ThenOrderDsl<dsl::Asc<C>>
    + boxed_meth::FilterDsl<dsl::LtEq<C, T>>
    + boxed_meth::FilterDsl<dsl::GtEq<C, T>>,
  C: Expression + Copy,
  C::SqlType: SingleValue + SqlType,
  T: AsExpression<C::SqlType>,
  F: Fn(&PostAggregates) -> T + Copy,
{
  fn order_and_page_filter<'a>(
    &self,
    query: BoxedQuery<'a>,
    order: Ord,
    first: &Option<PaginationCursorData>,
    last: &Option<PaginationCursorData>,
  ) -> BoxedQuery<'a> {
    let (column, getter) = *self;
    let (mut query, min, max) = match order {
      Ord::Desc => (query.then_order_by(column.desc()), last, first),
      Ord::Asc => (query.then_order_by(column.asc()), first, last),
    };
    if let Some(min) = min {
      query = query.filter(column.ge(getter(&min.0)));
    }
    if let Some(max) = max {
      query = query.filter(column.le(getter(&max.0)));
    }
    query
  }
}

/// Returns `&dyn PaginationCursorField` for the given name
macro_rules! field {
  ($name:ident) => {{
    &(post_aggregates::$name, |e: &PostAggregates| e.$name) as &dyn PaginationCursorField
  }};
}

enum QueryInput<'a> {
  Read {
    post_id: PostId,
    me: Option<PersonId>,
    is_mod_or_admin: bool,
  },
  List {
    options: PostQuery<'a>,
  },
}

sql_function!(fn coalesce(x: st::Nullable<st::BigInt>, y: st::BigInt) -> st::BigInt);

async fn build_query<'a>(pool: &mut DbPool<'_>, input: &'a QueryInput<'_>) -> Result<impl FirstOrLoad<'a, PostView>, Error> {
  let me = match input {
    QueryInput::Read{me, ..} => me,
    QueryInput::List{local_user, ..}=>local_user.person.id,
  };

  let mut subscribe = move || {
      and_then(me, |me| {
        community_follower::table
          .find((me, post_aggregates::community_id))
          .select(community_follower::pending.nullable())
          .single_value()
      })
  };
  let mut saved = is_some_and(me, |me| {
    exists(post_saved::table.find((me, post_aggregates::post_id)))
  });
  let mut read = is_some_and(me, |me| {
    exists(post_read::table.find((me, post_aggregates::post_id)))
  });
  let mut creator_blocked = is_some_and(me, |me| {
    exists(person_block::table.find((me, post_aggregates::creator_id)))
  });
  let mut my_vote = and_then(me, |me| {
    post_like::table
      .find((me, post_aggregates::post_id))
      .select(post_like::score.nullable())
      .single_value()
  });
  let read_comments = and_then(self.me, |me| {
    person_post_aggregates::table
      .find((me, post_aggregates::post_id))
      .select(person_post_aggregates::read_comments.nullable())
      .single_value()
  });
  let creator_banned_from_community = exists(
    community_person_ban::table
      .find((post_aggregates::creator_id, post_aggregates::community_id)),
  );
  let creator_is_moderator = exists(
    community_moderator::table.find((post_aggregates::creator_id, post_aggregates::community_id)),
  );
  let creator_is_admin = exists(
    local_user::table
      .filter(local_user::person_id.eq(post_aggregates::creator_id))
      .filter(local_user::admin),
  );
  let not_removed = not(community::removed.or(post::removed));
  let not_deleted = not(community::deleted.or(post::deleted));
  let is_creator = post_aggregates::creator_id.nullable().eq(me);

  let new_query = || post_aggregates::table
    .inner_join(person::table)
    .inner_join(community::table)
    .inner_join(post::table)
    .into_boxed();

  let final_query = match input {
    QueryInput::Read{post_id, me, is_mod_or_admin} => {
      let mut query = new_query().filter(post_aggregates::post_id.eq(post_id));
  
      if !is_mod_or_admin {
        // only show removed or deleted posts to creator
        query = query.filter(is_creator.or(not_removed.and(not_deleted)));
      }

      query
    },
    QueryInput::List{options} => {
      let (limit, mut offset) = limit_and_offset(options.page, options.limit)?;
      let listing_type = options.listing_type.unwrap_or(ListingType::All);
      let sort = options.sort.unwrap_or(SortType::Hot);
      let local_user = options.local_user.map(|l| &l.local_user);

      let admin = local_user.map(|l| l.admin).unwrap_or(false);
      let show_nsfw = local_user.map(|l| l.show_nsfw).unwrap_or(false);
      let show_bot_accounts = local_user.map(|l| l.show_bot_accounts).unwrap_or(true);
      let show_read_posts = local_user.map(|l| l.show_read_posts).unwrap_or(true);
  
      if options.page_after.is_some() {
        if offset != 0 {
          return Err(Error::QueryBuilderError(
            "legacy pagination cannot be combined with v2 pagination".into(),
          ));
        }
  
        // always skip exactly one post because that's the last post of the previous page
        // fixing the where clause is more difficult because we'd have to change only the last order-by-where clause
        // e.g. WHERE (featured_local<=, hot_rank<=, published<=) to WHERE (<=, <=, <)
        offset = 1;
      }
  
      let build_inner_query = |page_before_or_equal: Option<PaginationCursorData>| {
        let mut query = new_query();
  
        let is_subscriber = || subscribe().is_not_null();
  
        query = query
          // hide posts from deleted communities
          .filter(not(community::deleted))
          // only show deleted posts to creator
          .filter(is_creator(me).or(not(post::deleted)));
  
        // only show removed posts to admin when viewing user profile
        if !(options.is_profile_view && admin) {
          query = query.filter(not_removed());
        }
  
        if let Some(community_id) = options.community_id {
          query = query.filter(post_aggregates::community_id.eq(community_id));
        }
        if let Some(creator_id) = options.creator_id {
          query = query.filter(post_aggregates::creator_id.eq(creator_id));
        }
        if let Some(url_search) = &options.url_search {
          query = query.filter(post::url.eq(url_search));
        }
        if let Some(search_term) = &options.search_term {
          let pattern = fuzzy_search(search_term);
          let name_matches = post::name.ilike(pattern.clone());
          let body_matches = post::body.ilike(pattern);
          query = query.filter(name_matches.or(body_matches));
        }
  
        query = match listing_type {
          ListingType::Subscribed => query.filter(is_subscriber()),
          ListingType::Local => query.filter(community::local),
          ListingType::All => query,
          ListingType::ModeratorView => query.filter(is_some_and(me, |me| {
            exists(community_moderator::table.find((me, post_aggregates::community_id)))
          })),
        };
  
        // Filters that should not affect which posts can be moderated
        if listing_type != ListingType::ModeratorView {
          // If a user is logged in, then only show posts with a language that the user enabled.
          if let Some(local_user) = local_user {
            query = query.filter(exists(
              local_user_language::table.find((local_user.id, post::language_id)),
            ));
          }
  
          // Hide posts from blocked instances, communities, and persons
          query = query
            .filter_var_eq(&mut selection_builder.creator_blocked, false)
            .filter(not(is_some_and(me, |me| {
              let community_blocked =
                exists(community_block::table.find((me, post_aggregates::community_id)));
              let instance_blocked =
                exists(instance_block::table.find((me, post_aggregates::instance_id)));
              community_blocked.or(instance_blocked)
            })));
  
          // This filter hides hidden communities for non-subscribers. For `ListingType::Subscribed`,
          // it is redundant and would cause a duplicated `community_follower` subquery.
          if listing_type != ListingType::Subscribed {
            query = query.filter(is_subscriber().or(not(community::hidden)));
          }
        }
  
        if !show_nsfw {
          query = query.filter(not(post::nsfw.or(community::nsfw)));
        }
        if !show_bot_accounts {
          query = query.filter(not(person::bot_account));
        }
        if !(show_read_posts || options.saved_only || options.is_profile_view) {
          query = query.filter_var_eq(&mut selection_builder.read, false);
        }
        if options.saved_only {
          query = query.filter_var_eq(&mut selection_builder.saved, true);
        }
        if options.liked_only {
          query = query.filter_var_eq(&mut selection_builder.my_vote, 1);
        }
        if options.disliked_only {
          query = query.filter_var_eq(&mut selection_builder.my_vote, -1);
        }
  
        // Show featured posts first
        let featured_field = if options.community_id.is_some() {
          field!(featured_community)
        } else {
          field!(featured_local)
        };
  
        let (main_sort_field, top_sort_interval) = match sort
        {
          SortType::Active => (field!(hot_rank_active), None),
          SortType::Hot => (field!(hot_rank), None),
          SortType::Scaled => (field!(scaled_rank), None),
          SortType::Controversial => (field!(controversy_rank), None),
          SortType::New => (field!(published), None),
          SortType::Old => (field!(published), None),
          SortType::NewComments => (field!(newest_comment_time), None),
          SortType::MostComments => (field!(comments), None),
          SortType::TopAll => (field!(score), None),
          SortType::TopYear => (field!(score), Some(1.years())),
          SortType::TopMonth => (field!(score), Some(1.months())),
          SortType::TopWeek => (field!(score), Some(1.weeks())),
          SortType::TopDay => (field!(score), Some(1.days())),
          SortType::TopHour => (field!(score), Some(1.hours())),
          SortType::TopSixHour => (field!(score), Some(6.hours())),
          SortType::TopTwelveHour => (field!(score), Some(12.hours())),
          SortType::TopThreeMonths => (field!(score), Some(3.months())),
          SortType::TopSixMonths => (field!(score), Some(6.months())),
          SortType::TopNineMonths => (field!(score), Some(9.months())),
        };

        let main_sort_ord = match sort {
          SortType::Old => Ord::Asc,
          _ => Ord::Desc,
        };
  
        let tie_breaker = match sort {
          // A second time-based sort would not be very useful
          SortType::New | SortType::Old | SortType::NewComments => None,
          _ => Some((Ord::Desc, field!(published))),
        };
  
        for (order, field) in [Some((Ord::Desc, featured_field)), Some((main_sort_ord, main_sort_field)), tie_breaker]
          .into_iter()
          .flatten()
        {
          query = field.order_and_page_filter(query, order, &options.page_after, &page_before_or_equal);
        }
  
        if let Some(interval) = top_sort_interval {
          query = query.filter(post_aggregates::published.gt(now() - interval));
        }
  
        query = query
          .limit(limit)
          .offset(offset);
  
        debug!("Post View Query: {:?}", debug_query::<Pg, _>(&query));
  
        query
      };
  
      let page_before_or_equal = if listing_type == ListingType::Subscribed {
        // first get one page for the most popular community to get an upper bound for the the page end for the real query
        //
        // the reason this is needed is that when fetching posts for a single community PostgreSQL can optimize
        // the query to use an index on e.g. (=, >=, >=, >=) and fetch only LIMIT rows
        // but for the followed-communities query it has to query the index on (IN, >=, >=, >=)
        // which it currently can't do at all (as of PG 16). see the discussion here:
        // https://github.com/LemmyNet/lemmy/issues/2877#issuecomment-1673597190
        //
        // the results are correct no matter which community we fetch these for, since it basically covers the "worst case" of the whole page consisting of posts from one community
        // but using the largest community decreases the pagination-frame so make the real query more efficient.
  
        let largest_subscribed: Option<CommunityId> = community_aggregates::table
          .filter(is_some_and(me, |me| {
            exists(community_follower::table.find((me, community_aggregates::community_id)))
          }))
          .order_by(community_aggregates::users_active_month.desc())
          .select(community_aggregates::community_id)
          .first(&mut *get_conn(pool).await?)
          .await
          .optional()?;
  
        build_inner_query(None)
          .filter(post_aggregates::community_id.nullable().eq(largest_subscribed))
          // If there's at least `limit` rows, then get the last row within the limit, otherwise
          // get `None` which prevents the amount of rows returned by the final query from being
          // incorrectly limited
          .offset(offset + limit - 1)
          .select(post_aggregates::all_columns)
          .first(&mut *get_conn(pool).await?)
          .await
          .optional()?
          .map(PaginationCursorData)
      } else {
        None
      };
  
      build_inner_query(page_before_or_equal)
    },
  };
  
  Ok(final_query.select((
      post::all_columns,
      person::all_columns,
      community::all_columns,
      creator_banned_from_community,
      creator_is_moderator,
      creator_is_admin,
      post_aggregates::all_columns,
      subscribe(),
      saved,
      read,
      creator_blocked,
      my_vote,
      post_aggregates::comments - coalesce(read_comments, 0),
  )))
}

type BoxedQuery<'a> = dsl::IntoBoxed<
  'a,
  type_chain!(post_aggregates::table.InnerJoin<person::table>.InnerJoin<community::table>.InnerJoin<post::table>),
  Pg,
>;

impl PostView {
  pub async fn read(
    pool: &mut DbPool<'_>,
    post_id: PostId,
    me: Option<PersonId>,
    is_mod_or_admin: bool,
  ) -> Result<Self, Error> {
    build_query(pool, &QueryInput::Read {post_id, me, is_mod_or_admin}).await?
      .first(&mut *get_conn(pool).await?)
      .await
  }
}

impl PaginationCursor {
  // get cursor for page that starts immediately after the given post
  pub fn after_post(view: &PostView) -> PaginationCursor {
    // hex encoding to prevent ossification
    PaginationCursor(format!("P{:x}", view.counts.post_id.0))
  }
  pub async fn read(&self, pool: &mut DbPool<'_>) -> Result<PaginationCursorData, Error> {
    Ok(PaginationCursorData(
      PostAggregates::read(
        pool,
        PostId(
          self
            .0
            .get(1..)
            .and_then(|e| i32::from_str_radix(e, 16).ok())
            .ok_or_else(|| Error::QueryBuilderError("Could not parse pagination token".into()))?,
        ),
      )
      .await?,
    ))
  }
}

// currently we use a postaggregates struct as the pagination token.
// we only use some of the properties of the post aggregates, depending on which sort type we page by
#[derive(Clone)]
pub struct PaginationCursorData(PostAggregates);

#[derive(Default, Clone)]
pub struct PostQuery<'a> {
  pub listing_type: Option<ListingType>,
  pub sort: Option<SortType>,
  pub creator_id: Option<PersonId>,
  pub community_id: Option<CommunityId>,
  pub local_user: Option<&'a LocalUserView>,
  pub search_term: Option<String>,
  pub url_search: Option<String>,
  pub saved_only: bool,
  pub liked_only: bool,
  pub disliked_only: bool,
  pub is_profile_view: bool,
  pub page: Option<i64>,
  pub limit: Option<i64>,
  pub page_after: Option<PaginationCursorData>,
}

impl<'a> PostQuery<'a> {
  pub async fn list(self, pool: &mut DbPool<'_>) -> Result<Vec<PostView>, Error> {
    build_query(pool, &QueryInput::List(self)).await?
      .load(&mut *get_conn(pool).await?)
      .await
  }
}

#[cfg(test)]
mod tests {
  use crate::{
    post_view::{PostQuery, PostView},
    structs::LocalUserView,
  };
  use lemmy_db_schema::{
    aggregates::structs::PostAggregates,
    impls::actor_language::UNDETERMINED_ID,
    newtypes::LanguageId,
    source::{
      actor_language::LocalUserLanguage,
      community::{Community, CommunityInsertForm, CommunityModerator, CommunityModeratorForm},
      community_block::{CommunityBlock, CommunityBlockForm},
      instance::Instance,
      instance_block::{InstanceBlock, InstanceBlockForm},
      language::Language,
      local_user::{LocalUser, LocalUserInsertForm, LocalUserUpdateForm},
      person::{Person, PersonInsertForm},
      person_block::{PersonBlock, PersonBlockForm},
      post::{Post, PostInsertForm, PostLike, PostLikeForm, PostUpdateForm},
    },
    traits::{Blockable, Crud, Joinable, Likeable},
    utils::{build_db_pool, DbPool, RANK_DEFAULT},
    SortType,
    SubscribedType,
  };
  use lemmy_utils::error::LemmyResult;
  use serial_test::serial;

  const POST_BY_BLOCKED_PERSON: &str = "post by blocked person";
  const POST_BY_BOT: &str = "post by bot";
  const POST: &str = "post";

  fn names(post_views: &[PostView]) -> Vec<&str> {
    post_views.iter().map(|i| &i.post.name).collect()
  }

  struct Data {
    inserted_instance: Instance,
    local_user_view: LocalUserView,
    blocked_local_user_view: LocalUserView,
    inserted_bot: Person,
    inserted_community: Community,
    inserted_post: Post,
  }

  impl Data {
    fn default_post_query(&self) -> PostQuery<'_> {
      PostQuery {
        sort: Some(SortType::New),
        local_user: Some(&self.local_user_view),
        ..Default::default()
      }
    }
  }
  
    fn person_insert_form(&self, instance_id: InstanceId, name: &str) -> PersonInsertForm {
      PersonInsertForm::builder()
        .name(name.to_owned())
        .public_key("pubkey".to_string())
        .instance_id(instance_id)
        .build()
    }

  fn local_user_form(person_id: PersonId) -> LocalUserInsertForm {
      LocalUserInsertForm::builder()
        .person_id(person_id)
        .password_encrypted(String::new())
        .build()
  }

  async fn init_data(pool: &mut DbPool<'_>) -> LemmyResult<Data> {
    let inserted_instance = Instance::read_or_create(pool, "my_domain.tld".to_string())
      .await?;

    let person_insert_form = |name| person_insert_form(inserted_instance.id, name);

    let inserted_person = Person::create(pool, &person_insert_form("tegan").await?;

    let inserted_local_user = LocalUser::create(pool, &LocalUserInsertForm {
      admin: Some(true),
      ..local_user_form(inserted_person.id)
    }).await?;

    let inserted_bot = Person::create(pool, &PersonInsertForm {
      bot_account: Some(true),
      ..person_insert_form("mybot")
    }).await?;


    let inserted_community = Community::create(pool, &
    CommunityInsertForm::builder()
      .name("test_community_3".to_string())
      .title("nada".to_owned())
      .public_key("pubkey".to_string())
      .instance_id(inserted_instance.id)
      .build()
                                              ).await?;

    // Test a person block, make sure the post query doesn't include their post
    let inserted_blocked_person = Person::create(pool, &person_insert_form("john")).await?;

    let inserted_blocked_local_user = LocalUser::create(pool, &local_user_insert_form(inserted_blocked_person.id)).await?;

    Post::create(pool, &
    PostInsertForm::builder()
      .name(POST_BY_BLOCKED_PERSON.to_string())
      .creator_id(inserted_blocked_person.id)
      .community_id(inserted_community.id)
      .language_id(Some(LanguageId(1)))
      .build()
                ).await?;

    // block that person
    PersonBlock::block(pool, &
    PersonBlockForm {
      person_id: inserted_person.id,
      target_id: inserted_blocked_person.id,
    }
                      ).await?;

    // A sample post
    let inserted_post = Post::create(pool, &
    PostInsertForm::builder()
      .name(POST.to_string())
      .creator_id(inserted_person.id)
      .community_id(inserted_community.id)
      .language_id(Some(LanguageId(47)))
      .build()
                                    ).await?;


    let _inserted_bot_post = Post::create(pool, &
    PostInsertForm::builder()
      .name(POST_BY_BOT.to_string())
      .creator_id(inserted_bot.id)
      .community_id(inserted_community.id)
      .build()
                                         ).await?;
    let local_user_view = LocalUserView {
      local_user: inserted_local_user,
      person: inserted_person,
      counts: Default::default(),
    };

    let blocked_local_user_view = LocalUserView {
      local_user: inserted_blocked_local_user,
      person: inserted_blocked_person,
      counts: Default::default(),
    };

    Ok(Data {
      inserted_instance,
      local_user_view,
      blocked_local_user_view,
      inserted_bot,
      inserted_community,
      inserted_post,
    })
  }

  #[tokio::test]
  #[serial]
  async fn post_listing_with_person() -> LemmyResult<()> {
    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let mut data = init_data(pool).await?

    data.local_user_view.local_user =
      LocalUser::update(pool, data.local_user_view.local_user.id, &
    LocalUserUpdateForm {
      show_bot_accounts: Some(false),
      ..Default::default()
    }
                       )
        .await?;

    let post_list_without_bots = PostQuery {
      community_id: Some(data.inserted_community.id),
      ..data.default_post_query()
    }
    .list(pool)
    .await?;

    let post = PostView::read(
      pool,
      data.inserted_post.id,
      Some(data.local_user_view.person.id),
      false,
    )
    .await?;

    let expected_post = expected_post_view(&data, pool).await;

    // Should be only one person, IE the bot post, and blocked should be missing
    assert_eq!(vec![post], post_list);
    assert_eq!(
      expected_post,
      post
    );

    data.local_user_view.local_user =
      LocalUser::update(pool, data.local_user_view.local_user.id, &
    LocalUserUpdateForm {
      show_bot_accounts: Some(true),
      ..Default::default()
    }
                       )
        .await?;

    let post_list_with_bots = PostQuery {
      community_id: Some(data.inserted_community.id),
      ..data.default_post_query()
    }
    .list(pool)
    .await?;
    // should include bot post which has "undetermined" language
    assert_eq!(vec![POST_BY_BOT, POST], names(&post_list_with_bots));

    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn post_listing_no_person() -> LemmyResult<()> {
    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let data = init_data(pool).await?;

    let post_list = PostQuery {
      community_id: Some(data.inserted_community.id),
      local_user: None,
      ..data.default_post_query()
    }
    .list(pool)
    .await?;

    let post =
      PostView::read(pool, data.inserted_post.id, None, false)
        .await?;

    let expected_post = expected_post_view(&data, pool).await;

    // Should be 2 posts, with the bot post, and the blocked
    assert_eq!([POST_BY_BOT, POST, POST_BY_BLOCKED_PERSON], names(&post_list));

    assert_eq!(
      Some(expected_post),
      post_list.get(1)
    );
    assert_eq!(
      expected_post,
      post
    );

    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn post_listing_block_community() -> LemmyResult<()> {
    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let data = init_data(pool).await?;

    let community_block = CommunityBlockForm {
      person_id: data.local_user_view.person.id,
      community_id: data.inserted_community.id,
    };
    CommunityBlock::block(pool, &community_block).await?;

    let post_list_after_block = PostQuery {
      community_id: Some(data.inserted_community.id),
      ..data.default_post_query()
    }
    .list(pool)
    .await?;
    // Should be 0 posts after the community block
    assert_eq!(vec![], post_list_after_block);

    CommunityBlock::unblock(pool, &community_block)
      .await?;
    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn post_listing_like() -> LemmyResult<()> {
    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let mut data = init_data(pool).await?;

    let inserted_post_like = PostLike::like(pool, &
    PostLikeForm {
      post_id: data.inserted_post.id,
      person_id: data.local_user_view.person.id,
      score: 1,
    }
                                           ).await?;

    let expected_post_like = PostLike {
      post_id: data.inserted_post.id,
      person_id: data.local_user_view.person.id,
      published: inserted_post_like.published,
      score: 1,
    };
    assert_eq!(expected_post_like, inserted_post_like);

    let post = PostView::read(
      pool,
      data.inserted_post.id,
      Some(data.local_user_view.person.id),
      false,
    )
    .await?;

    let mut expected_post_with_upvote = expected_post_view(&data, pool).await;
    expected_post_with_upvote.my_vote = Some(1);
    expected_post_with_upvote.counts.score = 1;
    expected_post_with_upvote.counts.upvotes = 1;
    assert_eq!(expected_post_with_upvote, post);

    data.local_user_view.local_user = 
      LocalUser::update(pool, data.local_user_view.local_user.id, &
    LocalUserUpdateForm {
      show_bot_accounts: Some(false),
      ..Default::default()
    }
                       )
        .await?;

    let post_list = PostQuery {
      community_id: Some(data.inserted_community.id),
      ..data.default_post_query()
    }
    .list(pool)
    .await?;
    assert_eq!(vec![expected_post_vith_upvote], post_list);

    let liked_post_list = PostQuery {
      community_id: Some(data.inserted_community.id),
      liked_only: true,
      ..data.default_post_query()
    }
    .list(pool)
    .await?;
    assert_eq!(post_list, liked_post_list);

    let disliked_post_list = PostQuery {
      community_id: Some(data.inserted_community.id),
      disliked_only: true,
      ..data.default_post_query()
    }
    .list(pool)
    .await?;
    assert_eq!(vec![], disliked_post_list);

    let like_removed =
      PostLike::remove(pool, data.local_user_view.person.id, data.inserted_post.id)
        .await?;
    assert_eq!(1, like_removed);
    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn creator_info() -> LemmyResult<()> {
    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let data = init_data(pool).await?;

    // Make one of the inserted persons a moderator
    CommunityModerator::join(pool, &
    CommunityModeratorForm {
      community_id: data.inserted_community.id,
      person_id: data.local_user_view.person.id,
    }
                            ).await?;

    let post_list = PostQuery {
      community_id: Some(data.inserted_community.id),
      ..data.default_post_query()
    }
    .list(pool)
    .await?
    .into_iter()
    .map(|p| (p.creator.name, p.creator_is_moderator, p.creator_is_admin))
    .collect::<Vec<_>>();

    let expected_post_list = vec![
      ("mybot".to_owned(), false, false),
      ("tegan".to_owned(), true, true),
    ];

    assert_eq!(expected_post_listing, post_listing);

    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn post_listing_person_language() -> LemmyResult<()> {
    const EL_POSTO: &str = "el posto";

    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let data = init_data(pool).await?;

    let spanish_id = Language::read_id_from_code(pool, Some("es"))
      .await?
      .unwrap();

    let french_id = Language::read_id_from_code(pool, Some("fr"))
      .await?
      .unwrap();

    let post_spanish = PostInsertForm::builder()
      .name(EL_POSTO.to_string())
      .creator_id(data.local_user_view.person.id)
      .community_id(data.inserted_community.id)
      .language_id(Some(spanish_id))
      .build();

    Post::create(pool, &post_spanish).await?;

    let post_list = data.default_post_query().list(pool).await?;

    // no language filters specified, all posts should be returned
    assert_eq!(vec![EL_POSTO, POST_BY_BOT, POST], names(&post_list));

    LocalUserLanguage::update(pool, vec![french_id], data.local_user_view.local_user.id)
      .await?;

    let french_post_list = data.default_post_query().list(pool).await?;

    // only one post in french and one undetermined should be returned
    assert_eq!(vec![POST_BY_BOT, POST], names(&french_post_list));
    assert_eq!(Some(french_id), french_post_list.get(1).map(|p| p.post.language_id));

    LocalUserLanguage::update(
      pool,
      vec![french_id, UNDETERMINED_ID],
      data.local_user_view.local_user.id,
    )
    .await?;
    let french_und_post_list = data.default_post_query().list(pool).await?.into_iter().map(|p| (p.post.name, p.language_id)).collect::<Vec<_>>();
    let expected_french_und_post_list = vec![
      (POST_BY_BOT, UNDETERMINED_ID),
      (POST, french_id),
    ];

    // french post and undetermined language post should be returned
    assert_eq!(expected_french_und_post_list, french_und_post_list);

    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn post_listings_removed() -> LemmyResult<()> {
    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let mut data = init_data(pool).await?;

    // Remove the post
    Post::update(
      pool,
      data.inserted_post.id,
      &PostUpdateForm {
        removed: Some(true),
        ..Default::default()
      },
    )
    .await?;

    // Make sure you don't see the removed post in the results
    let post_list = data.default_post_query().list(pool).await?;
    assert_eq!(vec![POST_BY_BOT], names(&post_list));

    // Removed post is shown to admins on profile page
    data.local_user_view.local_user.admin = true;
    let post_list_on_profile_page = PostQuery {
      is_profile_view: true,
      ..data.default_post_query()
    }
    .list(pool)
    .await?;
    assert_eq!(vec![POST_BY_BOT, POST], names(&post_list_on_profile_page));

    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn post_listings_deleted() {
    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let data = init_data(pool).await?;

    // Delete the post
    Post::update(
      pool,
      data.inserted_post.id,
      &PostUpdateForm {
        deleted: Some(true),
        ..Default::default()
      },
    )
    .await?;

    for (local_user, expect_contains_deleted) in [
      (None, false),
      (Some(&data.blocked_local_user_view), false),
      (Some(&data.local_user_view), true),
    ] {
      let contains_deleted = PostQuery {
        local_user,
        ..data.default_post_query()
      }
      .list(pool)
      .await?
        .iter()
        .any(|p| p.post.id == data.inserted_post.id);

      assert_eq!(expect_contains_deleted, contains_deleted);
    }

    cleanup(data, pool).await
  }

  #[tokio::test]
  #[serial]
  async fn post_listing_instance_block() -> LemmyResult<()> {
    const POST_FROM_BLOCKED_INSTANCE: &str = "post on blocked instance";

    let pool = &build_db_pool().await?;
    let pool = &mut pool.into();
    let data = init_data(pool).await?;

    let blocked_instance = Instance::read_or_create(pool, "another_domain.tld".to_string())
      .await?;

    let inserted_community = Community::create(pool, &
    CommunityInsertForm::builder()
      .name("test_community_4".to_string())
      .title("none".to_owned())
      .public_key("pubkey".to_string())
      .instance_id(blocked_instance.id)
      .build()
                                              ).await?;

    let post_from_blocked_instance = Post::create(pool, &
    PostInsertForm::builder()
      .name(POST_FROM_BLOCKED_INSTANCE.to_string())
      .creator_id(data.inserted_bot.id)
      .community_id(inserted_community.id)
      .language_id(Some(LanguageId(1)))
      .build()
                                                 ).await?;

    // no instance block, should return all posts
    let post_list = data.default_post_query().list(pool).await?;
    assert_eq!(vec![POST_FROM_BLOCKED_INSTANCE, POST_BY_BOT, POST], name(&post_list));

    // block the instance
    InstanceBlock::block(pool, &
    InstanceBlockForm {
      person_id: data.local_user_view.person.id,
      instance_id: blocked_instance.id,
    }
                        ).await?;

    // now posts from communities on that instance should be hidden
    let post_list_with_block = data.default_post_query().list(pool).await?;
    assert_eq!(vec![POST_BY_BOT, POST], names(&post_list_with_block));
    assert(post_list_with_block.iter().all(|p| p.post.id != post_from_blocked_instance.id));

    // after unblocking it should return all posts again
    InstanceBlock::unblock(pool, &block_form).await?;
    let post_list_with_removed_block = data.default_post_query().list(pool).await?;
    assert_eq!(vec![POST_FROM_BLOCKED_INSTANCE, POST_BY_BOT, POST], names(&post_listings_blocked));

    Instance::delete(pool, blocked_instance.id).await?;
    cleanup(data, pool).await
  }

  async fn cleanup(data: Data, pool: &mut DbPool<'_>) -> LemmyResult<()> {
    let num_deleted = Post::delete(pool, data.inserted_post.id).await?;
    Community::delete(pool, data.inserted_community.id)
      .await?;
    Person::delete(pool, data.local_user_view.person.id)
      .await?;
    Person::delete(pool, data.inserted_bot.id).await?;
    Person::delete(pool, data.blocked_local_user_view.person.id)
      .await?;
    Instance::delete(pool, data.inserted_instance.id)
      .await?;
    assert_eq!(1, num_deleted);

    Ok(())
  }

  async fn expected_post_view(data: &Data, pool: &mut DbPool<'_>) -> LemmyResult<PostView> {
    let (inserted_person, inserted_community, inserted_post) = (
      &data.local_user_view.person,
      &data.inserted_community,
      &data.inserted_post,
    );
    let agg = PostAggregates::read(pool, inserted_post.id).await?;

    Ok(PostView {
      post: Post {
        id: inserted_post.id,
        name: inserted_post.name.clone(),
        creator_id: inserted_person.id,
        url: None,
        body: None,
        published: inserted_post.published,
        updated: None,
        community_id: inserted_community.id,
        removed: false,
        deleted: false,
        locked: false,
        nsfw: false,
        embed_title: None,
        embed_description: None,
        embed_video_url: None,
        thumbnail_url: None,
        ap_id: inserted_post.ap_id.clone(),
        local: true,
        language_id: LanguageId(47),
        featured_community: false,
        featured_local: false,
      },
      my_vote: None,
      unread_comments: 0,
      creator: Person {
        id: inserted_person.id,
        name: inserted_person.name.clone(),
        display_name: None,
        published: inserted_person.published,
        avatar: None,
        actor_id: inserted_person.actor_id.clone(),
        local: true,
        bot_account: false,
        banned: false,
        deleted: false,
        bio: None,
        banner: None,
        updated: None,
        inbox_url: inserted_person.inbox_url.clone(),
        shared_inbox_url: None,
        matrix_user_id: None,
        ban_expires: None,
        instance_id: data.inserted_instance.id,
        private_key: inserted_person.private_key.clone(),
        public_key: inserted_person.public_key.clone(),
        last_refreshed_at: inserted_person.last_refreshed_at,
      },
      creator_banned_from_community: false,
      creator_is_moderator: false,
      creator_is_admin: true,
      community: Community {
        id: inserted_community.id,
        name: inserted_community.name.clone(),
        icon: None,
        removed: false,
        deleted: false,
        nsfw: false,
        actor_id: inserted_community.actor_id.clone(),
        local: true,
        title: "nada".to_owned(),
        description: None,
        updated: None,
        banner: None,
        hidden: false,
        posting_restricted_to_mods: false,
        published: inserted_community.published,
        instance_id: data.inserted_instance.id,
        private_key: inserted_community.private_key.clone(),
        public_key: inserted_community.public_key.clone(),
        last_refreshed_at: inserted_community.last_refreshed_at,
        followers_url: inserted_community.followers_url.clone(),
        inbox_url: inserted_community.inbox_url.clone(),
        shared_inbox_url: inserted_community.shared_inbox_url.clone(),
        moderators_url: inserted_community.moderators_url.clone(),
        featured_url: inserted_community.featured_url.clone(),
      },
      counts: PostAggregates {
        post_id: inserted_post.id,
        comments: 0,
        score: 0,
        upvotes: 0,
        downvotes: 0,
        published: agg.published,
        newest_comment_time_necro: inserted_post.published,
        newest_comment_time: inserted_post.published,
        featured_community: false,
        featured_local: false,
        hot_rank: RANK_DEFAULT,
        hot_rank_active: RANK_DEFAULT,
        controversy_rank: 0.0,
        scaled_rank: RANK_DEFAULT,
        community_id: inserted_post.community_id,
        creator_id: inserted_post.creator_id,
        instance_id: data.inserted_instance.id,
      },
      subscribed: SubscribedType::NotSubscribed,
      read: false,
      saved: false,
      creator_blocked: false,
    })
  }
}
