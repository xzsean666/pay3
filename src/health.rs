use std::{
    fmt,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Serialize, Serializer};

pub type SharedDependencyRegistry = Arc<dyn DependencyRegistry>;

pub trait DependencyRegistry: Send + Sync + 'static {
    fn readiness(&self) -> ReadinessReport;
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthzResponse {
    pub status: &'static str,
}

impl Default for HealthzResponse {
    fn default() -> Self {
        Self { status: "ok" }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReadinessReport {
    pub status: ReadinessState,
    pub dependencies: Vec<DependencyCheck>,
}

impl ReadinessReport {
    pub fn new(dependencies: Vec<DependencyCheck>) -> Self {
        let status = if dependencies
            .iter()
            .all(|dependency| dependency.status.is_healthy())
        {
            ReadinessState::Ok
        } else {
            ReadinessState::Failed
        };

        Self {
            status,
            dependencies,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.status == ReadinessState::Ok
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyCheck {
    pub name: DependencyName,
    pub status: DependencyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl DependencyCheck {
    pub fn healthy(name: DependencyName) -> Self {
        Self {
            name,
            status: DependencyState::Ok,
            message: None,
        }
    }

    pub fn failed(name: DependencyName, message: impl Into<String>) -> Self {
        Self {
            name,
            status: DependencyState::Failed,
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum DependencyName {
    Db,
    Migration,
    RpcChainId,
    Kvdb,
    Signer,
    WorkerLease,
}

impl DependencyName {
    pub const ALL: [DependencyName; 6] = [
        DependencyName::Db,
        DependencyName::Migration,
        DependencyName::RpcChainId,
        DependencyName::Kvdb,
        DependencyName::Signer,
        DependencyName::WorkerLease,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DependencyName::Db => "db",
            DependencyName::Migration => "migration",
            DependencyName::RpcChainId => "rpc_chain_id",
            DependencyName::Kvdb => "kvdb",
            DependencyName::Signer => "signer",
            DependencyName::WorkerLease => "worker_lease",
        }
    }
}

impl fmt::Display for DependencyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for DependencyName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DependencyState {
    Ok,
    Failed,
}

impl DependencyState {
    pub fn as_str(self) -> &'static str {
        match self {
            DependencyState::Ok => "ok",
            DependencyState::Failed => "failed",
        }
    }

    pub fn is_healthy(self) -> bool {
        self == DependencyState::Ok
    }
}

impl Serialize for DependencyState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReadinessState {
    Ok,
    Failed,
}

impl ReadinessState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadinessState::Ok => "ok",
            ReadinessState::Failed => "failed",
        }
    }
}

impl Serialize for ReadinessState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct StaticDependencyRegistry {
    dependencies: Arc<RwLock<Vec<DependencyCheck>>>,
}

impl StaticDependencyRegistry {
    pub fn all_healthy() -> Self {
        Self::from_checks(
            DependencyName::ALL
                .into_iter()
                .map(DependencyCheck::healthy)
                .collect(),
        )
    }

    pub fn from_checks(dependencies: Vec<DependencyCheck>) -> Self {
        Self {
            dependencies: Arc::new(RwLock::new(dependencies)),
        }
    }

    pub fn set_status(&self, check: DependencyCheck) {
        let mut dependencies = self
            .dependencies
            .write()
            .expect("static dependency registry lock poisoned");

        if let Some(existing) = dependencies
            .iter_mut()
            .find(|dependency| dependency.name == check.name)
        {
            *existing = check;
        } else {
            dependencies.push(check);
        }
    }
}

impl Default for StaticDependencyRegistry {
    fn default() -> Self {
        Self::all_healthy()
    }
}

impl DependencyRegistry for StaticDependencyRegistry {
    fn readiness(&self) -> ReadinessReport {
        let dependencies = self
            .dependencies
            .read()
            .expect("static dependency registry lock poisoned")
            .clone();
        ReadinessReport::new(dependencies)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MetricsRecorder {
    inner: Arc<MetricsInner>,
}

#[derive(Debug, Default)]
struct MetricsInner {
    request_count: AtomicU64,
    request_latency_micros_total: AtomicU64,
    request_latency_micros_max: AtomicU64,
}

impl MetricsRecorder {
    pub fn record_request(&self, latency: Duration) {
        let latency_micros = latency.as_micros().min(u128::from(u64::MAX)) as u64;

        self.inner.request_count.fetch_add(1, Ordering::Relaxed);
        self.inner
            .request_latency_micros_total
            .fetch_add(latency_micros, Ordering::Relaxed);
        update_max(&self.inner.request_latency_micros_max, latency_micros);
    }

    pub fn render_prometheus(&self, readiness: &ReadinessReport) -> String {
        let count = self.inner.request_count.load(Ordering::Relaxed);
        let total_seconds = micros_to_seconds(
            self.inner
                .request_latency_micros_total
                .load(Ordering::Relaxed),
        );
        let max_seconds = micros_to_seconds(
            self.inner
                .request_latency_micros_max
                .load(Ordering::Relaxed),
        );

        let mut output = String::new();
        output.push_str("# HELP pay3_build_info Build metadata for the running Pay3 binary.\n");
        output.push_str("# TYPE pay3_build_info gauge\n");
        output.push_str(&format!(
            "pay3_build_info{{crate=\"pay3\",version=\"{}\"}} 1\n",
            escape_label_value(env!("CARGO_PKG_VERSION"))
        ));
        output.push_str(
            "# HELP pay3_http_request_latency_seconds Request latency observed by the API router.\n",
        );
        output.push_str("# TYPE pay3_http_request_latency_seconds summary\n");
        output.push_str(&format!(
            "pay3_http_request_latency_seconds_count {}\n",
            count
        ));
        output.push_str(&format!(
            "pay3_http_request_latency_seconds_sum {:.6}\n",
            total_seconds
        ));
        output.push_str(&format!(
            "pay3_http_request_latency_seconds_max {:.6}\n",
            max_seconds
        ));
        output.push_str(
            "# HELP pay3_readyz_status Overall readiness status (1=ready, 0=not ready).\n",
        );
        output.push_str("# TYPE pay3_readyz_status gauge\n");
        output.push_str(&format!(
            "pay3_readyz_status {}\n",
            if readiness.is_ready() { 1 } else { 0 }
        ));
        output.push_str(
            "# HELP pay3_readyz_dependency_status Readiness dependency status (1=healthy, 0=unhealthy).\n",
        );
        output.push_str("# TYPE pay3_readyz_dependency_status gauge\n");

        for dependency in &readiness.dependencies {
            output.push_str(&format!(
                "pay3_readyz_dependency_status{{dependency=\"{}\"}} {}\n",
                escape_label_value(dependency.name.as_str()),
                if dependency.status.is_healthy() { 1 } else { 0 }
            ));
        }

        output
    }
}

fn update_max(max: &AtomicU64, value: u64) {
    let mut current = max.load(Ordering::Relaxed);
    while value > current {
        match max.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next_current) => current = next_current,
        }
    }
}

fn micros_to_seconds(micros: u64) -> f64 {
    micros as f64 / 1_000_000.0
}

fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::{
        DependencyCheck, DependencyName, DependencyRegistry, ReadinessState,
        StaticDependencyRegistry,
    };

    #[test]
    fn static_registry_reports_failure_if_any_dependency_failed() {
        let registry = StaticDependencyRegistry::all_healthy();
        registry.set_status(DependencyCheck::failed(
            DependencyName::Signer,
            "signer down",
        ));

        let readiness = registry.readiness();

        assert_eq!(readiness.status, ReadinessState::Failed);
        assert!(!readiness.is_ready());
        assert!(readiness.dependencies.iter().any(|dependency| {
            dependency.name == DependencyName::Signer && !dependency.status.is_healthy()
        }));
    }
}
