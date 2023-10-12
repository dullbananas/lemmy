use crate::error::{LemmyError, LemmyErrorType};
use actix_web::dev::{ConnectionInfo, Service, ServiceRequest, ServiceResponse, Transform};
use enum_map::{enum_map, EnumMap};
use futures::future::{ok, Ready};
use rate_limiter::{ActionType, BucketConfig, InstantSecs, RateLimitState};
use serde::{Deserialize, Serialize};
use std::{
  future::Future,
  net::{IpAddr, Ipv4Addr, SocketAddr},
  pin::Pin,
  rc::Rc,
  str::FromStr,
  sync::{Arc, Mutex},
  task::{Context, Poll},
  time::Duration,
};
use tokio::sync::OnceCell;
use typed_builder::TypedBuilder;

pub mod rate_limiter;

#[derive(Debug, Deserialize, Serialize, Clone, TypedBuilder)]
pub struct RateLimitConfig {
  #[builder(default = 180)]
  /// Maximum number of messages created in interval
  pub message: i32,
  #[builder(default = 60)]
  /// Interval length for message limit, in seconds
  pub message_per_second: i32,
  #[builder(default = 6)]
  /// Maximum number of posts created in interval
  pub post: i32,
  #[builder(default = 300)]
  /// Interval length for post limit, in seconds
  pub post_per_second: i32,
  #[builder(default = 3)]
  /// Maximum number of registrations in interval
  pub register: i32,
  #[builder(default = 3600)]
  /// Interval length for registration limit, in seconds
  pub register_per_second: i32,
  #[builder(default = 6)]
  /// Maximum number of image uploads in interval
  pub image: i32,
  #[builder(default = 3600)]
  /// Interval length for image uploads, in seconds
  pub image_per_second: i32,
  #[builder(default = 6)]
  /// Maximum number of comments created in interval
  pub comment: i32,
  #[builder(default = 600)]
  /// Interval length for comment limit, in seconds
  pub comment_per_second: i32,
  #[builder(default = 60)]
  /// Maximum number of searches created in interval
  pub search: i32,
  #[builder(default = 600)]
  /// Interval length for search limit, in seconds
  pub search_per_second: i32,
  #[builder(default = 1)]
  /// Maximum number of user settings imports in interval
  pub import_user_settings: i32,
  #[builder(default = 24 * 60 * 60)]
  /// Interval length for importing user settings, in seconds (defaults to 24 hours)
  pub import_user_settings_per_second: i32,
}

impl From<RateLimitConfig> for EnumMap<ActionType, BucketConfig> {
  fn from(rate_limit: RateLimitConfig) -> Self {
    enum_map! {
      ActionType::Message => (rate_limit.message, rate_limit.message_per_second),
      ActionType::Post => (rate_limit.post, rate_limit.post_per_second),
      ActionType::Register => (rate_limit.register, rate_limit.register_per_second),
      ActionType::Image => (rate_limit.image, rate_limit.image_per_second),
      ActionType::Comment => (rate_limit.comment, rate_limit.comment_per_second),
      ActionType::Search => (rate_limit.search, rate_limit.search_per_second),
      ActionType::ImportUserSettings => (rate_limit.import_user_settings, rate_limit.import_user_settings_per_second),
    }
    .map(|_key, (capacity, secs_to_refill)| BucketConfig {
      capacity: u32::try_from(capacity).unwrap_or(0),
      secs_to_refill: u32::try_from(secs_to_refill).unwrap_or(0),
    })
  }
}

#[derive(Debug, Clone)]
pub struct RateLimitChecker {
  state: Arc<Mutex<RateLimitState>>,
  action_type: ActionType,
}

/// Single instance of rate limit config and buckets, which is shared across all threads.
#[derive(Clone)]
pub struct RateLimitCell {
  state: Arc<Mutex<RateLimitState>>,
}

impl RateLimitCell {
  /// Initialize cell if it wasnt initialized yet. Otherwise returns the existing cell.
  pub async fn new(rate_limit_config: RateLimitConfig) -> &'static Self {
    static LOCAL_INSTANCE: OnceCell<RateLimitCell> = OnceCell::const_new();
    LOCAL_INSTANCE
      .get_or_init(|| async {
        let rate_limit = Arc::new(Mutex::new(RateLimitState::new(rate_limit_config.into())));
        let rate_limit3 = rate_limit.clone();
        tokio::spawn(async move {
          let hour = Duration::from_secs(3600);
          loop {
            tokio::time::sleep(hour).await;
            rate_limit3
              .lock()
              .expect("Failed to lock rate limit mutex for reading")
              .remove_full_buckets(InstantSecs::now());
          }
        });
        RateLimitCell { state: rate_limit }
      })
      .await
  }

  pub fn set_config(&self, config: RateLimitConfig) {
    self
      .state
      .lock()
      .expect("Failed to lock rate limit mutex for updating")
      .set_config(config.into());
  }

  pub fn message(&self) -> RateLimitChecker {
    self.new_checker(ActionType::Message)
  }

  pub fn post(&self) -> RateLimitChecker {
    self.new_checker(ActionType::Post)
  }

  pub fn register(&self) -> RateLimitChecker {
    self.new_checker(ActionType::Register)
  }

  pub fn image(&self) -> RateLimitChecker {
    self.new_checker(ActionType::Image)
  }

  pub fn comment(&self) -> RateLimitChecker {
    self.new_checker(ActionType::Comment)
  }

  pub fn search(&self) -> RateLimitChecker {
    self.new_checker(ActionType::Search)
  }

  pub fn import_user_settings(&self) -> RateLimitChecker {
    self.new_checker(ActionType::ImportUserSettings)
  }

  fn new_checker(&self, action_type: ActionType) -> RateLimitChecker {
    RateLimitChecker {
      state: self.state.clone(),
      action_type,
    }
  }
}

pub struct RateLimitedMiddleware<S> {
  checker: RateLimitChecker,
  service: Rc<S>,
}

impl RateLimitChecker {
  /// Returns true if the request passed the rate limit, false if it failed and should be rejected.
  pub fn check(self, ip_addr: IpAddr) -> bool {
    // Does not need to be blocking because the RwLock in settings never held across await points,
    // and the operation here locks only long enough to clone
    let mut state = self
      .state
      .lock()
      .expect("Failed to lock rate limit mutex for reading");

    state.check(self.action_type, ip_addr, InstantSecs::now())
  }
}

impl<S> Transform<S, ServiceRequest> for RateLimitChecker
where
  S: Service<ServiceRequest, Response = ServiceResponse, Error = actix_web::Error> + 'static,
  S::Future: 'static,
{
  type Response = S::Response;
  type Error = actix_web::Error;
  type InitError = ();
  type Transform = RateLimitedMiddleware<S>;
  type Future = Ready<Result<Self::Transform, Self::InitError>>;

  fn new_transform(&self, service: S) -> Self::Future {
    ok(RateLimitedMiddleware {
      checker: self.clone(),
      service: Rc::new(service),
    })
  }
}

type FutResult<T, E> = dyn Future<Output = Result<T, E>>;

impl<S> Service<ServiceRequest> for RateLimitedMiddleware<S>
where
  S: Service<ServiceRequest, Response = ServiceResponse, Error = actix_web::Error> + 'static,
  S::Future: 'static,
{
  type Response = S::Response;
  type Error = actix_web::Error;
  type Future = Pin<Box<FutResult<Self::Response, Self::Error>>>;

  fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.service.poll_ready(cx)
  }

  fn call(&self, req: ServiceRequest) -> Self::Future {
    let ip_addr = get_ip(&req.connection_info());

    let checker = self.checker.clone();
    let service = self.service.clone();

    Box::pin(async move {
      if checker.check(ip_addr) {
        service.call(req).await
      } else {
        let (http_req, _) = req.into_parts();
        Ok(ServiceResponse::from_err(
          LemmyError::from(LemmyErrorType::RateLimitError),
          http_req,
        ))
      }
    })
  }
}

fn get_ip(conn_info: &ConnectionInfo) -> IpAddr {
  conn_info
    .realip_remote_addr()
    .and_then(parse_ip)
    .unwrap_or(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)))
}

fn parse_ip(addr: &str) -> Option<IpAddr> {
  if let Some(s) = addr.strip_suffix(']') {
    IpAddr::from_str(s.get(1..)?).ok()
  } else if let Ok(ip) = IpAddr::from_str(addr) {
    Some(ip)
  } else if let Ok(socket) = SocketAddr::from_str(addr) {
    Some(socket.ip())
  } else {
    None
  }
}

#[cfg(test)]
mod tests {
  #![allow(clippy::unwrap_used)]
  #![allow(clippy::indexing_slicing)]

  #[test]
  fn test_parse_ip() {
    let ip_addrs = [
      "1.2.3.4",
      "1.2.3.4:8000",
      "2001:db8::",
      "[2001:db8::]",
      "[2001:db8::]:8000",
    ];
    for addr in ip_addrs {
      assert!(super::parse_ip(addr).is_some(), "failed to parse {addr}");
    }
  }
}
