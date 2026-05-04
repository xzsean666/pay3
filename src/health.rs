use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    TransferLogIngestor,
    PaymentScanner,
    CollectionCollector,
}

impl DependencyName {
    // Startup/static dependencies. Runtime worker dependencies are added by
    // RuntimeDependencyRegistry from live worker telemetry.
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
            DependencyName::TransferLogIngestor => "transfer_log_ingestor",
            DependencyName::PaymentScanner => "payment_scanner",
            DependencyName::CollectionCollector => "collection_collector",
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

#[derive(Clone, Debug)]
pub struct RuntimeDependencyRegistry {
    base: StaticDependencyRegistry,
    metrics: MetricsRecorder,
}

impl RuntimeDependencyRegistry {
    pub fn new(base: StaticDependencyRegistry, metrics: MetricsRecorder) -> Self {
        Self { base, metrics }
    }
}

impl DependencyRegistry for RuntimeDependencyRegistry {
    fn readiness(&self) -> ReadinessReport {
        let mut dependencies = self.base.readiness().dependencies;
        dependencies.extend(self.metrics.worker_dependency_checks());
        ReadinessReport::new(dependencies)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum WorkerName {
    TransferLogIngestor,
    PaymentScanner,
    CollectionCollector,
}

impl WorkerName {
    pub const ALL: [WorkerName; 3] = [
        WorkerName::TransferLogIngestor,
        WorkerName::PaymentScanner,
        WorkerName::CollectionCollector,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WorkerName::TransferLogIngestor => "transfer_log_ingestor",
            WorkerName::PaymentScanner => "payment_scanner",
            WorkerName::CollectionCollector => "collection_collector",
        }
    }

    pub const fn dependency_name(self) -> DependencyName {
        match self {
            WorkerName::TransferLogIngestor => DependencyName::TransferLogIngestor,
            WorkerName::PaymentScanner => DependencyName::PaymentScanner,
            WorkerName::CollectionCollector => DependencyName::CollectionCollector,
        }
    }

    pub fn lag_metric_name(self) -> Option<&'static str> {
        match self {
            WorkerName::TransferLogIngestor => Some("pay3_log_ingestor_lag_blocks"),
            WorkerName::PaymentScanner => Some("pay3_payment_scanner_lag_blocks"),
            WorkerName::CollectionCollector => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerMetricSnapshot {
    pub name: WorkerName,
    pub success_count: u64,
    pub error_count: u64,
    pub consecutive_failures: u64,
    pub last_success_unix_seconds: Option<u64>,
    pub last_error_unix_seconds: Option<u64>,
    pub last_error: Option<String>,
    pub last_tick_duration_micros: Option<u64>,
    pub lag_blocks: Option<u64>,
    pub lag_threshold_blocks: Option<u64>,
    pub last_lag_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct WorkerMetricState {
    success_count: u64,
    error_count: u64,
    consecutive_failures: u64,
    last_success_unix_seconds: Option<u64>,
    last_error_unix_seconds: Option<u64>,
    last_error: Option<String>,
    last_tick_duration_micros: Option<u64>,
    lag_blocks: Option<u64>,
    lag_threshold_blocks: Option<u64>,
    last_lag_unix_seconds: Option<u64>,
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
    worker_metrics: RwLock<BTreeMap<WorkerName, WorkerMetricState>>,
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

    pub fn record_worker_success(&self, worker: WorkerName, latency: Duration) {
        let mut metrics = self
            .inner
            .worker_metrics
            .write()
            .expect("worker metrics lock poisoned");
        let state = metrics.entry(worker).or_default();
        state.success_count = state.success_count.saturating_add(1);
        state.consecutive_failures = 0;
        state.last_success_unix_seconds = Some(unix_now_seconds());
        state.last_tick_duration_micros = Some(duration_to_micros(latency));
    }

    pub fn record_worker_error(
        &self,
        worker: WorkerName,
        latency: Duration,
        message: impl Into<String>,
    ) {
        let mut metrics = self
            .inner
            .worker_metrics
            .write()
            .expect("worker metrics lock poisoned");
        let state = metrics.entry(worker).or_default();
        state.error_count = state.error_count.saturating_add(1);
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.last_error_unix_seconds = Some(unix_now_seconds());
        state.last_error = Some(message.into());
        state.last_tick_duration_micros = Some(duration_to_micros(latency));
    }

    pub fn record_worker_lag(
        &self,
        worker: WorkerName,
        lag_blocks: u64,
        lag_threshold_blocks: u64,
    ) {
        let mut metrics = self
            .inner
            .worker_metrics
            .write()
            .expect("worker metrics lock poisoned");
        let state = metrics.entry(worker).or_default();
        state.lag_blocks = Some(lag_blocks);
        state.lag_threshold_blocks = Some(lag_threshold_blocks);
        state.last_lag_unix_seconds = Some(unix_now_seconds());
    }

    pub fn worker_snapshots(&self) -> Vec<WorkerMetricSnapshot> {
        let metrics = self
            .inner
            .worker_metrics
            .read()
            .expect("worker metrics lock poisoned");

        WorkerName::ALL
            .into_iter()
            .map(|name| {
                let state = metrics.get(&name).cloned().unwrap_or_default();
                WorkerMetricSnapshot {
                    name,
                    success_count: state.success_count,
                    error_count: state.error_count,
                    consecutive_failures: state.consecutive_failures,
                    last_success_unix_seconds: state.last_success_unix_seconds,
                    last_error_unix_seconds: state.last_error_unix_seconds,
                    last_error: state.last_error,
                    last_tick_duration_micros: state.last_tick_duration_micros,
                    lag_blocks: state.lag_blocks,
                    lag_threshold_blocks: state.lag_threshold_blocks,
                    last_lag_unix_seconds: state.last_lag_unix_seconds,
                }
            })
            .collect()
    }

    pub fn worker_dependency_checks(&self) -> Vec<DependencyCheck> {
        self.worker_snapshots()
            .into_iter()
            .map(|snapshot| {
                let dependency = snapshot.name.dependency_name();
                if snapshot.success_count == 0 && snapshot.error_count == 0 {
                    return DependencyCheck::failed(
                        dependency,
                        "worker has not reported a tick yet",
                    );
                }

                if snapshot.consecutive_failures > 0 {
                    return DependencyCheck::failed(
                        dependency,
                        snapshot
                            .last_error
                            .unwrap_or_else(|| "worker tick failed".to_string()),
                    );
                }

                if let Some(threshold) = snapshot.lag_threshold_blocks {
                    let Some(lag_blocks) = snapshot.lag_blocks else {
                        return DependencyCheck::failed(
                            dependency,
                            "worker lag has not been reported yet",
                        );
                    };

                    if lag_blocks > threshold {
                        return DependencyCheck::failed(
                            dependency,
                            format!(
                                "worker lag {} blocks exceeds threshold {}",
                                lag_blocks, threshold
                            ),
                        );
                    }
                }

                DependencyCheck::healthy(dependency)
            })
            .collect()
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

        let workers = self.worker_snapshots();
        output.push_str(
            "# HELP pay3_worker_ticks_total Worker tick outcomes by worker and result.\n",
        );
        output.push_str("# TYPE pay3_worker_ticks_total counter\n");
        for worker in &workers {
            output.push_str(&format!(
                "pay3_worker_ticks_total{{worker=\"{}\",result=\"success\"}} {}\n",
                escape_label_value(worker.name.as_str()),
                worker.success_count
            ));
            output.push_str(&format!(
                "pay3_worker_ticks_total{{worker=\"{}\",result=\"error\"}} {}\n",
                escape_label_value(worker.name.as_str()),
                worker.error_count
            ));
        }

        output.push_str(
            "# HELP pay3_worker_consecutive_failures Consecutive worker tick failures.\n",
        );
        output.push_str("# TYPE pay3_worker_consecutive_failures gauge\n");
        for worker in &workers {
            output.push_str(&format!(
                "pay3_worker_consecutive_failures{{worker=\"{}\"}} {}\n",
                escape_label_value(worker.name.as_str()),
                worker.consecutive_failures
            ));
        }

        output.push_str(
            "# HELP pay3_worker_last_success_unixtime_seconds Unix timestamp of the last successful worker tick.\n",
        );
        output.push_str("# TYPE pay3_worker_last_success_unixtime_seconds gauge\n");
        for worker in &workers {
            output.push_str(&format!(
                "pay3_worker_last_success_unixtime_seconds{{worker=\"{}\"}} {}\n",
                escape_label_value(worker.name.as_str()),
                worker.last_success_unix_seconds.unwrap_or(0)
            ));
        }

        output.push_str(
            "# HELP pay3_worker_last_error_unixtime_seconds Unix timestamp of the last failed worker tick.\n",
        );
        output.push_str("# TYPE pay3_worker_last_error_unixtime_seconds gauge\n");
        for worker in &workers {
            output.push_str(&format!(
                "pay3_worker_last_error_unixtime_seconds{{worker=\"{}\"}} {}\n",
                escape_label_value(worker.name.as_str()),
                worker.last_error_unix_seconds.unwrap_or(0)
            ));
        }

        output.push_str(
            "# HELP pay3_worker_last_tick_duration_seconds Duration of the latest worker tick.\n",
        );
        output.push_str("# TYPE pay3_worker_last_tick_duration_seconds gauge\n");
        for worker in &workers {
            output.push_str(&format!(
                "pay3_worker_last_tick_duration_seconds{{worker=\"{}\"}} {:.6}\n",
                escape_label_value(worker.name.as_str()),
                micros_to_seconds(worker.last_tick_duration_micros.unwrap_or(0))
            ));
        }

        for worker in &workers {
            if let Some(metric_name) = worker.name.lag_metric_name() {
                output.push_str(&format!(
                    "# HELP {} Current worker lag in blocks.\n",
                    metric_name
                ));
                output.push_str(&format!("# TYPE {} gauge\n", metric_name));
                output.push_str(&format!(
                    "{} {}\n",
                    metric_name,
                    worker.lag_blocks.unwrap_or(0)
                ));
            }
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

fn duration_to_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn unix_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn escape_label_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        DependencyCheck, DependencyName, DependencyRegistry, MetricsRecorder, ReadinessReport,
        ReadinessState, RuntimeDependencyRegistry, StaticDependencyRegistry, WorkerName,
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

    #[test]
    fn worker_metrics_feed_prometheus_and_readiness() {
        let metrics = MetricsRecorder::default();
        metrics.record_worker_success(WorkerName::PaymentScanner, Duration::from_millis(25));
        metrics.record_worker_lag(WorkerName::PaymentScanner, 7, 12);
        metrics.record_worker_error(
            WorkerName::CollectionCollector,
            Duration::from_millis(2),
            "rpc timeout",
        );

        let checks = metrics.worker_dependency_checks();
        assert!(checks.iter().any(|dependency| {
            dependency.name == DependencyName::PaymentScanner && dependency.status.is_healthy()
        }));
        assert!(checks.iter().any(|dependency| {
            dependency.name == DependencyName::CollectionCollector
                && !dependency.status.is_healthy()
                && dependency.message.as_deref() == Some("rpc timeout")
        }));
        assert!(checks.iter().any(|dependency| {
            dependency.name == DependencyName::TransferLogIngestor
                && !dependency.status.is_healthy()
        }));

        let readiness = ReadinessReport::new(checks);
        let body = metrics.render_prometheus(&readiness);

        assert!(
            body.contains(
                "pay3_worker_ticks_total{worker=\"payment_scanner\",result=\"success\"} 1"
            )
        );
        assert!(body.contains(
            "pay3_worker_ticks_total{worker=\"collection_collector\",result=\"error\"} 1"
        ));
        assert!(
            body.contains("pay3_worker_consecutive_failures{worker=\"collection_collector\"} 1")
        );
        assert!(body.contains("pay3_log_ingestor_lag_blocks 0"));
        assert!(body.contains("pay3_payment_scanner_lag_blocks 7"));
        assert!(
            body.contains("pay3_readyz_dependency_status{dependency=\"collection_collector\"} 0")
        );
    }

    #[test]
    fn worker_lag_exceeding_threshold_fails_readiness() {
        let metrics = MetricsRecorder::default();
        metrics.record_worker_success(WorkerName::TransferLogIngestor, Duration::from_millis(1));
        metrics.record_worker_lag(WorkerName::TransferLogIngestor, 25, 24);

        let checks = metrics.worker_dependency_checks();

        assert!(checks.iter().any(|dependency| {
            dependency.name == DependencyName::TransferLogIngestor
                && !dependency.status.is_healthy()
                && dependency
                    .message
                    .as_deref()
                    .map(|message| message.contains("exceeds threshold"))
                    .unwrap_or(false)
        }));
    }

    #[test]
    fn runtime_registry_combines_static_and_worker_readiness() {
        let metrics = MetricsRecorder::default();
        for worker in WorkerName::ALL {
            metrics.record_worker_success(worker, Duration::from_millis(1));
        }
        let registry =
            RuntimeDependencyRegistry::new(StaticDependencyRegistry::all_healthy(), metrics);

        let readiness = registry.readiness();

        assert!(readiness.is_ready());
        assert!(readiness.dependencies.iter().any(|dependency| {
            dependency.name == DependencyName::TransferLogIngestor && dependency.status.is_healthy()
        }));
    }
}
