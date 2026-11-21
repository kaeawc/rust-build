//! JWT-compatible HS256 tokens.
//!
//! [`Hmac256::verify`] checks the signature (constant time, via `ring`), pins the algorithm to
//! `HS256` (rejecting `alg=none` and algorithm substitution), and enforces `exp`/`nbf` against
//! an injected [`Clock`] with optional leeway. RSA/ECDSA are intentionally out of scope.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rb_core::clock::{SharedClock, System};
use ring::hmac;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("token is malformed")]
    Malformed,
    #[error("token signature is invalid")]
    Signature,
    #[error("token algorithm is not HS256")]
    Algorithm,
    #[error("token has expired")]
    Expired,
    #[error("token is not valid yet")]
    NotYetValid,
}

/// Registered claims. Embed with `#[serde(flatten)]` in your own claims type.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StandardClaims {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iat: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
}

/// Claims types expose their registered claims so `verify` can check time bounds.
pub trait Claims: Serialize + DeserializeOwned {
    fn standard(&self) -> &StandardClaims;
}

impl Claims for StandardClaims {
    fn standard(&self) -> &StandardClaims {
        self
    }
}

#[derive(Serialize, Deserialize)]
struct Header {
    alg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    typ: Option<String>,
}

pub struct Hmac256 {
    key: hmac::Key,
    clock: SharedClock,
    leeway: Duration,
}

impl Hmac256 {
    pub fn new(secret: &[u8]) -> Self {
        Hmac256 { key: hmac::Key::new(hmac::HMAC_SHA256, secret), clock: Arc::new(System), leeway: Duration::ZERO }
    }

    pub fn with_clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// Tolerated clock skew for `exp` and `nbf`.
    pub fn with_leeway(mut self, leeway: Duration) -> Self {
        self.leeway = leeway;
        self
    }

    /// Current Unix time in seconds, per the injected clock (for filling `iat`/`exp`).
    pub fn now_unix(&self) -> u64 {
        self.clock.unix().as_secs()
    }

    pub fn sign<C: Claims>(&self, claims: &C) -> Result<String, Error> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).map_err(|_| Error::Malformed)?);
        let signing_input = format!("{header}.{payload}");
        let tag = hmac::sign(&self.key, signing_input.as_bytes());
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(tag.as_ref())))
    }

    pub fn verify<C: Claims>(&self, token: &str) -> Result<C, Error> {
        let mut parts = token.split('.');
        let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(Error::Malformed);
        };
        let header: Header =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).map_err(|_| Error::Malformed)?).map_err(|_| Error::Malformed)?;
        if header.alg != "HS256" {
            return Err(Error::Algorithm);
        }
        let sig = URL_SAFE_NO_PAD.decode(s).map_err(|_| Error::Malformed)?;
        let signing_input = &token[..h.len() + 1 + p.len()];
        hmac::verify(&self.key, signing_input.as_bytes(), &sig).map_err(|_| Error::Signature)?;
        let claims: C = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).map_err(|_| Error::Malformed)?).map_err(|_| Error::Malformed)?;

        let now = self.clock.unix().as_secs();
        let leeway = self.leeway.as_secs();
        let std = claims.standard();
        if std.exp.is_some_and(|exp| now >= exp.saturating_add(leeway)) {
            return Err(Error::Expired);
        }
        if std.nbf.is_some_and(|nbf| now.saturating_add(leeway) < nbf) {
            return Err(Error::NotYetValid);
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_core::clock::Fake;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct MyClaims {
        #[serde(flatten)]
        std: StandardClaims,
        role: String,
    }

    impl Claims for MyClaims {
        fn standard(&self) -> &StandardClaims {
            &self.std
        }
    }

    fn signer(clock: &Fake) -> Hmac256 {
        Hmac256::new(b"secret").with_clock(Arc::new(clock.clone()))
    }

    #[test]
    fn round_trips_custom_claims() {
        let clock = Fake::at_unix(1000);
        let s = signer(&clock);
        let claims =
            MyClaims { std: StandardClaims { sub: Some("u1".into()), exp: Some(2000), ..Default::default() }, role: "admin".into() };
        let token = s.sign(&claims).unwrap_or_default();
        assert_eq!(token.split('.').count(), 3);
        assert_eq!(s.verify::<MyClaims>(&token), Ok(claims));
    }

    #[test]
    fn enforces_expiry_and_not_before_with_leeway() {
        let clock = Fake::at_unix(1000);
        let s = signer(&clock);
        let t = s.sign(&StandardClaims { exp: Some(1010), nbf: Some(1005), ..Default::default() }).unwrap_or_default();
        assert_eq!(s.verify::<StandardClaims>(&t), Err(Error::NotYetValid));
        clock.advance(Duration::from_secs(5));
        assert!(s.verify::<StandardClaims>(&t).is_ok());
        clock.advance(Duration::from_secs(5));
        assert_eq!(s.verify::<StandardClaims>(&t), Err(Error::Expired));
        let lenient = signer(&clock).with_leeway(Duration::from_secs(30));
        assert!(lenient.verify::<StandardClaims>(&t).is_ok());
    }

    #[test]
    fn rejects_tampering_wrong_key_and_alg_none() {
        let clock = Fake::at_unix(1000);
        let s = signer(&clock);
        let t = s.sign(&StandardClaims { sub: Some("a".into()), ..Default::default() }).unwrap_or_default();
        let other = Hmac256::new(b"other").with_clock(Arc::new(clock.clone()));
        assert_eq!(other.verify::<StandardClaims>(&t), Err(Error::Signature));

        let mut parts: Vec<&str> = t.split('.').collect();
        let forged = URL_SAFE_NO_PAD.encode(br#"{"sub":"admin"}"#);
        parts[1] = &forged;
        assert_eq!(s.verify::<StandardClaims>(&parts.join(".")), Err(Error::Signature));

        let none = format!("{}.{}.", URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#), forged);
        assert_eq!(s.verify::<StandardClaims>(&none), Err(Error::Algorithm));
        assert_eq!(s.verify::<StandardClaims>("a.b"), Err(Error::Malformed));
    }
}
