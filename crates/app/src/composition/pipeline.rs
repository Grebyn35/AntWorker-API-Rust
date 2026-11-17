//! The request pipeline that serves the modules' routes, which `serve` builds once the services
//! are composed (ARCH-12).

use std::sync::Arc;

use platform::{
    config::{Environment, ServerConfig},
    errors,
    http::{Origins, Pipeline},
    rate_limit::RateLimiter,
    telemetry::Service,
};
// part: files
use platform::signed_urls::SignedUrls;
// end part: files

use super::{Resources, Services, openapi};

/// The request pipeline that `config` describes, which counts its rate limits in the database of
/// `resources` and verifies access tokens through `services`, and which serves the API reference
/// of `service`'s document in development (PLATFORM-052).
pub fn pipeline(
    resources: &Resources,
    config: &ServerConfig,
    services: &Services,
    service: Service,
) -> Pipeline {
    let reference = if config.environment == Environment::Development {
        match serde_json::to_string(&openapi(service)) {
            Ok(document) => Some(Arc::from(document)),
            // Development goes without the reference, which serves no client (ERR-11).
            Err(error) => {
                errors::ignore(&error, "api_reference_not_written");
                None
            }
        }
    } else {
        None
    };
    Pipeline {
        clock: resources.clock.clone(),
        max_concurrent_requests: usize::try_from(config.max_concurrent_requests)
            .unwrap_or(usize::MAX),
        readiness: resources.readiness.clone(),
        trusted_proxies: config.trusted_proxies.clone(),
        origins: Origins::new(&config.cors_allowed_origins),
        limiter: RateLimiter::new(resources.database.clone(), config.hmac_key.clone()),
        rate_limits: config.rate_limits,
        verifier: services.verifier.clone(),
        // part: files
        signed_urls: SignedUrls::new(config.hmac_key.clone()),
        // end part: files
        authorizer: services.authorizer.clone(),
        reference,
    }
}
