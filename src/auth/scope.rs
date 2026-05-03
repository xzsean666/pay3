use std::collections::BTreeSet;
use std::iter::FromIterator;

use super::AuthError;

pub const ORDERS_CREATE_SCOPE: &str = "orders:create";
pub const ORDERS_READ_SCOPE: &str = "orders:read";
pub const ORDERS_VERIFY_SCOPE: &str = "orders:verify";
pub const COLLECTIONS_CREATE_SCOPE: &str = "collections:create";
pub const COLLECTIONS_READ_SCOPE: &str = "collections:read";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeSet {
    scopes: BTreeSet<String>,
}

impl ScopeSet {
    pub fn new<I, S>(scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let scopes = scopes
            .into_iter()
            .map(Into::into)
            .map(|scope| scope.trim().to_owned())
            .filter(|scope| !scope.is_empty())
            .collect();

        Self { scopes }
    }

    pub fn parse(scope_claim: &str) -> Self {
        Self::new(scope_claim.split_whitespace())
    }

    pub fn insert(&mut self, scope: impl Into<String>) {
        let scope = scope.into();
        let scope = scope.trim();
        if !scope.is_empty() {
            self.scopes.insert(scope.to_owned());
        }
    }

    pub fn extend<I, S>(&mut self, scopes: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for scope in scopes {
            self.insert(scope);
        }
    }

    pub fn contains(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }

    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.scopes.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.scopes.iter().map(String::as_str)
    }

    pub fn require(&self, scope: &str) -> Result<(), AuthError> {
        if self.contains(scope) {
            Ok(())
        } else if self.is_empty() {
            Err(AuthError::MissingScope(scope.to_owned()))
        } else {
            Err(AuthError::InsufficientScope(scope.to_owned()))
        }
    }
}

impl FromIterator<String> for ScopeSet {
    fn from_iter<T: IntoIterator<Item = String>>(iter: T) -> Self {
        Self::new(iter)
    }
}

impl<'a> FromIterator<&'a str> for ScopeSet {
    fn from_iter<T: IntoIterator<Item = &'a str>>(iter: T) -> Self {
        Self::new(iter)
    }
}

pub fn require_scope(principal: &super::Principal, scope: &str) -> Result<(), AuthError> {
    principal.scopes.require(scope)
}
