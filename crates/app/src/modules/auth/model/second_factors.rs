//! The domain types of second factors (D25, D134): an authenticator app's secret, which computes
//! its codes, a code as the user typed it, and what a request gives to prove the factor.

use std::borrow::Cow;

use data_encoding::BASE32_NOPAD;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use platform::{
    errors::Internal,
    random,
    types::{Invalid, Secret},
};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

use super::RecoveryCode;

/// How many bytes an app's secret holds: the 160 bits that RFC 4226 recommends (AUTH-055).
pub(crate) const SECRET_BYTES: usize = 20;

/// How long each step of an app's codes lasts, in seconds (AUTH-057).
pub(crate) const STEP_SECONDS: i64 = 30;

/// How many digits an app's code has (AUTH-057).
const APP_CODE_DIGITS: u8 = 6;

/// What a URI's label and parameters escape: every character but the unreserved ones of RFC 3986.
const ESCAPED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// An authenticator app's secret, 160 random bits. Its `Debug` shows no byte (SEC-01).
#[derive(Debug)]
pub(crate) struct AppSecret(Secret<[u8; SECRET_BYTES]>);

impl AppSecret {
    /// A new secret, from the operating system's secure random source (AUTH-055, SEC-03).
    pub(crate) fn generate() -> Result<Self, Internal> {
        Ok(Self(Secret::new(random::bytes::<SECRET_BYTES>()?)))
    }

    /// The secret that a row held, once opened (AUTH-066).
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, Internal> {
        let bytes: [u8; SECRET_BYTES] = bytes
            .try_into()
            .map_err(|_| Internal::new("a stored app's secret isn't 20 bytes"))?;
        Ok(Self(Secret::new(bytes)))
    }

    /// The secret's bytes, which a row stores encrypted. Never log them.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.0.expose()
    }

    /// The secret in base32 without padding (RFC 4648), as a user types it into an app that
    /// can't read a QR code (AUTH-055). Never log it.
    pub(crate) fn base32(&self) -> String {
        BASE32_NOPAD.encode(self.0.expose())
    }

    /// The `otpauth://totp/` URI that an app reads from a QR code, with `issuer`, the product's
    /// name, before `account`, the account's address, in its label, and the parameters that every
    /// common app reads (AUTH-055). Never log it.
    pub(crate) fn uri(&self, issuer: &str, account: &str) -> String {
        let issuer = utf8_percent_encode(issuer, ESCAPED);
        let account = utf8_percent_encode(account, ESCAPED);
        format!(
            "otpauth://totp/{issuer}:{account}?secret={}&issuer={issuer}&algorithm=SHA1&digits={APP_CODE_DIGITS}&period={STEP_SECONDS}",
            self.base32()
        )
    }

    /// The step whose code `code` is, at `now`, seconds since the Unix epoch by the database's
    /// clock: the current step or the one before it, and only a step after `last_used`, so each
    /// step's code works once (AUTH-057). The comparison is constant-time, as `totp-rs` makes it.
    pub(crate) fn accepts(&self, code: &AppCode, now: i64, last_used: Option<i64>) -> Option<i64> {
        let totp = self.totp()?;
        let current = now.div_euclid(STEP_SECONDS);
        [current, current - 1].into_iter().find(|&step| {
            last_used.is_none_or(|last| step > last)
                && u64::try_from(step * STEP_SECONDS)
                    .is_ok_and(|at| totp.check(code.digits(), at).is_some())
        })
    }

    /// The code of the step at `at`, seconds since the Unix epoch, as the app shows it.
    #[cfg(test)]
    pub(crate) fn code_at(&self, at: u64) -> String {
        self.totp()
            .map(|totp| totp.generate(at).to_string())
            .unwrap_or_default()
    }

    /// RFC 6238's one-time passwords of this secret: HMAC-SHA-1, 6 digits and steps of 30
    /// seconds, with no step of skew, since `accepts` chooses the steps (AUTH-057).
    fn totp(&self) -> Option<totp_rs::Totp> {
        totp_rs::Builder::new()
            .with_algorithm(totp_rs::Algorithm::SHA1)
            .with_digits(APP_CODE_DIGITS)
            .with_skew(0)
            .with_step_duration(STEP_SECONDS.unsigned_abs())
            .with_secret(totp_rs::Secret::new_stack(*self.0.expose()))
            .build()
            .ok()
    }
}

/// An app's code, 6 digits, as the user typed it. Its `Debug` shows no digit (SEC-01).
#[derive(Clone, Debug)]
pub(crate) struct AppCode(Secret<String>);

impl AppCode {
    /// The code the user typed, trimmed, or why it isn't one (AUTH-056, AUTH-060).
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let value = value.trim();
        if value.len() == usize::from(APP_CODE_DIGITS)
            && value.bytes().all(|byte| byte.is_ascii_digit())
        {
            Ok(Self(Secret::new(value.to_owned())))
        } else {
            Err(Invalid::new("invalid_code", "An app's code is 6 digits."))
        }
    }

    /// The code's digits. Never log them.
    pub(crate) fn digits(&self) -> &str {
        self.0.expose()
    }
}

impl PartialSchema for AppCode {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(usize::from(APP_CODE_DIGITS)))
            .max_length(Some(32))
            .pattern(Some("^\\s*[0-9]{6}\\s*$"))
            .description(Some(
                "The 6 digits that the authenticator app shows, which spaces around them may \
                 surround.",
            ))
            .into()
    }
}

impl ToSchema for AppCode {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("AppCode")
    }
}

/// What a request gives to prove an account's second factor: its app's code, or one of its
/// recovery codes (AUTH-059, AUTH-062, AUTH-065).
#[derive(Clone, Debug)]
pub(crate) enum FactorProof {
    /// The app's code.
    App(AppCode),
    /// A recovery code.
    Recovery(RecoveryCode),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The secret of RFC 6238's test vectors for SHA-1: the ASCII of `12345678901234567890`.
    fn rfc_secret() -> AppSecret {
        AppSecret::from_bytes(b"12345678901234567890").unwrap()
    }

    #[test]
    fn auth_057_an_app_s_code_is_rfc_6238_s_with_sha_1_6_digits_and_30_second_steps() {
        // RFC 6238, Appendix B, with the last 6 of the 8 digits that its vectors show.
        for (at, code) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(rfc_secret().code_at(at), code, "{at}");
        }
    }

    #[test]
    fn auth_057_a_check_takes_the_current_step_or_the_one_before_and_each_step_once() {
        let secret = rfc_secret();
        let now: i64 = 1_111_111_111;
        let step = now / STEP_SECONDS;
        let at = |step: i64| u64::try_from(step * STEP_SECONDS).unwrap();
        let current = AppCode::parse(&secret.code_at(at(step))).unwrap();
        let before = AppCode::parse(&secret.code_at(at(step - 1))).unwrap();
        let older = AppCode::parse(&secret.code_at(at(step - 2))).unwrap();
        let next = AppCode::parse(&secret.code_at(at(step + 1))).unwrap();
        assert_eq!(secret.accepts(&current, now, None), Some(step));
        assert_eq!(secret.accepts(&before, now, None), Some(step - 1));
        assert_eq!(secret.accepts(&older, now, None), None);
        assert_eq!(secret.accepts(&next, now, None), None);
        // A step at or before the last used one works no more.
        assert_eq!(secret.accepts(&current, now, Some(step)), None);
        assert_eq!(secret.accepts(&before, now, Some(step - 1)), None);
        assert_eq!(secret.accepts(&current, now, Some(step - 1)), Some(step));
    }

    #[test]
    fn auth_055_the_secret_is_160_bits_in_base32_and_its_uri_names_the_issuer_and_the_account() {
        let secret = AppSecret::generate().unwrap();
        assert_eq!(secret.bytes().len(), 20);
        assert_eq!(secret.base32().len(), 32);
        assert_ne!(secret.bytes(), AppSecret::generate().unwrap().bytes());
        let rfc = rfc_secret();
        assert_eq!(rfc.base32(), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(
            rfc.uri("AntWorker", "ada+1@example.com"),
            "otpauth://totp/AntWorker:ada%2B1%40example.com?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\
             &issuer=AntWorker&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn auth_060_a_typed_app_code_is_6_digits_once_trimmed() {
        assert_eq!(AppCode::parse(" 012345\n").unwrap().digits(), "012345");
        for refused in ["12345", "1234567", "12 456", "12a456", "", "+12345"] {
            assert_eq!(
                AppCode::parse(refused).unwrap_err().code,
                "invalid_code",
                "{refused}"
            );
        }
    }

    #[test]
    fn sec_01_an_app_s_secret_and_code_show_nothing_in_debug() {
        let secret = rfc_secret();
        assert!(!format!("{secret:?}").contains("49"));
        assert!(!format!("{:?}", AppCode::parse("424242").unwrap()).contains("4242"));
    }
}
