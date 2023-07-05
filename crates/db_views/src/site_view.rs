use crate::structs::SiteView;
use diesel::{result::Error, ExpressionMethods, JoinOnDsl, QueryDsl};
use lemmy_db_schema::{
  aggregates::structs::SiteAggregates,
  schema::{local_site, local_site_rate_limit, site, site_aggregates},
  source::{local_site::LocalSite, local_site_rate_limit::LocalSiteRateLimit, site::Site},
  utils::{GetConn, RunQueryDsl},
};

impl SiteView {
  pub async fn read_local(mut conn: impl GetConn) -> Result<Self, Error> {
    let (mut site, local_site, local_site_rate_limit, counts) = site::table
      .inner_join(local_site::table)
      .inner_join(
        local_site_rate_limit::table.on(local_site::id.eq(local_site_rate_limit::local_site_id)),
      )
      .inner_join(site_aggregates::table)
      .select((
        site::all_columns,
        local_site::all_columns,
        local_site_rate_limit::all_columns,
        site_aggregates::all_columns,
      ))
      .first::<(Site, LocalSite, LocalSiteRateLimit, SiteAggregates)>(conn)
      .await?;

    site.private_key = None;
    Ok(SiteView {
      site,
      local_site,
      local_site_rate_limit,
      counts,
    })
  }
}
