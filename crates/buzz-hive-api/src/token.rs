//! HS256 Beehive tokens and EdDSA requester claims. Expiry is checked against an injected
//! clock so tests control time.

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

/// A signed statement of who an agent is acting for. An agent asks for one with
/// `/hive/v1/claim` and sends it as `x-hive-claim`. The requester's level is read when the
/// claim is used, so a claim carries no level of its own.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RequesterClaim {
    /// The person the agent acts for.
    pub sub: String,
    pub community: uuid::Uuid,
    /// The agent the claim was issued to. Anyone else who holds it gets nothing from it.
    pub agent: String,
    /// What the claim rests on: `chat`, or `run:<id>`.
    pub via: String,
    pub iat: i64,
    pub exp: i64,
}

/// How long a claim is good for.
pub const CLAIM_LIFETIME_SECS: i64 = 5 * 60;
/// The `kid` the signing key is published under.
pub const CLAIM_KEY_ID: &str = "beehive-claims-1";

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
    claim_enc: jsonwebtoken::EncodingKey,
    claim_dec: jsonwebtoken::DecodingKey,
    /// The claim key's public half, for `/.well-known/jwks.json`.
    claim_public: [u8; 32],
    clock: Arc<dyn Clock>,
}

/// The claim signing key, derived from the token secret so every instance that shares the
/// secret signs and verifies alike and there is no second secret to store. The label keeps
/// the key apart from the HS256 use of the same secret.
fn claim_keys(
    secret: &[u8],
) -> Result<
    (
        jsonwebtoken::EncodingKey,
        jsonwebtoken::DecodingKey,
        [u8; 32],
    ),
    String,
> {
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, secret);
    let seed = aws_lc_rs::hmac::sign(&key, b"buzz-hive requester claim signing key v1");
    let pair = Ed25519KeyPair::from_seed_unchecked(seed.as_ref())
        .map_err(|_| "could not derive the claim signing key")?;
    let public: [u8; 32] = pair
        .public_key()
        .as_ref()
        .try_into()
        .map_err(|_| "the claim public key is not 32 bytes")?;
    let der = pair
        .to_pkcs8()
        .map_err(|_| "could not encode the claim signing key")?;
    Ok((
        jsonwebtoken::EncodingKey::from_ed_der(der.as_ref()),
        jsonwebtoken::DecodingKey::from_ed_der(&public),
        public,
    ))
}

impl TokenMinter {
    pub fn new(secret: &[u8], clock: Arc<dyn Clock>) -> Result<Self, String> {
        if secret.len() < MIN_SECRET_BYTES {
            return Err("BUZZ_HIVE_TOKEN_SECRET must be at least 32 bytes".into());
        }
        let (claim_enc, claim_dec, claim_public) = claim_keys(secret)?;
        Ok(Self {
            key_enc: jsonwebtoken::EncodingKey::from_secret(secret),
            key_dec: jsonwebtoken::DecodingKey::from_secret(secret),
            claim_enc,
            claim_dec,
            claim_public,
            clock,
        })
    }

    /// Seconds since the epoch on this minter's clock.
    pub fn now(&self) -> i64 {
        self.clock.now()
    }

    /// A claim that `agent` acts for `sub`, good for [`CLAIM_LIFETIME_SECS`]. Returns it
    /// with its expiry in seconds.
    pub fn sign_claim(
        &self,
        sub: &str,
        community: uuid::Uuid,
        agent: &str,
        via: &str,
    ) -> Result<(String, i64), HiveError> {
        let iat = self.clock.now();
        let exp = iat + CLAIM_LIFETIME_SECS;
        let claim = RequesterClaim {
            sub: sub.to_owned(),
            community,
            agent: agent.to_owned(),
            via: via.to_owned(),
            iat,
            exp,
        };
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
        header.kid = Some(CLAIM_KEY_ID.to_owned());
        let token =
            jsonwebtoken::encode(&header, &claim, &self.claim_enc).map_err(HiveError::internal)?;
        Ok((token, exp))
    }

    /// The claim in `token`, when it is signed by this service's key and has not expired.
    pub fn verify_claim(&self, token: &str) -> Result<RequesterClaim, HiveError> {
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
        validation.validate_exp = false; // checked below against the injected clock
        validation.required_spec_claims.clear();
        let claim = jsonwebtoken::decode::<RequesterClaim>(token, &self.claim_dec, &validation)
            .map_err(|_| HiveError::Unauthenticated("invalid requester claim".into()))?
            .claims;
        if claim.exp <= self.clock.now() {
            return Err(HiveError::Unauthenticated("requester claim expired".into()));
        }
        Ok(claim)
    }

    /// The public claim key as a JWK set, for a server that checks claims without calling
    /// back here.
    pub fn jwks(&self) -> serde_json::Value {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
        use base64::Engine as _;
        serde_json::json!({ "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "alg": "EdDSA",
            "use": "sig",
            "kid": CLAIM_KEY_ID,
            "x": B64.encode(self.claim_public),
        }]})
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
    fn a_signed_claim_verifies_until_it_expires() {
        let (minter, clock) = minter(SECRET, 1_000);
        let community = uuid::Uuid::from_u128(7);
        let (claim, exp) = minter.sign_claim("pp", community, "aa", "chat").unwrap();
        assert_eq!(exp, 1_000 + CLAIM_LIFETIME_SECS);
        let read = minter.verify_claim(&claim).unwrap();
        assert_eq!(
            (read.sub.as_str(), read.agent.as_str(), read.via.as_str()),
            ("pp", "aa", "chat")
        );
        assert_eq!(read.community, community);
        clock.0.store(exp, Ordering::SeqCst);
        assert!(minter.verify_claim(&claim).is_err());
    }

    #[test]
    fn a_claim_from_another_secret_or_a_bearer_token_is_refused() {
        let (mine, _) = minter(SECRET, 1_000);
        let (other, _) = minter(b"ffffffffffffffffffffffffffffffff", 1_000);
        let (forged, _) = other
            .sign_claim("pp", uuid::Uuid::nil(), "aa", "chat")
            .unwrap();
        assert!(mine.verify_claim(&forged).is_err());
        let (bearer, _) = mine.mint("aa", uuid::Uuid::nil(), None).unwrap();
        assert!(mine.verify_claim(&bearer).is_err());
        // And the other way: a claim is not a bearer token.
        let (claim, _) = mine
            .sign_claim("pp", uuid::Uuid::nil(), "aa", "chat")
            .unwrap();
        assert!(mine.verify(&claim).is_err());
    }

    #[test]
    fn the_published_key_verifies_a_claim() {
        let (minter, _) = minter(SECRET, 1_000);
        let (claim, _) = minter
            .sign_claim("pp", uuid::Uuid::nil(), "aa", "chat")
            .unwrap();
        let jwk = minter.jwks()["keys"][0].clone();
        assert_eq!(jwk["kty"], "OKP");
        assert_eq!(jwk["crv"], "Ed25519");
        let key =
            jsonwebtoken::DecodingKey::from_ed_components(jwk["x"].as_str().unwrap()).unwrap();
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        let read = jsonwebtoken::decode::<RequesterClaim>(&claim, &key, &validation).unwrap();
        assert_eq!(read.claims.sub, "pp");
        let header = jsonwebtoken::decode_header(&claim).unwrap();
        assert_eq!(header.kid.as_deref(), Some(CLAIM_KEY_ID));
    }

    #[test]
    fn garbage_is_refused() {
        let (minter, _) = minter(SECRET, 1_000);
        assert!(minter.verify("not a token").is_err());
        assert!(minter.verify("").is_err());
    }
}
