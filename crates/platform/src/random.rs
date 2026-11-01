//! Random values from the operating system's secure random source (CODE-06, SEC-03).

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::errors::Internal;

/// `N` bytes from the operating system's secure random source, such as a password hash's salt.
pub fn bytes<const N: usize>() -> Result<[u8; N], Internal> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|error| Internal::new(error.to_string()))?;
    Ok(bytes)
}

/// A new token: 256 bits from the operating system's secure random source, as 43 characters of
/// unpadded base64url (SEC-03).
pub fn token() -> Result<String, Internal> {
    Ok(URL_SAFE_NO_PAD.encode(bytes::<32>()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_03_a_token_holds_256_random_bits() {
        let first = token().unwrap();
        let decoded = URL_SAFE_NO_PAD.decode(&first).unwrap();
        assert_eq!(decoded.len(), 32);
        assert_eq!(first.len(), 43);
        assert_ne!(first, token().unwrap());
    }
}
