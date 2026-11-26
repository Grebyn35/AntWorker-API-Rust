//! The codes that `auth` sends to an address (D25): a verification code, which proves the
//! account's address (AUTH-030), a reset code, which resets its password (AUTH-036), and a change
//! code, which changes its address to the one the code was sent to (AUTH-050), with what each
//! proves; and a recovery code, which stands in for an authenticator app's code (AUTH-062).

use std::{borrow::Cow, marker::PhantomData};

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
pub(crate) enum Purpose {
    /// That the account holds its email address (AUTH-031).
    EmailVerification,
    /// That its requester may reset the account's password (AUTH-037).
    PasswordReset,
    /// That the account may change its address to the one the code was sent to (AUTH-051).
    EmailChange,
}

impl Purpose {
    /// The purpose's name in the database.
    pub(in crate::modules::auth) fn name(self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::EmailChange => "email_change",
        }
    }

    /// The purpose named `name`, which a query read.
    pub(in crate::modules::auth) fn from_name(name: &str) -> Result<Self, Internal> {
        match name {
            "email_verification" => Ok(Self::EmailVerification),
            "password_reset" => Ok(Self::PasswordReset),
            "email_change" => Ok(Self::EmailChange),
            _ => Err(Internal::new("a code's purpose that auth doesn't know")),
        }
    }
}

/// A code sent to an address, which proves one purpose: what its keyed hash covers is its
/// account, its purpose, the address it was sent to and its characters (SEC-03).
pub(in crate::modules::auth) trait Code {
    /// What the code proves.
    const PURPOSE: Purpose;

    /// The code's characters, as the hash that stands in for it covers them. Never log them.
    fn characters(&self) -> &str;
}

impl Code for VerificationCode {
    const PURPOSE: Purpose = Purpose::EmailVerification;

    fn characters(&self) -> &str {
        self.digits()
    }
}

impl<P: LongPurpose> Code for LongCode<P> {
    const PURPOSE: Purpose = P::PURPOSE;

    fn characters(&self) -> &str {
        self.0.expose()
    }
}

/// How many digits a verification code has (AUTH-030).
const CODE_DIGITS: usize = 6;

/// How many codes of [`CODE_DIGITS`] digits there are.
const CODES: u32 = 1_000_000;

/// How many characters a typed verification code holds at most, its digits and the white space
/// around them, as its schema gives (API-17).
const CODE_MAX_INPUT: usize = 32;

/// The white space that a typed code may hold, as a class of its schema's pattern writes it:
/// ECMA-262's `\s`, and U+0085, so the class takes each character that `char::is_whitespace`
/// takes, which a code's conversion reads as white space, and U+FEFF, which it doesn't (API-10).
pub(super) const SPACE_CLASS: &str = "\\s\\u0085";

/// A verification code: 6 digits (AUTH-030), as the user typed it, or as the API drew it. Its
/// `Debug` shows no digit (SEC-01).
#[derive(Clone, Debug)]
pub(crate) struct VerificationCode(Secret<String>);

impl VerificationCode {
    /// The code the user typed, of at most [`CODE_MAX_INPUT`] characters, trimmed, or why it
    /// isn't one (AUTH-032, API-17).
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let within = value.chars().count() <= CODE_MAX_INPUT;
        let value = value.trim();
        if within && value.len() == CODE_DIGITS && value.bytes().all(|byte| byte.is_ascii_digit()) {
            Ok(Self(Secret::new(value.to_owned())))
        } else {
            Err(Invalid::new("invalid_code", "A code is 6 digits."))
        }
    }

    /// A new code, each of the million as likely, from the operating system's secure random source
    /// (AUTH-030, SEC-03).
    pub(crate) fn generate() -> Result<Self, Internal> {
        Self::drawn(|| Ok(u32::from_be_bytes(random::bytes::<4>()?)))
    }

    /// The code of the first number of 32 bits that `draw` gives below the largest multiple of a
    /// million that 32 bits hold, so that its remainder is uniform when the numbers are.
    fn drawn(mut draw: impl FnMut() -> Result<u32, Internal>) -> Result<Self, Internal> {
        const UNIFORM_BELOW: u32 = u32::MAX - u32::MAX % CODES;
        loop {
            let value = draw()?;
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
            .max_length(Some(CODE_MAX_INPUT))
            .pattern(Some(format!(
                "^[{SPACE_CLASS}]*[0-9]{{6}}[{SPACE_CLASS}]*$"
            )))
            .description(Some(
                "The 6 digits of the code, which whitespace may surround.",
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

/// The most characters a typed code of Crockford's base32 alphabet has, its hyphens and white
/// space among them.
const LONG_CODE_MAX_INPUT: usize = 64;

/// What a code of Crockford's base32 alphabet is: how many characters it has, 5 bits to a
/// character, and the name its schema has in the API's document.
pub(crate) trait LongKind {
    /// How many characters the code has: 16 for a reset code or a change code, 80 bits
    /// (AUTH-036, AUTH-050), and 24 for a recovery code, 120 bits (AUTH-062).
    const CHARS: usize;
    /// The name of the code's schema.
    const SCHEMA: &'static str;
    /// The sentence that refuses one.
    const REFUSAL: &'static str;
}

/// What a code of Crockford's base32 alphabet that `auth` sends to an address proves.
pub(crate) trait LongPurpose: LongKind {
    /// What the code proves.
    const PURPOSE: Purpose;
}

/// That a code resets the account's password (AUTH-036).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ForReset;

impl LongKind for ForReset {
    const CHARS: usize = 16;
    const SCHEMA: &'static str = "ResetCode";
    const REFUSAL: &'static str =
        "A reset code is 16 letters and digits, which hyphens and whitespace may separate.";
}

impl LongPurpose for ForReset {
    const PURPOSE: Purpose = Purpose::PasswordReset;
}

/// That a code changes the account's address to the one it was sent to (AUTH-050).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ForChange;

impl LongKind for ForChange {
    const CHARS: usize = 16;
    const SCHEMA: &'static str = "ChangeCode";
    const REFUSAL: &'static str =
        "A change code is 16 letters and digits, which hyphens and whitespace may separate.";
}

impl LongPurpose for ForChange {
    const PURPOSE: Purpose = Purpose::EmailChange;
}

/// That a code stands in once for an authenticator app's code (AUTH-062).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ForRecovery;

impl LongKind for ForRecovery {
    const CHARS: usize = 24;
    const SCHEMA: &'static str = "RecoveryCode";
    const REFUSAL: &'static str =
        "A recovery code is 24 letters and digits, which hyphens and whitespace may separate.";
}

/// A code of Crockford's base32 alphabet, as long as `P` says, 5 random bits to a character, as
/// the API drew it, or as the user typed it, read as Crockford's decoding reads it. Its `Debug`
/// shows no character (SEC-01).
#[derive(Clone, Debug)]
pub(crate) struct LongCode<P>(Secret<String>, PhantomData<P>);

/// A reset code (AUTH-036).
pub(crate) type ResetCode = LongCode<ForReset>;

/// A code that changes the account's address to the one it was sent to (AUTH-050).
pub(crate) type ChangeCode = LongCode<ForChange>;

/// A recovery code (AUTH-062).
pub(crate) type RecoveryCode = LongCode<ForRecovery>;

impl<P: LongKind> LongCode<P> {
    /// The code's characters, as its keyed hash covers them. Never log them.
    pub(crate) fn characters(&self) -> &str {
        self.0.expose()
    }

    /// The code the user typed, read without regard to case, without its hyphens and white space,
    /// and with I and L read as 1 and O as 0, or why it isn't one (AUTH-038, AUTH-052, AUTH-062).
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let refused = || Invalid::new("invalid_code", P::REFUSAL);
        if value.chars().count() > LONG_CODE_MAX_INPUT {
            return Err(refused());
        }
        let mut code = String::with_capacity(P::CHARS);
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
        if code.len() == P::CHARS {
            Ok(Self(Secret::new(code), PhantomData))
        } else {
            Err(refused())
        }
    }

    /// A new code, 5 bits to a character of the operating system's secure random source, each
    /// character as likely as any other (AUTH-036, AUTH-050, AUTH-062, SEC-03): the low bits of
    /// 16 random bytes, of which a character takes 5 at a time.
    pub(crate) fn generate() -> Result<Self, Internal> {
        Ok(Self::of_bits(u128::from_be_bytes(random::bytes::<16>()?)))
    }

    /// The code of `value`'s low bits, 5 to a character: its last character is the one of the
    /// alphabet that the lowest 5 name, the one before it the next 5's, and so on.
    fn of_bits(value: u128) -> Self {
        let code = (0..P::CHARS)
            .rev()
            .map(|place| {
                let index = usize::try_from((value >> (5 * place)) & 0x1f).unwrap_or(0);
                char::from(CROCKFORD[index])
            })
            .collect();
        Self(Secret::new(code), PhantomData)
    }

    /// The code in groups of four characters, as its email or its answer shows it (AUTH-036,
    /// AUTH-050, AUTH-062). Never log it.
    pub(crate) fn grouped(&self) -> String {
        let characters: Vec<char> = self.0.expose().chars().collect();
        characters
            .chunks(4)
            .map(|group| group.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl<P: LongKind> PartialSchema for LongCode<P> {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(P::CHARS))
            .max_length(Some(LONG_CODE_MAX_INPUT))
            .pattern(Some(format!(
                "^[{SPACE_CLASS}-]*(?:[0-9A-TV-Za-tv-z][{SPACE_CLASS}-]*){{{}}}$",
                P::CHARS
            )))
            .description(Some(format!(
                "The {} characters of the code, of Crockford's base32 alphabet, without regard to \
                 case, which hyphens and whitespace may separate.",
                P::CHARS
            )))
            .into()
    }
}

impl<P: LongKind> ToSchema for LongCode<P> {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed(P::SCHEMA)
    }
}

#[cfg(test)]
mod tests {
    use super::{super::AppCode, *};

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
    fn auth_030_a_number_at_or_above_the_last_multiple_of_a_million_is_drawn_again() {
        // 4,294,000,000 is the largest multiple of a million that 32 bits hold: the numbers from
        // it up would make the codes below 967,296 likelier than the rest.
        let mut numbers = [4_294_000_000, u32::MAX, 4_293_999_999].into_iter();

        let code =
            VerificationCode::drawn(|| numbers.next().ok_or_else(|| Internal::new("no more")));

        assert_eq!(code.unwrap().digits(), "999999");
        assert_eq!(numbers.next(), None);
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
    fn api_17_a_typed_code_holds_at_most_the_32_characters_that_its_schema_gives() {
        let at_bound = format!("{}424242{}", " ".repeat(13), "\t".repeat(13));
        assert_eq!(at_bound.chars().count(), 32);
        assert_eq!(
            VerificationCode::parse(&at_bound).unwrap().digits(),
            "424242"
        );
        assert_eq!(
            VerificationCode::parse(&format!("{at_bound} "))
                .unwrap_err()
                .code,
            "invalid_code"
        );
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
    fn auth_036_each_character_of_a_reset_code_takes_5_bits_of_its_own() {
        // The alphabet's indices 0 to 15, the first in the highest 5 of the code's 80 bits.
        let value = (0..16_u128).fold(0, |value, index| (value << 5) | index);

        assert_eq!(ResetCode::of_bits(value).characters(), "0123456789ABCDEF");
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
        // A code padded to 64 characters is taken, and one of 65 is refused.
        assert_eq!(
            ResetCode::parse(&format!("7KQM2XWD9RTBH4NP{}", " ".repeat(48)))
                .unwrap()
                .characters(),
            "7KQM2XWD9RTBH4NP"
        );
        assert_eq!(
            ResetCode::parse(&format!("7KQM2XWD9RTBH4NP{}", " ".repeat(49)))
                .unwrap_err()
                .code,
            "invalid_code"
        );
    }

    #[test]
    fn auth_050_a_change_code_is_16_characters_of_crockford_s_base32_apart_from_a_reset_code() {
        let code = ChangeCode::generate().unwrap();
        assert!(code.characters().len() == 16);
        assert!(
            code.characters()
                .bytes()
                .all(|byte| CROCKFORD.contains(&byte))
        );
        assert_eq!(code.grouped().len(), 19);
        assert_eq!(ChangeCode::PURPOSE, Purpose::EmailChange);
        assert_eq!(ResetCode::PURPOSE, Purpose::PasswordReset);
    }

    #[test]
    fn auth_052_a_typed_change_code_is_read_as_a_reset_code_is() {
        assert_eq!(
            ChangeCode::parse("7kqm-2xwd 9rtb-h4np")
                .unwrap()
                .characters(),
            "7KQM2XWD9RTBH4NP"
        );
        let refused = ChangeCode::parse("7KQM-2XWD-9RTB-H4NU").unwrap_err();
        assert_eq!(refused.code, "invalid_code");
        assert!(
            refused.message.starts_with("A change code"),
            "{}",
            refused.message
        );
    }

    #[test]
    fn auth_062_a_recovery_code_is_24_characters_of_crockford_s_base32_in_six_groups() {
        let codes: Vec<String> = (0..2_000)
            .map(|_| RecoveryCode::generate().unwrap().characters().to_owned())
            .collect();
        assert!(codes.iter().all(|code| {
            code.len() == 24 && code.bytes().all(|byte| CROCKFORD.contains(&byte))
        }));
        for &character in CROCKFORD {
            assert!(
                codes.iter().any(|code| code.as_bytes()[0] == character),
                "{character}"
            );
        }
        // A code padded to 64 characters is taken, and one of 65 is refused.
        assert_eq!(
            RecoveryCode::parse(&format!("7KQM2XWD9RTBH4NP1100ABCD{}", " ".repeat(40)))
                .unwrap()
                .characters(),
            "7KQM2XWD9RTBH4NP1100ABCD"
        );
        assert_eq!(
            RecoveryCode::parse(&format!("7KQM2XWD9RTBH4NP1100ABCD{}", " ".repeat(41)))
                .unwrap_err()
                .code,
            "invalid_code"
        );
        let code = RecoveryCode::parse("7kqm-2xwd 9rtb-h4np-ilo0-abcd").unwrap();
        assert_eq!(code.characters(), "7KQM2XWD9RTBH4NP1100ABCD");
        assert_eq!(code.grouped(), "7KQM-2XWD-9RTB-H4NP-1100-ABCD");
        for refused in [
            "7KQM-2XWD-9RTB-H4NP",
            "7KQM-2XWD-9RTB-H4NP-1100-ABCDE",
            "7KQM-2XWD-9RTB-H4NP-1100-ABCU",
        ] {
            let refusal = RecoveryCode::parse(refused).unwrap_err();
            assert_eq!(refusal.code, "invalid_code", "{refused}");
            assert!(refusal.message.starts_with("A recovery code"), "{refused}");
        }
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

    /// Whether `value` fits `schema`, a type's, read as JSON Schema.
    fn fits(schema: RefOr<Schema>, value: &str) -> bool {
        let schema = serde_json::to_value(schema).unwrap();
        jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&serde_json::json!(value))
    }

    #[test]
    fn api_10_each_white_space_that_a_typed_code_may_hold_is_one_its_pattern_takes() {
        let spaces = (0..=u32::from(char::MAX))
            .filter_map(char::from_u32)
            .filter(|c| c.is_whitespace());

        for space in spaces {
            let six = format!("{space}424242{space}");
            let long = format!("{space}7KQM{space}2XWD-9RTB{space}H4NP{space}");
            let recovery = format!("{long}1100{space}ABCD");
            assert!(
                VerificationCode::parse(&six).is_ok(),
                "U+{:04X}",
                u32::from(space)
            );
            assert!(
                fits(VerificationCode::schema(), &six),
                "U+{:04X}",
                u32::from(space)
            );
            assert!(AppCode::parse(&six).is_ok());
            assert!(fits(AppCode::schema(), &six), "U+{:04X}", u32::from(space));
            assert!(ResetCode::parse(&long).is_ok() && ChangeCode::parse(&long).is_ok());
            assert!(
                fits(ResetCode::schema(), &long),
                "U+{:04X}",
                u32::from(space)
            );
            assert!(
                fits(ChangeCode::schema(), &long),
                "U+{:04X}",
                u32::from(space)
            );
            assert!(RecoveryCode::parse(&recovery).is_ok());
            assert!(
                fits(RecoveryCode::schema(), &recovery),
                "U+{:04X}",
                u32::from(space)
            );
        }
    }
}
