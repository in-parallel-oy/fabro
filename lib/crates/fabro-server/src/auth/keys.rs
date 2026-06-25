use cookie::Key;
use hkdf::Hkdf;
use jsonwebtoken::{DecodingKey, EncodingKey};
use sha2::Sha256;

const COOKIE_KEY_INFO: &[u8] = b"fabro-cookie-v1";
const JWT_KEY_INFO: &[u8] = b"fabro-jwt-hs256-v1";
const WORKER_JWT_KEY_INFO: &[u8] = b"fabro-worker-jwt-v1";
const MIN_MASTER_BYTES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JwtSigningKey([u8; 32]);

impl JwtSigningKey {
    #[cfg(test)]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn encoding_key(&self) -> EncodingKey {
        EncodingKey::from_secret(&self.0)
    }

    pub(crate) fn decoding_key(&self) -> DecodingKey {
        DecodingKey::from_secret(&self.0)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum KeyDeriveError {
    #[error("SESSION_SECRET must not be empty")]
    Empty,
    #[error("SESSION_SECRET must be at least {min_bytes} bytes, got {got_bytes}")]
    TooShort { got_bytes: usize, min_bytes: usize },
}

pub(crate) fn derive_cookie_key(master: &[u8]) -> Result<Key, KeyDeriveError> {
    let bytes = derive_bytes::<64>(master, COOKIE_KEY_INFO)?;
    Ok(Key::from(bytes.as_ref()))
}

pub(crate) fn derive_jwt_key(master: &[u8]) -> Result<JwtSigningKey, KeyDeriveError> {
    Ok(JwtSigningKey(derive_bytes::<32>(master, JWT_KEY_INFO)?))
}

pub(crate) fn derive_worker_jwt_key(master: &[u8]) -> Result<[u8; 32], KeyDeriveError> {
    derive_bytes::<32>(master, WORKER_JWT_KEY_INFO)
}

fn derive_bytes<const N: usize>(master: &[u8], info: &[u8]) -> Result<[u8; N], KeyDeriveError> {
    validate_master(master)?;

    let hkdf = Hkdf::<Sha256>::new(None, master);
    let mut output = [0_u8; N];
    hkdf.expand(info, &mut output)
        .expect("fixed-size HKDF output should always be valid");
    Ok(output)
}

fn validate_master(master: &[u8]) -> Result<(), KeyDeriveError> {
    if master.is_empty() {
        return Err(KeyDeriveError::Empty);
    }
    if master.len() < MIN_MASTER_BYTES {
        return Err(KeyDeriveError::TooShort {
            got_bytes: master.len(),
            min_bytes: MIN_MASTER_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use cookie::{Cookie, CookieJar};

    use super::{KeyDeriveError, derive_cookie_key, derive_jwt_key, derive_worker_jwt_key};

    #[test]
    fn derives_same_cookie_key_for_same_master() {
        let master = [0x61; 32];

        let first = derive_cookie_key(&master).expect("first derivation should succeed");
        let second = derive_cookie_key(&master).expect("second derivation should succeed");

        assert_eq!(first.master(), second.master());
    }

    #[test]
    fn derives_different_cookie_and_jwt_subkeys() {
        let master = [0x61; 32];

        let cookie_key = derive_cookie_key(&master).expect("cookie derivation should succeed");
        let jwt_key = derive_jwt_key(&master).expect("jwt derivation should succeed");

        assert_ne!(cookie_key.master(), jwt_key.as_bytes());
        assert_eq!(cookie_key.master().len(), 64);
        assert_eq!(jwt_key.as_bytes().len(), 32);
    }

    #[test]
    fn derives_different_worker_and_user_jwt_subkeys() {
        let master = [0x61; 32];

        let user_key = derive_jwt_key(&master).expect("jwt derivation should succeed");
        let worker_key =
            derive_worker_jwt_key(&master).expect("worker jwt derivation should succeed");

        assert_ne!(user_key.as_bytes(), worker_key);
        assert_eq!(worker_key.len(), 32);
    }

    #[test]
    fn rejects_empty_master_secret() {
        let err = derive_cookie_key(&[]).expect_err("empty secret should fail");
        assert_eq!(err, KeyDeriveError::Empty);
    }

    #[test]
    fn rejects_short_master_secret() {
        let err = derive_jwt_key(&[0x61; 31]).expect_err("short secret should fail");
        assert_eq!(
            err,
            KeyDeriveError::TooShort {
                got_bytes: 31,
                min_bytes: 32,
            }
        );
    }

    #[test]
    fn worker_derivation_rejects_empty_master_secret() {
        let err = derive_worker_jwt_key(&[]).expect_err("empty secret should fail");
        assert_eq!(err, KeyDeriveError::Empty);
    }

    #[test]
    fn worker_derivation_rejects_short_master_secret() {
        let err = derive_worker_jwt_key(&[0x61; 31]).expect_err("short secret should fail");
        assert_eq!(
            err,
            KeyDeriveError::TooShort {
                got_bytes: 31,
                min_bytes: 32,
            }
        );
    }

    #[test]
    fn derived_cookie_key_round_trips_private_cookie() {
        let key = derive_cookie_key(&[0x61; 32]).expect("derivation should succeed");
        let mut jar = CookieJar::new();

        jar.private_mut(&key)
            .add(cookie::Cookie::new("session", "encrypted"));

        let cookie = jar
            .delta()
            .next()
            .expect("private cookie should be emitted")
            .clone();

        let mut verify_jar = CookieJar::new();
        verify_jar.add_original(cookie);

        assert_eq!(
            verify_jar
                .private(&key)
                .get("session")
                .as_ref()
                .map(Cookie::value),
            Some("encrypted")
        );
    }
}
