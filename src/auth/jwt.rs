use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};

use super::AuthError;
use super::scope::ScopeSet;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    pub fn contains(&self, expected: &str) -> bool {
        match self {
            Self::One(audience) => audience == expected,
            Self::Many(audiences) => audiences.iter().any(|audience| audience == expected),
        }
    }

    pub fn values(&self) -> Vec<String> {
        match self {
            Self::One(audience) => vec![audience.clone()],
            Self::Many(audiences) => audiences.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claims {
    pub exp: u64,
    pub nbf: u64,
    pub iat: u64,
    pub iss: String,
    pub aud: Audience,
    pub sub: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    #[serde(default)]
    pub scp: Option<Vec<String>>,
}

impl Claims {
    fn scope_set(&self) -> ScopeSet {
        let mut scopes = ScopeSet::default();

        if let Some(scope) = &self.scope {
            scopes.extend(scope.split_whitespace());
        }
        if let Some(scope_values) = &self.scopes {
            scopes.extend(scope_values.iter().map(String::as_str));
        }
        if let Some(scope_values) = &self.scp {
            scopes.extend(scope_values.iter().map(String::as_str));
        }

        scopes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub subject: String,
    pub issuer: String,
    pub audience: Vec<String>,
    pub scopes: ScopeSet,
}

impl Principal {
    pub fn require_scope(&self, scope: &str) -> Result<(), AuthError> {
        self.scopes.require(scope)
    }
}

pub struct JwtVerifier {
    issuer: String,
    audience: String,
    keys: HashMap<String, DecodingKey>,
    leeway_seconds: u64,
}

impl JwtVerifier {
    pub fn new_hs256<I, K, S>(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        keys: I,
    ) -> Result<Self, AuthError>
    where
        I: IntoIterator<Item = (K, S)>,
        K: Into<String>,
        S: AsRef<str>,
    {
        let keys = keys
            .into_iter()
            .map(|(kid, secret)| {
                (
                    kid.into(),
                    DecodingKey::from_secret(secret.as_ref().as_bytes()),
                )
            })
            .collect::<HashMap<_, _>>();

        if keys.is_empty() {
            return Err(AuthError::MissingKeyId);
        }

        Ok(Self {
            issuer: issuer.into(),
            audience: audience.into(),
            keys,
            leeway_seconds: 0,
        })
    }

    pub fn with_leeway_seconds(mut self, leeway_seconds: u64) -> Self {
        self.leeway_seconds = leeway_seconds;
        self
    }

    pub fn verify_bearer(&self, authorization: Option<&str>) -> Result<Principal, AuthError> {
        let token = bearer_token(authorization)?;
        self.verify_token(token)
    }

    pub fn verify_bearer_with_scope(
        &self,
        authorization: Option<&str>,
        required_scope: &str,
    ) -> Result<Principal, AuthError> {
        let principal = self.verify_bearer(authorization)?;
        principal.require_scope(required_scope)?;
        Ok(principal)
    }

    pub fn verify_token(&self, token: &str) -> Result<Principal, AuthError> {
        let header = decode_header(token).map_err(|_| AuthError::InvalidToken)?;

        if header.alg != Algorithm::HS256 {
            return Err(AuthError::UnsupportedAlgorithm);
        }

        let kid = header.kid.ok_or(AuthError::MissingKeyId)?;
        let key = self.keys.get(&kid).ok_or(AuthError::UnknownKeyId)?;

        let claims = decode::<Claims>(token, key, &self.validation())
            .map_err(map_jwt_error)?
            .claims;

        self.validate_claims(&claims)?;

        let audience = claims.aud.values();
        let scopes = claims.scope_set();

        Ok(Principal {
            subject: claims.sub,
            issuer: claims.iss,
            audience,
            scopes,
        })
    }

    pub fn verify_token_with_scope(
        &self,
        token: &str,
        required_scope: &str,
    ) -> Result<Principal, AuthError> {
        let principal = self.verify_token(token)?;
        principal.require_scope(required_scope)?;
        Ok(principal)
    }

    fn validation(&self) -> Validation {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = self.leeway_seconds;
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.set_required_spec_claims(&["exp", "nbf", "iat", "iss", "aud", "sub"]);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_audience(&[self.audience.as_str()]);
        validation
    }

    fn validate_claims(&self, claims: &Claims) -> Result<(), AuthError> {
        if claims.iss != self.issuer {
            return Err(AuthError::InvalidIssuer);
        }
        if !claims.aud.contains(&self.audience) {
            return Err(AuthError::InvalidAudience);
        }
        if claims.sub.trim().is_empty() {
            return Err(AuthError::InvalidSubject);
        }

        let now = current_timestamp();
        if claims.iat > now.saturating_add(self.leeway_seconds) {
            return Err(AuthError::InvalidIssuedAt);
        }
        if claims.nbf > claims.exp {
            return Err(AuthError::TokenNotYetValid);
        }
        if claims.iat > claims.exp {
            return Err(AuthError::InvalidIssuedAt);
        }

        Ok(())
    }
}

fn bearer_token(authorization: Option<&str>) -> Result<&str, AuthError> {
    let authorization = authorization
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(AuthError::MissingBearerToken)?;

    let (scheme, token) = authorization
        .split_once(' ')
        .ok_or(AuthError::InvalidAuthorizationScheme)?;

    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthError::InvalidAuthorizationScheme);
    }

    let token = token.trim();
    if token.is_empty() || token.split_whitespace().count() != 1 {
        return Err(AuthError::MalformedBearerToken);
    }

    Ok(token)
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn map_jwt_error(error: jsonwebtoken::errors::Error) -> AuthError {
    match error.kind() {
        ErrorKind::ExpiredSignature => AuthError::TokenExpired,
        ErrorKind::ImmatureSignature => AuthError::TokenNotYetValid,
        ErrorKind::InvalidIssuer => AuthError::InvalidIssuer,
        ErrorKind::InvalidAudience => AuthError::InvalidAudience,
        ErrorKind::InvalidSubject => AuthError::InvalidSubject,
        ErrorKind::InvalidAlgorithm => AuthError::UnsupportedAlgorithm,
        ErrorKind::MissingRequiredClaim(claim) => AuthError::MissingClaim(claim.to_owned()),
        _ => AuthError::InvalidToken,
    }
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    use super::*;
    use crate::auth::{COLLECTIONS_CREATE_SCOPE, ORDERS_CREATE_SCOPE, ORDERS_READ_SCOPE};

    const ISSUER: &str = "pay3-test-issuer";
    const AUDIENCE: &str = "pay3-api";
    const KID: &str = "test-key";
    const SECRET: &str = "test-secret-with-enough-entropy";

    #[test]
    fn valid_token_builds_principal() {
        let verifier = verifier();
        let token = token_with(|claims| {
            claims.scope = Some(format!("{ORDERS_CREATE_SCOPE} {ORDERS_READ_SCOPE}"));
        });

        let principal = verifier
            .verify_bearer(Some(&format!("Bearer {token}")))
            .expect("valid token should verify");

        assert_eq!(principal.subject, "merchant-default");
        assert_eq!(principal.issuer, ISSUER);
        assert_eq!(principal.audience, vec![AUDIENCE.to_owned()]);
        assert!(principal.scopes.contains(ORDERS_CREATE_SCOPE));
        assert!(principal.scopes.contains(ORDERS_READ_SCOPE));
    }

    #[test]
    fn expired_token_is_rejected() {
        let verifier = verifier();
        let token = token_with(|claims| {
            claims.exp = current_timestamp().saturating_sub(1);
        });

        let err = verifier.verify_token(&token).expect_err("token is expired");

        assert_eq!(err, AuthError::TokenExpired);
    }

    #[test]
    fn not_before_token_is_rejected() {
        let verifier = verifier();
        let token = token_with(|claims| {
            claims.nbf = current_timestamp() + 60;
        });

        let err = verifier
            .verify_token(&token)
            .expect_err("token is not valid yet");

        assert_eq!(err, AuthError::TokenNotYetValid);
    }

    #[test]
    fn future_issued_at_is_rejected() {
        let verifier = verifier();
        let token = token_with(|claims| {
            claims.iat = current_timestamp() + 60;
        });

        let err = verifier
            .verify_token(&token)
            .expect_err("future iat should fail");

        assert_eq!(err, AuthError::InvalidIssuedAt);
    }

    #[test]
    fn issuer_audience_and_subject_are_validated() {
        let verifier = verifier();

        let wrong_issuer = token_with(|claims| {
            claims.iss = "other-issuer".to_owned();
        });
        assert_eq!(
            verifier.verify_token(&wrong_issuer),
            Err(AuthError::InvalidIssuer)
        );

        let wrong_audience = token_with(|claims| {
            claims.aud = Audience::One("other-api".to_owned());
        });
        assert_eq!(
            verifier.verify_token(&wrong_audience),
            Err(AuthError::InvalidAudience)
        );

        let empty_subject = token_with(|claims| {
            claims.sub = "  ".to_owned();
        });
        assert_eq!(
            verifier.verify_token(&empty_subject),
            Err(AuthError::InvalidSubject)
        );
    }

    #[test]
    fn bearer_token_is_required() {
        let verifier = verifier();
        let token = token_with(|_| {});

        assert_eq!(
            verifier.verify_bearer(None),
            Err(AuthError::MissingBearerToken)
        );
        assert_eq!(
            verifier.verify_bearer(Some(&format!("Basic {token}"))),
            Err(AuthError::InvalidAuthorizationScheme)
        );
        assert_eq!(
            verifier.verify_bearer(Some(&format!("Bearer {token} extra"))),
            Err(AuthError::MalformedBearerToken)
        );
    }

    #[test]
    fn missing_scope_is_rejected() {
        let verifier = verifier();
        let token = token_with(|claims| {
            claims.scope = None;
            claims.scopes = None;
            claims.scp = None;
        });
        let principal = verifier.verify_token(&token).expect("token should verify");

        let err = principal
            .require_scope(ORDERS_CREATE_SCOPE)
            .expect_err("scope is missing");

        assert_eq!(err, AuthError::MissingScope(ORDERS_CREATE_SCOPE.to_owned()));
    }

    #[test]
    fn insufficient_scope_is_rejected() {
        let verifier = verifier();
        let token = token_with(|claims| {
            claims.scope = Some(ORDERS_READ_SCOPE.to_owned());
        });
        let principal = verifier.verify_token(&token).expect("token should verify");

        let err = principal
            .require_scope(ORDERS_CREATE_SCOPE)
            .expect_err("scope is insufficient");

        assert_eq!(
            err,
            AuthError::InsufficientScope(ORDERS_CREATE_SCOPE.to_owned())
        );
    }

    #[test]
    fn collections_create_is_isolated_from_order_scopes() {
        let verifier = verifier();
        let orders_token = token_with(|claims| {
            claims.scope = Some(ORDERS_CREATE_SCOPE.to_owned());
        });
        let collections_token = token_with(|claims| {
            claims.scope = Some(COLLECTIONS_CREATE_SCOPE.to_owned());
        });

        let orders_principal = verifier
            .verify_token(&orders_token)
            .expect("orders token should verify");
        let collections_principal = verifier
            .verify_token(&collections_token)
            .expect("collections token should verify");

        assert_eq!(
            orders_principal.require_scope(COLLECTIONS_CREATE_SCOPE),
            Err(AuthError::InsufficientScope(
                COLLECTIONS_CREATE_SCOPE.to_owned()
            ))
        );
        assert_eq!(
            collections_principal.require_scope(ORDERS_CREATE_SCOPE),
            Err(AuthError::InsufficientScope(ORDERS_CREATE_SCOPE.to_owned()))
        );
        assert!(
            collections_principal
                .require_scope(COLLECTIONS_CREATE_SCOPE)
                .is_ok()
        );
    }

    #[test]
    fn wrong_kid_is_rejected() {
        let verifier = verifier();
        let token = signed_token(default_claims(), "other-key", Algorithm::HS256, SECRET);

        let err = verifier
            .verify_token(&token)
            .expect_err("unknown kid should fail");

        assert_eq!(err, AuthError::UnknownKeyId);
    }

    #[test]
    fn wrong_alg_is_rejected() {
        let verifier = verifier();
        let token = signed_token(
            default_claims(),
            KID,
            Algorithm::HS384,
            "different-secret-for-hs384",
        );

        let err = verifier
            .verify_token(&token)
            .expect_err("wrong algorithm should fail before signature verification");

        assert_eq!(err, AuthError::UnsupportedAlgorithm);
    }

    fn verifier() -> JwtVerifier {
        JwtVerifier::new_hs256(ISSUER, AUDIENCE, [(KID, SECRET)]).expect("verifier")
    }

    fn token_with(update: impl FnOnce(&mut Claims)) -> String {
        let mut claims = default_claims();
        update(&mut claims);
        signed_token(claims, KID, Algorithm::HS256, SECRET)
    }

    fn default_claims() -> Claims {
        let now = current_timestamp();
        Claims {
            exp: now + 3600,
            nbf: now.saturating_sub(1),
            iat: now.saturating_sub(1),
            iss: ISSUER.to_owned(),
            aud: Audience::One(AUDIENCE.to_owned()),
            sub: "merchant-default".to_owned(),
            scope: Some(ORDERS_CREATE_SCOPE.to_owned()),
            scopes: None,
            scp: None,
        }
    }

    fn signed_token(claims: Claims, kid: &str, alg: Algorithm, secret: &str) -> String {
        let mut header = Header::new(alg);
        header.kid = Some(kid.to_owned());
        encode(
            &header,
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("token should encode")
    }
}
