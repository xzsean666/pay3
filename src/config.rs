use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fmt,
    net::SocketAddr,
    path::PathBuf,
    time::Duration,
};

use crate::domain::{EvmAddress, RawAmount};

const DEFAULT_COLLECTION_GAS_LIMIT: u64 = 80_000;
const DEFAULT_COLLECTION_MAX_FEE_PER_GAS_WEI: u64 = 50_000_000_000;
const DEFAULT_COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI: u64 = 2_000_000_000;
const DEFAULT_COLLECTION_COLLECTOR_REPLACEMENT_STUCK_AFTER_SECS: u64 = 30 * 60;
const DEFAULT_SIGNER_REMOTE_REQUEST_TIMEOUT_SECS: u64 = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppProfile {
    Development,
    Test,
    Staging,
    Production,
}

impl AppProfile {
    fn parse(value: &str) -> Result<Self, ConfigError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dev" | "development" | "local" => Ok(Self::Development),
            "test" | "testing" => Ok(Self::Test),
            "stage" | "staging" => Ok(Self::Staging),
            "prod" | "production" => Ok(Self::Production),
            _ => Err(ConfigError::invalid(
                "APP_PROFILE",
                value,
                "expected one of development, test, staging, production",
            )),
        }
    }

    pub fn is_production(&self) -> bool {
        matches!(self, Self::Production)
    }
}

#[derive(Clone)]
pub struct AppConfig {
    pub profile: AppProfile,
    pub http: HttpConfig,
    pub database: DatabaseConfig,
    pub kvdb: KvdbConfig,
    pub jwt: JwtConfig,
    pub chain: ChainConfig,
    pub collection: CollectionConfig,
    pub collector: CollectorConfig,
    pub signer: SignerConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpConfig {
    pub bind_addr: SocketAddr,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    pub url: String,
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseConfig")
            .field("url", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KvdbConfig {
    pub path: PathBuf,
}

#[derive(Clone, PartialEq, Eq)]
pub struct JwtConfig {
    pub issuer: String,
    pub audience: String,
    pub secret: String,
    pub key_id: Option<String>,
}

impl fmt::Debug for JwtConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JwtConfig")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("secret", &"<redacted>")
            .field("key_id", &self.key_id)
            .finish()
    }
}

#[derive(Clone)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub token_address: EvmAddress,
    pub token_decimals: u8,
    pub token_symbol: String,
    pub treasury_address: EvmAddress,
    pub rpc_http_urls: Vec<String>,
    pub start_block: u64,
    pub min_confirmations: u64,
    pub allow_full_history_replay: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionConfig {
    pub gas_limit: u64,
    pub max_fee_per_gas_wei: RawAmount,
    pub max_priority_fee_per_gas_wei: RawAmount,
}

impl Default for CollectionConfig {
    fn default() -> Self {
        Self {
            gas_limit: DEFAULT_COLLECTION_GAS_LIMIT,
            max_fee_per_gas_wei: RawAmount::from(DEFAULT_COLLECTION_MAX_FEE_PER_GAS_WEI),
            max_priority_fee_per_gas_wei: RawAmount::from(
                DEFAULT_COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI,
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectorConfig {
    pub replacement_stuck_after: Duration,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            replacement_stuck_after: Duration::from_secs(
                DEFAULT_COLLECTION_COLLECTOR_REPLACEMENT_STUCK_AFTER_SECS,
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignerConfig {
    pub mode: SignerMode,
    pub key_ref: String,
    pub remote_endpoint: Option<String>,
    pub remote_request_timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignerMode {
    External,
    Kms,
    Hsm,
    Local,
    Fake,
}

impl SignerMode {
    fn parse(value: &str) -> Result<Self, ConfigError> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "external" | "remote" => Ok(Self::External),
            "kms" | "aws_kms" | "gcp_kms" | "azure_kms" => Ok(Self::Kms),
            "hsm" => Ok(Self::Hsm),
            "local" => Ok(Self::Local),
            "fake" | "test_fake" => Ok(Self::Fake),
            _ => Err(ConfigError::invalid(
                "SIGNER_MODE",
                value,
                "expected one of external, kms, hsm, local, fake",
            )),
        }
    }

    fn is_local_or_fake(&self) -> bool {
        matches!(self, Self::Local | Self::Fake)
    }

    fn is_fake(&self) -> bool {
        matches!(self, Self::Fake)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Missing {
        key: &'static str,
    },
    InvalidValue {
        key: &'static str,
        value: String,
        reason: String,
    },
    Validation {
        errors: Vec<String>,
    },
}

impl ConfigError {
    fn invalid(key: &'static str, value: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidValue {
            key,
            value: value.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { key } => write!(f, "missing required config {key}"),
            Self::InvalidValue { key, reason, .. } => {
                write!(f, "invalid config {key}: {reason}")
            }
            Self::Validation { errors } => {
                write!(f, "invalid profile config: {}", errors.join("; "))
            }
        }
    }
}

impl Error for ConfigError {}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_pairs(env::vars())
    }

    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let values = EnvPairs::new(pairs);
        let profile = match values.optional(&["APP_PROFILE", "PAY3_PROFILE", "PROFILE"]) {
            Some(value) => AppProfile::parse(value)?,
            None => AppProfile::Development,
        };

        let bind_addr = values
            .optional(&["APP_BIND", "HTTP_BIND_ADDR", "BIND_ADDR"])
            .unwrap_or("127.0.0.1:8080")
            .parse::<SocketAddr>()
            .map_err(|_| {
                ConfigError::invalid(
                    "APP_BIND",
                    values
                        .optional(&["APP_BIND", "HTTP_BIND_ADDR", "BIND_ADDR"])
                        .unwrap_or(""),
                    "expected host:port socket address",
                )
            })?;

        Ok(Self {
            profile,
            http: HttpConfig { bind_addr },
            database: DatabaseConfig {
                url: values.required(&["DATABASE_URL"])?,
            },
            kvdb: KvdbConfig {
                path: PathBuf::from(values.required(&["KVDB_PATH", "REDB_PATH"])?),
            },
            jwt: JwtConfig {
                issuer: values.required(&["JWT_ISSUER"])?,
                audience: values.required(&["JWT_AUDIENCE"])?,
                secret: values.required(&["JWT_SECRET"])?,
                key_id: values.optional_owned(&["JWT_KEY_ID", "JWT_KID"]),
            },
            chain: ChainConfig {
                chain_id: parse_required_u64(&values, &["CHAIN_ID"])?,
                token_address: parse_required_address(&values, &["TOKEN_ADDRESS"])?,
                token_decimals: parse_required_u8(&values, &["TOKEN_DECIMALS"])?,
                token_symbol: values
                    .optional(&["TOKEN_SYMBOL"])
                    .unwrap_or("TOKEN")
                    .to_string(),
                treasury_address: parse_required_address(&values, &["TREASURY_ADDRESS"])?,
                rpc_http_urls: parse_required_list(&values, &["RPC_HTTP_URLS", "RPC_URLS"])?,
                start_block: parse_required_u64(&values, &["START_BLOCK", "SCAN_FROM_BLOCK"])?,
                min_confirmations: parse_required_u64(&values, &["MIN_CONFIRMATIONS"])?,
                allow_full_history_replay: parse_optional_bool(
                    &values,
                    &["ALLOW_FULL_HISTORY_REPLAY"],
                    false,
                )?,
            },
            collection: {
                let defaults = CollectionConfig::default();
                CollectionConfig {
                    gas_limit: parse_optional_u64(
                        &values,
                        &["COLLECTION_GAS_LIMIT"],
                        defaults.gas_limit,
                    )?,
                    max_fee_per_gas_wei: parse_optional_raw_amount(
                        &values,
                        &["COLLECTION_MAX_FEE_PER_GAS_WEI"],
                        defaults.max_fee_per_gas_wei,
                    )?,
                    max_priority_fee_per_gas_wei: parse_optional_raw_amount(
                        &values,
                        &["COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI"],
                        defaults.max_priority_fee_per_gas_wei,
                    )?,
                }
            },
            collector: {
                let defaults = CollectorConfig::default();
                CollectorConfig {
                    replacement_stuck_after: parse_optional_duration_secs(
                        &values,
                        &["COLLECTION_REPLACEMENT_STUCK_AFTER_SECS"],
                        defaults.replacement_stuck_after,
                    )?,
                }
            },
            signer: SignerConfig {
                mode: SignerMode::parse(values.required_ref(&["SIGNER_MODE", "SIGNER_PROVIDER"])?)?,
                key_ref: values.required(&["SIGNER_KEY_REF"])?,
                remote_endpoint: values.optional_owned(&[
                    "SIGNER_REMOTE_ENDPOINT",
                    "SIGNER_ENDPOINT",
                    "REMOTE_SIGNER_ENDPOINT",
                ]),
                remote_request_timeout: parse_optional_duration_secs(
                    &values,
                    &[
                        "SIGNER_REMOTE_REQUEST_TIMEOUT_SECS",
                        "SIGNER_REQUEST_TIMEOUT_SECS",
                        "REMOTE_SIGNER_REQUEST_TIMEOUT_SECS",
                    ],
                    Duration::from_secs(DEFAULT_SIGNER_REMOTE_REQUEST_TIMEOUT_SECS),
                )?,
            },
        })
    }

    pub fn validate_profile(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();

        if !self.signer.mode.is_fake()
            && self
                .signer
                .remote_endpoint
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty()
        {
            errors.push("non-fake signer modes require SIGNER_REMOTE_ENDPOINT".to_string());
        }

        if !self.profile.is_production() {
            return if errors.is_empty() {
                Ok(())
            } else {
                Err(ConfigError::Validation { errors })
            };
        }

        if self.signer.mode.is_local_or_fake() {
            errors.push("production profile requires an external/KMS/HSM signer".to_string());
        }

        let distinct_rpc_urls = self
            .chain
            .rpc_http_urls
            .iter()
            .map(|url| url.trim())
            .collect::<BTreeSet<_>>()
            .len();
        if distinct_rpc_urls < 2 {
            errors.push(
                "production profile requires at least two distinct RPC providers".to_string(),
            );
        }

        if self.chain.start_block == 0 && !self.chain.allow_full_history_replay {
            errors.push(
                "production profile forbids START_BLOCK=0 unless ALLOW_FULL_HISTORY_REPLAY=true"
                    .to_string(),
            );
        }

        if self
            .jwt
            .key_id
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            errors.push("production profile requires JWT_KEY_ID".to_string());
        }

        if is_weak_jwt_secret(&self.jwt.secret) {
            errors.push("production profile requires a strong JWT_SECRET".to_string());
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Validation { errors })
        }
    }
}

struct EnvPairs {
    values: BTreeMap<String, String>,
}

impl EnvPairs {
    fn new<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let values = pairs
            .into_iter()
            .map(|(key, value)| {
                (
                    key.as_ref().trim().to_string(),
                    value.as_ref().trim().to_string(),
                )
            })
            .collect();
        Self { values }
    }

    fn optional(&self, keys: &[&'static str]) -> Option<&str> {
        keys.iter()
            .find_map(|key| self.values.get(*key).map(String::as_str))
            .filter(|value| !value.trim().is_empty())
    }

    fn optional_owned(&self, keys: &[&'static str]) -> Option<String> {
        self.optional(keys).map(ToOwned::to_owned)
    }

    fn required(&self, keys: &[&'static str]) -> Result<String, ConfigError> {
        self.required_ref(keys).map(ToOwned::to_owned)
    }

    fn required_ref(&self, keys: &[&'static str]) -> Result<&str, ConfigError> {
        self.optional(keys)
            .ok_or(ConfigError::Missing { key: keys[0] })
    }
}

fn parse_required_u64(values: &EnvPairs, keys: &[&'static str]) -> Result<u64, ConfigError> {
    let value = values.required_ref(keys)?;
    value
        .parse::<u64>()
        .map_err(|_| ConfigError::invalid(keys[0], value, "expected unsigned integer"))
}

fn parse_optional_u64(
    values: &EnvPairs,
    keys: &[&'static str],
    default: u64,
) -> Result<u64, ConfigError> {
    let Some(value) = values.optional(keys) else {
        return Ok(default);
    };
    value
        .parse::<u64>()
        .map_err(|_| ConfigError::invalid(keys[0], value, "expected unsigned integer"))
}

fn parse_required_u8(values: &EnvPairs, keys: &[&'static str]) -> Result<u8, ConfigError> {
    let value = values.required_ref(keys)?;
    value
        .parse::<u8>()
        .map_err(|_| ConfigError::invalid(keys[0], value, "expected unsigned 8-bit integer"))
}

fn parse_optional_raw_amount(
    values: &EnvPairs,
    keys: &[&'static str],
    default: RawAmount,
) -> Result<RawAmount, ConfigError> {
    let Some(value) = values.optional(keys) else {
        return Ok(default);
    };
    value
        .parse::<RawAmount>()
        .map_err(|_| ConfigError::invalid(keys[0], value, "expected raw amount integer"))
}

fn parse_optional_duration_secs(
    values: &EnvPairs,
    keys: &[&'static str],
    default: Duration,
) -> Result<Duration, ConfigError> {
    let Some(value) = values.optional(keys) else {
        return Ok(default);
    };
    let secs = value
        .parse::<u64>()
        .map_err(|_| ConfigError::invalid(keys[0], value, "expected unsigned integer seconds"))?;
    Ok(Duration::from_secs(secs))
}

fn parse_required_address(
    values: &EnvPairs,
    keys: &[&'static str],
) -> Result<EvmAddress, ConfigError> {
    let value = values.required_ref(keys)?;
    value
        .parse::<EvmAddress>()
        .map_err(|_| ConfigError::invalid(keys[0], value, "expected 0x-prefixed EVM address"))
}

fn parse_required_list(
    values: &EnvPairs,
    keys: &[&'static str],
) -> Result<Vec<String>, ConfigError> {
    let value = values.required_ref(keys)?;
    let items = value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if items.is_empty() {
        Err(ConfigError::invalid(
            keys[0],
            value,
            "expected comma-separated list",
        ))
    } else {
        Ok(items)
    }
}

fn parse_optional_bool(
    values: &EnvPairs,
    keys: &[&'static str],
    default: bool,
) -> Result<bool, ConfigError> {
    let Some(value) = values.optional(keys) else {
        return Ok(default);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "no" | "n" | "off" => Ok(false),
        _ => Err(ConfigError::invalid(keys[0], value, "expected boolean")),
    }
}

fn is_weak_jwt_secret(secret: &str) -> bool {
    let trimmed = secret.trim();
    if trimmed.len() < 32 {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    let placeholder = matches!(
        lower.as_str(),
        "secret"
            | "jwt_secret"
            | "jwt-secret"
            | "password"
            | "changeme"
            | "change-me"
            | "please-change-me"
            | "development"
            | "production"
            | "test"
            | "local"
    ) || lower.contains("changeme")
        || lower.contains("please-change");

    placeholder
        || trimmed
            .chars()
            .all(|ch| ch == trimmed.chars().next().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_pairs(profile: &str) -> Vec<(&'static str, String)> {
        vec![
            ("APP_PROFILE", profile.to_string()),
            ("APP_BIND", "127.0.0.1:8080".to_string()),
            (
                "DATABASE_URL",
                "postgres://pay3:pay3@localhost:5432/pay3".to_string(),
            ),
            ("KVDB_PATH", "./data/pay3.redb".to_string()),
            ("JWT_ISSUER", "pay3".to_string()),
            ("JWT_AUDIENCE", "pay3-api".to_string()),
            ("JWT_SECRET", "0123456789abcdef0123456789abcdef".to_string()),
            ("JWT_KEY_ID", "pay3-key-1".to_string()),
            ("CHAIN_ID", "31337".to_string()),
            (
                "TOKEN_ADDRESS",
                "0x0000000000000000000000000000000000000001".to_string(),
            ),
            ("TOKEN_DECIMALS", "6".to_string()),
            ("TOKEN_SYMBOL", "USDT".to_string()),
            (
                "TREASURY_ADDRESS",
                "0x0000000000000000000000000000000000000002".to_string(),
            ),
            (
                "RPC_HTTP_URLS",
                "http://localhost:8545,http://localhost:8546".to_string(),
            ),
            ("START_BLOCK", "1".to_string()),
            ("MIN_CONFIRMATIONS", "12".to_string()),
            ("SIGNER_MODE", "external".to_string()),
            ("SIGNER_KEY_REF", "pay3-master".to_string()),
            (
                "SIGNER_REMOTE_ENDPOINT",
                "http://localhost:8081".to_string(),
            ),
            ("SIGNER_REMOTE_REQUEST_TIMEOUT_SECS", "15".to_string()),
        ]
    }

    fn config_with(overrides: &[(&'static str, &str)]) -> AppConfig {
        let mut pairs = valid_pairs("production");
        for &(key, value) in overrides {
            if let Some((_, existing)) = pairs
                .iter_mut()
                .find(|(existing_key, _)| *existing_key == key)
            {
                *existing = value.to_string();
            } else {
                pairs.push((key, value.to_string()));
            }
        }
        AppConfig::from_pairs(pairs).expect("config should parse")
    }

    fn validation_errors(config: &AppConfig) -> Vec<String> {
        match config.validate_profile() {
            Err(ConfigError::Validation { errors }) => errors,
            other => panic!("expected validation errors, got {other:?}"),
        }
    }

    #[test]
    fn loads_typed_config_from_pairs() {
        let config =
            AppConfig::from_pairs(valid_pairs("development")).expect("config should parse");

        assert_eq!(config.profile, AppProfile::Development);
        assert_eq!(config.http.bind_addr.to_string(), "127.0.0.1:8080");
        assert_eq!(
            config.database.url,
            "postgres://pay3:pay3@localhost:5432/pay3"
        );
        assert_eq!(config.kvdb.path, PathBuf::from("./data/pay3.redb"));
        assert_eq!(config.jwt.issuer, "pay3");
        assert_eq!(config.jwt.audience, "pay3-api");
        assert_eq!(config.jwt.key_id.as_deref(), Some("pay3-key-1"));
        assert_eq!(config.chain.chain_id, 31337);
        assert_eq!(config.chain.token_decimals, 6);
        assert_eq!(config.chain.token_symbol, "USDT");
        assert_eq!(config.chain.rpc_http_urls.len(), 2);
        assert_eq!(config.chain.start_block, 1);
        assert_eq!(config.chain.min_confirmations, 12);
        assert_eq!(config.collection, CollectionConfig::default());
        assert_eq!(config.collector, CollectorConfig::default());
        assert_eq!(config.signer.mode, SignerMode::External);
        assert_eq!(config.signer.key_ref, "pay3-master");
        assert_eq!(
            config.signer.remote_endpoint.as_deref(),
            Some("http://localhost:8081")
        );
        assert_eq!(
            config.signer.remote_request_timeout,
            Duration::from_secs(15)
        );
    }

    #[test]
    fn collection_fee_config_can_be_overridden_from_pairs() {
        let config = config_with(&[
            ("COLLECTION_GAS_LIMIT", "90000"),
            ("COLLECTION_MAX_FEE_PER_GAS_WEI", "60000000000"),
            ("COLLECTION_MAX_PRIORITY_FEE_PER_GAS_WEI", "3000000000"),
        ]);

        assert_eq!(config.collection.gas_limit, 90_000);
        assert_eq!(
            config.collection.max_fee_per_gas_wei,
            RawAmount::from(60_000_000_000)
        );
        assert_eq!(
            config.collection.max_priority_fee_per_gas_wei,
            RawAmount::from(3_000_000_000)
        );
    }

    #[test]
    fn collector_replacement_timeout_can_be_overridden_from_pairs() {
        let config = config_with(&[("COLLECTION_REPLACEMENT_STUCK_AFTER_SECS", "120")]);

        assert_eq!(
            config.collector.replacement_stuck_after,
            Duration::from_secs(120)
        );
    }

    #[test]
    fn development_profile_allows_fake_signer_without_remote_endpoint() {
        let mut pairs = valid_pairs("development");
        for (key, value) in &mut pairs {
            if *key == "SIGNER_MODE" {
                *value = "fake".to_string();
            } else if *key == "SIGNER_REMOTE_ENDPOINT" {
                *value = "".to_string();
            }
        }

        let config = AppConfig::from_pairs(pairs).expect("config should parse");
        assert!(config.validate_profile().is_ok());
    }

    #[test]
    fn production_rejects_fake_or_local_signer() {
        for mode in ["fake", "local"] {
            let config = config_with(&[("SIGNER_MODE", mode)]);
            let errors = validation_errors(&config);
            assert!(errors.iter().any(|error| error.contains("signer")));
        }
    }

    #[test]
    fn non_fake_signer_requires_remote_endpoint_in_any_profile() {
        let config = config_with(&[
            ("APP_PROFILE", "development"),
            ("SIGNER_MODE", "external"),
            ("SIGNER_REMOTE_ENDPOINT", ""),
        ]);

        let errors = validation_errors(&config);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("SIGNER_REMOTE_ENDPOINT"))
        );
    }

    #[test]
    fn production_rejects_single_rpc_provider() {
        let config = config_with(&[("RPC_HTTP_URLS", "http://localhost:8545")]);
        let errors = validation_errors(&config);
        assert!(errors.iter().any(|error| error.contains("RPC")));
    }

    #[test]
    fn production_rejects_duplicate_rpc_providers() {
        let config = config_with(&[(
            "RPC_HTTP_URLS",
            "http://localhost:8545,http://localhost:8545",
        )]);
        let errors = validation_errors(&config);
        assert!(errors.iter().any(|error| error.contains("RPC")));
    }

    #[test]
    fn production_rejects_start_block_zero_by_default() {
        let config = config_with(&[("START_BLOCK", "0")]);
        let errors = validation_errors(&config);
        assert!(errors.iter().any(|error| error.contains("START_BLOCK=0")));
    }

    #[test]
    fn production_allows_start_block_zero_when_replay_is_explicit() {
        let config = config_with(&[("START_BLOCK", "0"), ("ALLOW_FULL_HISTORY_REPLAY", "true")]);
        assert!(config.validate_profile().is_ok());
    }

    #[test]
    fn production_rejects_weak_jwt_secret() {
        let config = config_with(&[("JWT_SECRET", "dev-secret")]);
        let errors = validation_errors(&config);
        assert!(errors.iter().any(|error| error.contains("JWT_SECRET")));
    }

    #[test]
    fn production_rejects_missing_jwt_key_id() {
        let config = config_with(&[("JWT_KEY_ID", "")]);
        let errors = validation_errors(&config);
        assert!(errors.iter().any(|error| error.contains("JWT_KEY_ID")));
    }
}
