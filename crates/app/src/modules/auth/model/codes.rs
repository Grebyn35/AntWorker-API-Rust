//! The codes that `auth` sends to an account's address (D25): a verification code, which proves
//! the address (AUTH-030), and a reset code, which resets its password (AUTH-036), with what
//! each proves.

use std::borrow::Cow;

use platform::{
    errors::Internal,
    random,
    types::{Invalid, Secret},
};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

/// What a verification code proves, as the database's `verification_purpose` names it
/// (AUTH-030, DATA-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::modules::auth) enum Purpose {
    /// That the account holds its email address (AUTH-031).
    EmailVerification,
    /// That its requester may reset the account's password (AUTH-037).
    PasswordReset,
}

impl Purpose {
    /// The purpose's name in the database.
    pub(in crate::modules::auth) fn name(self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
        }
    }

    /// The purpose named `name`, which a query read.
    pub(in crate::modules::auth) fn from_name(name: &str) -> Result<Self, Internal> {
        match name {
            "email_verification" => Ok(Self::EmailVerification),
            "password_reset" => Ok(Self::PasswordReset),
            _ => Err(Internal::new("a code's purpose that auth doesn't know")),
        }
    }
}

/// How many digits a verification code has (AUTH-030).
const CODE_DIGITS: usize = 6;

/// How many codes of [`CODE_DIGITS`] digits there are.
const CODES: u32 = 1_000_000;

/// A verification code: 6 digits (AUTH-030), as the user typed it, or as the API drew it. Its
/// `Debug` shows no digit (SEC-01).
#[derive(Clone, Debug)]
pub(crate) struct VerificationCode(Secret<String>);

impl VerificationCode {
    /// The code the user typed, trimmed, or why it isn't one (AUTH-032).
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let value = value.trim();
        if value.len() == CODE_DIGITS && value.bytes().all(|byte| byte.is_ascii_digit()) {
            Ok(Self(Secret::new(value.to_owned())))
        } else {
            Err(Invalid::new("invalid_code", "A code is 6 digits."))
        }
    }

    /// A new code, each of the million as likely, from the operating system's secure random source
    /// (AUTH-030, SEC-03): a number of 32 random bits is drawn again while it is at or above the
    /// largest multiple of a million that 32 bits hold, so its remainder is uniform.
    pub(crate) fn generate() -> Result<Self, Internal> {
        const UNIFORM_BELOW: u32 = u32::MAX - u32::MAX % CODES;
        loop {
            let value = u32::from_be_bytes(random::bytes::<4>()?);
            if value < UNIFORM_BELOW {
                return Ok(Self(Secret::new(format!("{:06}", value % CODES))));
            }
        }
    }

    /// The code's digits, for its HMAC and its email. Never log them.
    pub(crate) fn digits(&self) -> &str {
        self.0.expose()
    }
}

impl PartialSchema for VerificationCode {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(CODE_DIGITS))
            .max_length(Some(32))
            .pattern(Some("^\\s*[0-9]{6}\\s*$"))
            .description(Some(
                "The 6 digits of the code, which spaces around them may surround.",
            ))
            .into()
    }
}

impl ToSchema for VerificationCode {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("VerificationCode")
    }
}

/// Crockford's base32 alphabet, each character at its value: the digits, and the letters but I,
/// L, O and U (AUTH-036).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// How many characters a reset code has: the 80 bits of 10 random bytes, 5 to a character
/// (AUTH-036).
const RESET_CODE_CHARS: usize = 16;

/// The most characters a typed reset code has, its hyphens and spaces among them.
const RESET_CODE_MAX_INPUT: usize = 64;

/// A reset code: 16 characters of Crockford's base32 alphabet (AUTH-036), as the API drew it, or as
/// the user typed it, read as Crockford's decoding reads it. Its `Debug` shows no character
/// (SEC-01).
#[derive(Clone, Debug)]
pub(crate) struct ResetCode(Secret<String>);

impl ResetCode {
    /// The code the user typed, read without regard to case, without its hyphens and spaces, and
    /// with I and L read as 1 and O as 0, or why it isn't one (AUTH-038).
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let refused = || {
            Invalid::new(
                "invalid_code",
                "A reset code is 16 letters and digits, which hyphens and spaces may separate.",
            )
        };
        if value.chars().count() > RESET_CODE_MAX_INPUT {
            return Err(refused());
        }
        let mut code = String::with_capacity(RESET_CODE_CHARS);
        for character in value.chars() {
            let read = match character.to_ascii_uppercase() {
                '-' => continue,
                other if other.is_whitespace() => continue,
                'I' | 'L' => '1',
                'O' => '0',
                other if other.is_ascii() && CROCKFORD.contains(&(other as u8)) => other,
                _ => return Err(refused()),
            };
            code.push(read);
        }
        if code.len() == RESET_CODE_CHARS {
            Ok(Self(Secret::new(code)))
        } else {
            Err(refused())
        }
    }

    /// A new code, the 80 bits of 10 bytes from the operating system's secure random source, 5 to
    /// a character, each character as likely as any other (AUTH-036, SEC-03).
    pub(crate) fn generate() -> Result<Self, Internal> {
        let mut bits = [0; 16];
        bits[6..].copy_from_slice(&random::bytes::<10>()?);
        let value = u128::from_be_bytes(bits);
        let code = (0..RESET_CODE_CHARS)
            .rev()
            .map(|place| {
                let index = usize::try_from((value >> (5 * place)) & 0x1f).unwrap_or(0);
                char::from(CROCKFORD[index])
            })
            .collect();
        Ok(Self(Secret::new(code)))
    }

    /// The code's 16 characters, for its HMAC. Never log them.
    pub(crate) fn characters(&self) -> &str {
        self.0.expose()
    }

    /// The code in four groups of four characters, as its email shows it (AUTH-036). Never log
    /// it.
    pub(crate) fn grouped(&self) -> String {
        let characters: Vec<char> = self.0.expose().chars().collect();
        characters
            .chunks(4)
            .map(|group| group.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl PartialSchema for ResetCode {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(RESET_CODE_CHARS))
            .max_length(Some(RESET_CODE_MAX_INPUT))
            .description(Some(
                "The 16 characters of the reset code, of Crockford's base32 alphabet, without \
                 regard to case, which hyphens and spaces may separate.",
            ))
            .into()
    }
}

impl ToSchema for ResetCode {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("ResetCode")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_030_a_code_is_6_digits_of_any_value() {
        let codes: Vec<String> = (0..2_000)
            .map(|_| VerificationCode::generate().unwrap().digits().to_owned())
            .collect();
        assert!(
            codes
                .iter()
                .all(|code| code.len() == 6 && code.bytes().all(|byte| byte.is_ascii_digit()))
        );
        // Codes start with every digit, a leading zero among them.
        for digit in b'0'..=b'9' {
            assert!(
                codes.iter().any(|code| code.as_bytes()[0] == digit),
                "{digit}"
            );
        }
    }

    #[test]
    fn auth_032_a_typed_code_is_6_digits_once_trimmed() {
        assert_eq!(
            VerificationCode::parse(" 012345\t").unwrap().digits(),
            "012345"
        );
        for refused in ["12345", "1234567", "12 456", "12a456", "", "\u{661}23456"] {
            assert_eq!(
                VerificationCode::parse(refused).unwrap_err().code,
                "invalid_code",
                "{refused}"
            );
        }
    }

    #[test]
    fn auth_036_a_reset_code_is_16_characters_of_crockford_s_base32() {
        let codes: Vec<String> = (0..2_000)
            .map(|_| ResetCode::generate().unwrap().characters().to_owned())
            .collect();
        assert!(codes.iter().all(|code| {
            code.len() == 16 && code.bytes().all(|byte| CROCKFORD.contains(&byte))
        }));
        // Every character of the alphabet appears, at the first place and at the last.
        for &character in CROCKFORD {
            assert!(
                codes.iter().any(|code| code.as_bytes()[0] == character),
                "{character}"
            );
            assert!(
                codes.iter().any(|code| code.as_bytes()[15] == character),
                "{character}"
            );
        }
    }

    #[test]
    fn auth_036_a_reset_code_s_email_shows_it_in_four_groups_of_four() {
        let code = ResetCode::parse("7KQM2XWD9RTBH4NP").unwrap();
        assert_eq!(code.grouped(), "7KQM-2XWD-9RTB-H4NP");
    }

    #[test]
    fn auth_038_a_typed_reset_code_is_read_as_crockford_s_decoding_reads_it() {
        for typed in [
            "7KQM-2XWD-9RTB-H4NP",
            "7kqm 2xwd 9rtb h4np",
            " 7KQM2XWD9RTBH4NP\n",
        ] {
            assert_eq!(
                ResetCode::parse(typed).unwrap().characters(),
                "7KQM2XWD9RTBH4NP",
                "{typed}"
            );
        }
        assert_eq!(
            ResetCode::parse("IL0O-1111-0000-ABCD")
                .unwrap()
                .characters(),
            "110011110000ABCD"
        );
        for refused in [
            "7KQM-2XWD-9RTB-H4N",
            "7KQM-2XWD-9RTB-H4NPX",
            "7KQM-2XWD-9RTB-H4NU",
            "7KQM_2XWD_9RTB_H4NP",
            "",
            "\u{661}KQM2XWD9RTBH4NP",
        ] {
            assert_eq!(
                ResetCode::parse(refused).unwrap_err().code,
                "invalid_code",
                "{refused}"
            );
        }
        assert_eq!(
            ResetCode::parse(&format!("7KQM2XWD9RTBH4NP{}", " ".repeat(49)))
                .unwrap_err()
                .code,
            "invalid_code"
        );
    }

    #[test]
    fn sec_01_a_reset_code_s_debug_shows_no_character() {
        let code = ResetCode::parse("7KQM-2XWD-9RTB-H4NP").unwrap();
        assert!(!format!("{code:?}").contains("7KQM"));
    }

    #[test]
    fn sec_01_a_code_s_debug_shows_no_digit() {
        let code = VerificationCode::parse("424242").unwrap();
        assert!(!format!("{code:?}").contains("4242"));
    }
}
