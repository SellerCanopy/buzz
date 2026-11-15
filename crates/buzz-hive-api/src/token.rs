//! HS256 Beehive tokens. Expiry is checked against an injected clock so tests control time.

use std::sync::Arc;

use crate::error::HiveError;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Claims {
    pub sub: String,
    pub community: uuid::Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub iat: i64,
    pub exp: i64,
}

pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        chrono::Utc::now().timestamp()
    }
}

pub const TOKEN_LIFETIME_SECS: i64 = 15 * 60;
const MIN_SECRET_BYTES: usize = 32;

pub struct TokenMinter {
    key_enc: jsonwebtoken::EncodingKey,
    key_dec: jsonwebtoken::DecodingKey,
    clock: Arc<dyn Clock>,
}

impl TokenMinter {
    pub fn new(secret: &[u8], clock: Arc<dyn Clock>) -> Result<Self, String> {
        if secret.len() < MIN_SECRET_BYTES {
            return Err("BUZZ_HIVE_TOKEN_SECRET must be at least 32 bytes".into());
        }
        Ok(Self {
            key_enc: jsonwebtoken::EncodingKey::from_secret(secret),
            key_dec: jsonwebtoken::DecodingKey::from_secret(secret),
            clock,
        })
    }

    /// A token for `sub`, valid for [`TOKEN_LIFETIME_SECS`]. Returns it with its expiry in seconds.
    pub fn mint(
        &self,
        sub: &str,
        community: uuid::Uuid,
        owner: Option<String>,
    ) -> Result<(String, i64), HiveError> {
        let iat = self.clock.now();
        let exp = iat + TOKEN_LIFETIME_SECS;
        let claims = Claims {
            sub: sub.to_owned(),
            community,
            owner,
            iat,
            exp,
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &self.key_enc,
        )
        .map_err(HiveError::internal)?;
        Ok((token, exp))
    }

    pub fn verify(&self, token: &str) -> Result<Claims, HiveError> {
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
        validation.validate_exp = false; // checked below against the injected clock
        validation.required_spec_claims.clear();
        let claims = jsonwebtoken::decode::<Claims>(token, &self.key_dec, &validation)
            .map_err(|_| HiveError::Unauthenticated("invalid token".into()))?
            .claims;
        if claims.exp <= self.clock.now() {
            return Err(HiveError::Unauthenticated("token expired".into()));
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    struct FakeClock(AtomicI64);
    impl Clock for FakeClock {
        fn now(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn minter(secret: &[u8], at: i64) -> (TokenMinter, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock(AtomicI64::new(at)));
        (TokenMinter::new(secret, clock.clone()).unwrap(), clock)
    }

    #[test]
    fn a_minted_token_verifies() {
        let (minter, _) = minter(SECRET, 1_000);
        let community = uuid::Uuid::from_u128(7);
        let (token, exp) = minter.mint("aa", community, Some("bb".into())).unwrap();
        assert_eq!(exp, 1_000 + TOKEN_LIFETIME_SECS);
        let claims = minter.verify(&token).unwrap();
        assert_eq!(claims.sub, "aa");
        assert_eq!(claims.community, community);
        assert_eq!(claims.owner.as_deref(), Some("bb"));
        assert_eq!(claims.exp, exp);
    }

    #[test]
    fn a_token_without_an_owner_carries_none() {
        let (minter, _) = minter(SECRET, 1_000);
        let (token, _) = minter.mint("aa", uuid::Uuid::nil(), None).unwrap();
        assert_eq!(minter.verify(&token).unwrap().owner, None);
    }

    #[test]
    fn one_second_past_exp_fails() {
        let (minter, clock) = minter(SECRET, 1_000);
        let (token, exp) = minter.mint("aa", uuid::Uuid::nil(), None).unwrap();
        clock.0.store(exp - 1, Ordering::SeqCst);
        assert!(minter.verify(&token).is_ok());
        clock.0.store(exp, Ordering::SeqCst);
        assert!(matches!(
            minter.verify(&token),
            Err(HiveError::Unauthenticated(_))
        ));
        clock.0.store(exp + 1, Ordering::SeqCst);
        assert!(minter.verify(&token).is_err());
    }

    #[test]
    fn a_token_signed_with_another_secret_fails() {
        let (mine, _) = minter(SECRET, 1_000);
        let (other, _) = minter(b"ffffffffffffffffffffffffffffffff", 1_000);
        let (token, _) = other.mint("aa", uuid::Uuid::nil(), None).unwrap();
        assert!(mine.verify(&token).is_err());
    }

    #[test]
    fn a_short_secret_is_refused() {
        let clock = Arc::new(FakeClock(AtomicI64::new(0)));
        assert!(TokenMinter::new(b"too short", clock).is_err());
    }

    #[test]
    fn alg_none_is_refused() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
        use base64::Engine as _;
        let (minter, _) = minter(SECRET, 1_000);
        let header = B64.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let claims = B64.encode(format!(
            r#"{{"sub":"aa","community":"{}","iat":1000,"exp":9999999999}}"#,
            uuid::Uuid::nil()
        ));
        assert!(minter.verify(&format!("{header}.{claims}.")).is_err());
        assert!(minter.verify(&format!("{header}.{claims}")).is_err());
    }

    #[test]
    fn garbage_is_refused() {
        let (minter, _) = minter(SECRET, 1_000);
        assert!(minter.verify("not a token").is_err());
        assert!(minter.verify("").is_err());
    }
}
