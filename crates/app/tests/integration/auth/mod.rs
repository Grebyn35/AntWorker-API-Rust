//! Who a caller is (the auth spec).

mod challenges;
mod counted;
mod devices;
mod email_change;
mod ending;
// part: github
mod github;
// end part: github
// part: google
mod google;
// end part: google
mod hashing;
mod held;
mod identities;
mod listed;
// part: openid_connect
mod openid_connect;
// end part: openid_connect
mod password;
mod providers;
mod reauthentication;
mod renewal;
mod required;
mod second_factors;
mod sessions;
mod sign_in;
mod verification;
