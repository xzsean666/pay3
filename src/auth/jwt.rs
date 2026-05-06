use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::jwk::{JwkSet, KeyAlgorithm};
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
    keys: HashMap<String, VerificationKey>,
    leeway_seconds: u64,
}

struct VerificationKey {
    algorithm: Algorithm,
    key: DecodingKey,
}

impl JwtVerifier {
    pub fn new<I, K>(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        keys: I,
    ) -> Result<Self, AuthError>
    where
        I: IntoIterator<Item = (K, Algorithm, DecodingKey)>,
        K: Into<String>,
    {
        let mut verification_keys = HashMap::new();

        for (kid, algorithm, key) in keys {
            let kid = kid.into();
            if kid.trim().is_empty() {
                return Err(AuthError::MissingKeyId);
            }
            if verification_keys
                .insert(kid.clone(), VerificationKey { algorithm, key })
                .is_some()
            {
                return Err(AuthError::DuplicateKeyId(kid));
            }
        }

        if verification_keys.is_empty() {
            return Err(AuthError::MissingKeyId);
        }

        Ok(Self {
            issuer: issuer.into(),
            audience: audience.into(),
            keys: verification_keys,
            leeway_seconds: 0,
        })
    }

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
        Self::new(
            issuer,
            audience,
            keys.into_iter().map(|(kid, secret)| {
                (
                    kid,
                    Algorithm::HS256,
                    DecodingKey::from_secret(secret.as_ref().as_bytes()),
                )
            }),
        )
    }

    pub fn new_asymmetric_pem(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        kid: impl Into<String>,
        algorithm: Algorithm,
        public_key_pem: impl AsRef<str>,
    ) -> Result<Self, AuthError> {
        let key = match algorithm {
            Algorithm::RS256 => DecodingKey::from_rsa_pem(public_key_pem.as_ref().as_bytes()),
            Algorithm::EdDSA => DecodingKey::from_ed_pem(public_key_pem.as_ref().as_bytes()),
            _ => return Err(AuthError::UnsupportedAlgorithm),
        }
        .map_err(|_| AuthError::InvalidKey)?;

        Self::new(issuer, audience, [(kid, algorithm, key)])
    }

    pub fn from_jwks_json(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        jwks_json: impl AsRef<str>,
    ) -> Result<Self, AuthError> {
        let jwks = serde_json::from_str::<JwkSet>(jwks_json.as_ref())
            .map_err(|_| AuthError::InvalidJwks)?;
        let keys = jwks
            .keys
            .iter()
            .map(|jwk| {
                let kid = jwk
                    .common
                    .key_id
                    .as_deref()
                    .filter(|kid| !kid.trim().is_empty())
                    .ok_or(AuthError::MissingKeyId)?;
                let algorithm = jwk_algorithm(jwk.common.key_algorithm)?;
                let key = DecodingKey::from_jwk(jwk).map_err(|_| AuthError::InvalidKey)?;

                Ok((kid.to_owned(), algorithm, key))
            })
            .collect::<Result<Vec<_>, AuthError>>()?;

        Self::new(issuer, audience, keys)
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

        let kid = header.kid.ok_or(AuthError::MissingKeyId)?;
        let key = self.keys.get(&kid).ok_or(AuthError::UnknownKeyId)?;
        if header.alg != key.algorithm {
            return Err(AuthError::UnsupportedAlgorithm);
        }

        let claims = decode::<Claims>(token, &key.key, &self.validation(key.algorithm))
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

    fn validation(&self, algorithm: Algorithm) -> Validation {
        let mut validation = Validation::new(algorithm);
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

fn jwk_algorithm(algorithm: Option<KeyAlgorithm>) -> Result<Algorithm, AuthError> {
    match algorithm {
        Some(KeyAlgorithm::RS256) => Ok(Algorithm::RS256),
        Some(KeyAlgorithm::EdDSA) => Ok(Algorithm::EdDSA),
        _ => Err(AuthError::UnsupportedAlgorithm),
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
    use crate::auth::{
        COLLECTIONS_CREATE_SCOPE, COLLECTIONS_READ_SCOPE, ORDERS_CREATE_SCOPE, ORDERS_READ_SCOPE,
    };

    const ISSUER: &str = "pay3-test-issuer";
    const AUDIENCE: &str = "pay3-api";
    const KID: &str = "test-key";
    const SECRET: &str = "test-secret-with-enough-entropy";
    const RSA_KID: &str = "rsa-key";
    const ED_KID: &str = "ed-key";
    const RSA_PRIVATE_KEY_PEM: &str = r#"-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEAyRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTL
UTv4l4sggh5/CYYi/cvI+SXVT9kPWSKXxJXBXd/4LkvcPuUakBoAkfh+eiFVMh2V
rUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8H
oGfG/AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBI
Mc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi+yUod+j8MtvIj812dkS4QMiRVN/
by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQIDAQABAoIBAHREk0I0O9DvECKd
WUpAmF3mY7oY9PNQiu44Yaf+AoSuyRpRUGTMIgc3u3eivOE8ALX0BmYUO5JtuRNZ
Dpvt4SAwqCnVUinIf6C+eH/wSurCpapSM0BAHp4aOA7igptyOMgMPYBHNA1e9A7j
E0dCxKWMl3DSWNyjQTk4zeRGEAEfbNjHrq6YCtjHSZSLmWiG80hnfnYos9hOr5Jn
LnyS7ZmFE/5P3XVrxLc/tQ5zum0R4cbrgzHiQP5RgfxGJaEi7XcgherCCOgurJSS
bYH29Gz8u5fFbS+Yg8s+OiCss3cs1rSgJ9/eHZuzGEdUZVARH6hVMjSuwvqVTFaE
8AgtleECgYEA+uLMn4kNqHlJS2A5uAnCkj90ZxEtNm3E8hAxUrhssktY5XSOAPBl
xyf5RuRGIImGtUVIr4HuJSa5TX48n3Vdt9MYCprO/iYl6moNRSPt5qowIIOJmIjY
2mqPDfDt/zw+fcDD3lmCJrFlzcnh0uea1CohxEbQnL3cypeLt+WbU6kCgYEAzSp1
9m1ajieFkqgoB0YTpt/OroDx38vvI5unInJlEeOjQ+oIAQdN2wpxBvTrRorMU6P0
7mFUbt1j+Co6CbNiw+X8HcCaqYLR5clbJOOWNR36PuzOpQLkfK8woupBxzW9B8gZ
mY8rB1mbJ+/WTPrEJy6YGmIEBkWylQ2VpW8O4O0CgYEApdbvvfFBlwD9YxbrcGz7
MeNCFbMz+MucqQntIKoKJ91ImPxvtc0y6e/Rhnv0oyNlaUOwJVu0yNgNG117w0g4
t/+Q38mvVC5xV7/cn7x9UMFk6MkqVir3dYGEqIl/OP1grY2Tq9HtB5iyG9L8NIam
QOLMyUqqMUILxdthHyFmiGkCgYEAn9+PjpjGMPHxL0gj8Q8VbzsFtou6b1deIRRA
2CHmSltltR1gYVTMwXxQeUhPMmgkMqUXzs4/WijgpthY44hK1TaZEKIuoxrS70nJ
4WQLf5a9k1065fDsFZD6yGjdGxvwEmlGMZgTwqV7t1I4X0Ilqhav5hcs5apYL7gn
PYPeRz0CgYALHCj/Ji8XSsDoF/MhVhnGdIs2P99NNdmo3R2Pv0CuZbDKMU559LJH
UvrKS8WkuWRDuKrz1W/EQKApFjDGpdqToZqriUFQzwy7mR3ayIiogzNtHcvbDHx8
oFnGY0OFksX/ye0/XGpy2SFxYRwGU98HPYeBvAQQrVjdkzfy7BmXQQ==
-----END RSA PRIVATE KEY-----"#;
    const ED_PRIVATE_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIGrD/e7uKYqSY4twDEsRfMMuLSrODf14dpTiTK6K1YI0
-----END PRIVATE KEY-----"#;
    const ED_PUBLIC_KEY_PEM: &str = r#"-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA2+Jj2UvNCvQiUPNYRgSi0cJSPiJI6Rs6D0UTeEpQVj8=
-----END PUBLIC KEY-----"#;

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
    fn collection_scopes_are_isolated_from_order_scopes() {
        let verifier = verifier();
        let orders_token = token_with(|claims| {
            claims.scope = Some(ORDERS_CREATE_SCOPE.to_owned());
        });
        let collections_token = token_with(|claims| {
            claims.scope = Some(COLLECTIONS_CREATE_SCOPE.to_owned());
        });
        let collections_read_token = token_with(|claims| {
            claims.scope = Some(COLLECTIONS_READ_SCOPE.to_owned());
        });

        let orders_principal = verifier
            .verify_token(&orders_token)
            .expect("orders token should verify");
        let collections_principal = verifier
            .verify_token(&collections_token)
            .expect("collections token should verify");
        let collections_read_principal = verifier
            .verify_token(&collections_read_token)
            .expect("collections read token should verify");

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
        assert_eq!(
            collections_principal.require_scope(COLLECTIONS_READ_SCOPE),
            Err(AuthError::InsufficientScope(
                COLLECTIONS_READ_SCOPE.to_owned()
            ))
        );
        assert!(
            collections_read_principal
                .require_scope(COLLECTIONS_READ_SCOPE)
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

    #[test]
    fn rs256_jwks_token_verifies() {
        let verifier =
            JwtVerifier::from_jwks_json(ISSUER, AUDIENCE, rsa_jwks_json()).expect("jwks verifier");
        let token = signed_asymmetric_token(
            default_claims(),
            RSA_KID,
            Algorithm::RS256,
            EncodingKey::from_rsa_pem(RSA_PRIVATE_KEY_PEM.as_bytes()).expect("rsa key"),
        );

        let principal = verifier.verify_token(&token).expect("token should verify");

        assert_eq!(principal.subject, "merchant-default");
    }

    #[test]
    fn eddsa_pem_token_verifies() {
        let verifier = JwtVerifier::new_asymmetric_pem(
            ISSUER,
            AUDIENCE,
            ED_KID,
            Algorithm::EdDSA,
            ED_PUBLIC_KEY_PEM,
        )
        .expect("eddsa verifier");
        let token = signed_asymmetric_token(
            default_claims(),
            ED_KID,
            Algorithm::EdDSA,
            EncodingKey::from_ed_pem(ED_PRIVATE_KEY_PEM.as_bytes()).expect("ed key"),
        );

        let principal = verifier.verify_token(&token).expect("token should verify");

        assert_eq!(principal.subject, "merchant-default");
    }

    #[test]
    fn asymmetric_wrong_kid_and_alg_are_rejected() {
        let verifier =
            JwtVerifier::from_jwks_json(ISSUER, AUDIENCE, rsa_jwks_json()).expect("jwks verifier");

        let wrong_kid = signed_asymmetric_token(
            default_claims(),
            "unknown-rsa-key",
            Algorithm::RS256,
            EncodingKey::from_rsa_pem(RSA_PRIVATE_KEY_PEM.as_bytes()).expect("rsa key"),
        );
        assert_eq!(
            verifier.verify_token(&wrong_kid),
            Err(AuthError::UnknownKeyId)
        );

        let wrong_alg = signed_token(default_claims(), RSA_KID, Algorithm::HS256, SECRET);
        assert_eq!(
            verifier.verify_token(&wrong_alg),
            Err(AuthError::UnsupportedAlgorithm)
        );
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

    fn signed_asymmetric_token(
        claims: Claims,
        kid: &str,
        alg: Algorithm,
        key: EncodingKey,
    ) -> String {
        let mut header = Header::new(alg);
        header.kid = Some(kid.to_owned());
        encode(&header, &claims, &key).expect("token should encode")
    }

    fn rsa_jwks_json() -> String {
        format!(
            r#"{{
                "keys": [
                    {{
                        "kty": "RSA",
                        "n": "yRE6rHuNR0QbHO3H3Kt2pOKGVhQqGZXInOduQNxXzuKlvQTLUTv4l4sggh5_CYYi_cvI-SXVT9kPWSKXxJXBXd_4LkvcPuUakBoAkfh-eiFVMh2VrUyWyj3MFl0HTVF9KwRXLAcwkREiS3npThHRyIxuy0ZMeZfxVL5arMhw1SRELB8HoGfG_AtH89BIE9jDBHZ9dLelK9a184zAf8LwoPLxvJb3Il5nncqPcSfKDDodMFBIMc4lQzDKL5gvmiXLXB1AGLm8KBjfE8s3L5xqi-yUod-j8MtvIj812dkS4QMiRVN_by2h3ZY8LYVGrqZXZTcgn2ujn8uKjXLZVD5TdQ",
                        "e": "AQAB",
                        "kid": "{RSA_KID}",
                        "alg": "RS256",
                        "use": "sig"
                    }}
                ]
            }}"#
        )
    }
}
