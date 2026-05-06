mod jwt;
mod scope;

pub use jwt::{Audience, Claims, JwtVerifier, Principal};
pub use scope::{
    COLLECTIONS_CREATE_SCOPE, COLLECTIONS_READ_SCOPE, ORDERS_CREATE_SCOPE, ORDERS_READ_SCOPE,
    ORDERS_VERIFY_SCOPE, ScopeSet, require_scope,
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("missing bearer token")]
    MissingBearerToken,
    #[error("invalid authorization scheme")]
    InvalidAuthorizationScheme,
    #[error("malformed bearer token")]
    MalformedBearerToken,
    #[error("missing jwt key id")]
    MissingKeyId,
    #[error("unknown jwt key id")]
    UnknownKeyId,
    #[error("unsupported jwt algorithm")]
    UnsupportedAlgorithm,
    #[error("invalid jwt key")]
    InvalidKey,
    #[error("invalid jwt jwks")]
    InvalidJwks,
    #[error("duplicate jwt key id: {0}")]
    DuplicateKeyId(String),
    #[error("invalid token")]
    InvalidToken,
    #[error("token expired")]
    TokenExpired,
    #[error("token not yet valid")]
    TokenNotYetValid,
    #[error("invalid issuer")]
    InvalidIssuer,
    #[error("invalid audience")]
    InvalidAudience,
    #[error("missing claim: {0}")]
    MissingClaim(String),
    #[error("invalid subject")]
    InvalidSubject,
    #[error("invalid issued-at")]
    InvalidIssuedAt,
    #[error("missing scope: {0}")]
    MissingScope(String),
    #[error("insufficient scope: {0}")]
    InsufficientScope(String),
}
