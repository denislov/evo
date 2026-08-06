use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

pub const DEFAULT_WINDOW_SIZE: usize = 20;
pub const DEFAULT_FAILURE_THRESHOLD_PCT: u8 = 50;
pub const DEFAULT_OPEN_DURATION_MS: u64 = 30_000;
pub const DEFAULT_HALF_OPEN_MAX_PROBES: u32 = 1;

/// Tuning knobs for one circuit breaker instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CircuitBreakerConfig {
    /// Number of most recent outcomes kept in the sliding window. The window
    /// must fill before the failure rate is evaluated.
    pub window_size: usize,
    /// Failure percentage (0-100) at or above which the breaker opens.
    pub failure_threshold_pct: u8,
    /// How long the breaker stays open before a half-open probe is allowed.
    pub open_duration_ms: u64,
    /// Maximum number of concurrent probes once half-open.
    pub half_open_max_probes: u32,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            window_size: DEFAULT_WINDOW_SIZE,
            failure_threshold_pct: DEFAULT_FAILURE_THRESHOLD_PCT,
            open_duration_ms: DEFAULT_OPEN_DURATION_MS,
            half_open_max_probes: DEFAULT_HALF_OPEN_MAX_PROBES,
        }
    }
}

impl CircuitBreakerConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.window_size == 0 {
            return Err("circuit breaker window_size must be at least 1".into());
        }
        if self.failure_threshold_pct > 100 {
            return Err("circuit breaker failure_threshold_pct must be at most 100".into());
        }
        if self.open_duration_ms == 0 {
            return Err("circuit breaker open_duration_ms must be at least 1".into());
        }
        if self.half_open_max_probes == 0 {
            return Err("circuit breaker half_open_max_probes must be at least 1".into());
        }
        Ok(())
    }
}

/// Injectable monotonic-ish clock so tests advance time without sleeping.
pub trait Clock: Send + Sync {
    /// Milliseconds since an arbitrary fixed epoch. Must never go backwards.
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default()
    }
}

/// Per-(provider, api) isolation key. Breakers never share state across keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BreakerKey {
    provider: String,
    api: String,
}

impl BreakerKey {
    pub fn new(provider: impl Into<String>, api: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            api: api.into(),
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn api(&self) -> &str {
        &self.api
    }
}

impl std::fmt::Display for BreakerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.api)
    }
}

/// Outcome of one request before it is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerVerdict {
    Allow,
    Reject { retry_after_ms: u64 },
}

/// Circuit breaker state, for diagnostics and transition tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerStateName {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug)]
enum BreakerState {
    Closed(Window),
    Open { opened_at_ms: u64 },
    HalfOpen { probes_in_flight: u32 },
}

#[derive(Debug)]
struct Window {
    results: VecDeque<bool>,
    failures: usize,
}

impl Window {
    fn new() -> Self {
        Self {
            results: VecDeque::new(),
            failures: 0,
        }
    }

    fn push(&mut self, success: bool, window_size: usize) {
        if self.results.len() == window_size {
            self.results.pop_front();
        }
        self.results.push_back(success);
        self.failures = self
            .results
            .iter()
            .filter(|is_success| !**is_success)
            .count();
    }
}

/// One per-key circuit breaker. `before_request` must be called before every
/// attempt; `record_success`/`record_failure` after every outcome.
pub struct CircuitBreaker {
    key: BreakerKey,
    config: CircuitBreakerConfig,
    clock: Arc<dyn Clock>,
    state: Mutex<BreakerState>,
}

impl std::fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("key", &self.key)
            .field("config", &self.config)
            .field("state", &self.state_name())
            .finish()
    }
}

impl CircuitBreaker {
    pub fn new(key: BreakerKey, config: CircuitBreakerConfig) -> Self {
        Self::with_clock(key, config, Arc::new(SystemClock))
    }

    pub fn with_clock(
        key: BreakerKey,
        config: CircuitBreakerConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            key,
            config,
            clock,
            state: Mutex::new(BreakerState::Closed(Window::new())),
        }
    }

    pub fn key(&self) -> &BreakerKey {
        &self.key
    }

    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    pub fn state_name(&self) -> BreakerStateName {
        match *self.state.lock().unwrap() {
            BreakerState::Closed(_) => BreakerStateName::Closed,
            BreakerState::Open { .. } => BreakerStateName::Open,
            BreakerState::HalfOpen { .. } => BreakerStateName::HalfOpen,
        }
    }

    pub fn before_request(&self) -> BreakerVerdict {
        let mut state = self.state.lock().unwrap();
        let now = self.clock.now_ms();
        match &mut *state {
            BreakerState::Closed(_) => BreakerVerdict::Allow,
            BreakerState::Open { opened_at_ms } => {
                let wait_until = opened_at_ms.saturating_add(self.config.open_duration_ms);
                if now < wait_until {
                    return BreakerVerdict::Reject {
                        retry_after_ms: wait_until - now,
                    };
                }
                *state = BreakerState::HalfOpen {
                    probes_in_flight: 1,
                };
                BreakerVerdict::Allow
            }
            BreakerState::HalfOpen {
                probes_in_flight, ..
            } => {
                if *probes_in_flight >= self.config.half_open_max_probes {
                    return BreakerVerdict::Reject { retry_after_ms: 1 };
                }
                *probes_in_flight += 1;
                BreakerVerdict::Allow
            }
        }
    }

    pub fn record_success(&self) {
        let mut state = self.state.lock().unwrap();
        let now = self.clock.now_ms();
        match &mut *state {
            BreakerState::Closed(window) => {
                window.push(true, self.config.window_size);
                if self.window_breached(window) {
                    *state = BreakerState::Open { opened_at_ms: now };
                }
            }
            BreakerState::HalfOpen {
                probes_in_flight, ..
            } => {
                *probes_in_flight = probes_in_flight.saturating_sub(1);
                *state = BreakerState::Closed(Window::new());
            }
            BreakerState::Open { .. } => {}
        }
    }

    pub fn record_failure(&self) {
        let mut state = self.state.lock().unwrap();
        let now = self.clock.now_ms();
        match &mut *state {
            BreakerState::Closed(window) => {
                window.push(false, self.config.window_size);
                if self.window_breached(window) {
                    *state = BreakerState::Open { opened_at_ms: now };
                }
            }
            BreakerState::HalfOpen {
                probes_in_flight, ..
            } => {
                *probes_in_flight = probes_in_flight.saturating_sub(1);
                *state = BreakerState::Open { opened_at_ms: now };
            }
            BreakerState::Open { .. } => {}
        }
    }

    fn window_breached(&self, window: &Window) -> bool {
        if window.results.len() != self.config.window_size {
            return false;
        }
        let failures_pct = window.failures * 100 / self.config.window_size;
        failures_pct >= self.config.failure_threshold_pct as usize
    }
}

/// Shared, keyed breaker registry. One breaker per
/// `(provider, api_name)` pair, lazily created with the shared config.
pub struct CircuitBreakerRegistry {
    config: CircuitBreakerConfig,
    clock: Arc<dyn Clock>,
    breakers: Mutex<HashMap<BreakerKey, Arc<CircuitBreaker>>>,
}

impl std::fmt::Debug for CircuitBreakerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircuitBreakerRegistry")
            .field("config", &self.config)
            .field(
                "breakers",
                &self.breakers.lock().unwrap().keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Default for CircuitBreakerRegistry {
    fn default() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }
}

impl CircuitBreakerRegistry {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self::with_clock(config, Arc::new(SystemClock))
    }

    pub fn with_clock(config: CircuitBreakerConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            config,
            clock,
            breakers: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    pub fn breaker_for(&self, key: BreakerKey) -> Arc<CircuitBreaker> {
        self.breakers
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_insert_with(|| {
                Arc::new(CircuitBreaker::with_clock(
                    key,
                    self.config,
                    self.clock.clone(),
                ))
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Default)]
    struct MockClock {
        now: AtomicU64,
    }

    impl MockClock {
        fn advance(&self, ms: u64) {
            self.now.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl Clock for MockClock {
        fn now_ms(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }
    }

    fn breaker(config: CircuitBreakerConfig, clock: &Arc<MockClock>) -> Arc<CircuitBreaker> {
        Arc::new(CircuitBreaker::with_clock(
            BreakerKey::new("test-provider", "test-api"),
            config,
            clock.clone(),
        ))
    }

    fn closed(config: CircuitBreakerConfig) -> (Arc<CircuitBreaker>, Arc<MockClock>) {
        let clock = Arc::new(MockClock::default());
        (breaker(config, &clock), clock)
    }

    #[test]
    fn default_config_is_valid_and_sane() {
        let config = CircuitBreakerConfig::default();
        config.validate().expect("defaults are valid");
        assert!(config.window_size > 0);
        assert!(config.failure_threshold_pct <= 100);
        assert!(config.open_duration_ms > 0);
        assert!(config.half_open_max_probes >= 1);
    }

    #[test]
    fn validate_rejects_degenerate_configs() {
        let base = CircuitBreakerConfig::default();
        for broken in [
            CircuitBreakerConfig {
                window_size: 0,
                ..base
            },
            CircuitBreakerConfig {
                failure_threshold_pct: 101,
                ..base
            },
            CircuitBreakerConfig {
                open_duration_ms: 0,
                ..base
            },
            CircuitBreakerConfig {
                half_open_max_probes: 0,
                ..base
            },
        ] {
            assert!(broken.validate().is_err());
        }
        assert!(base.validate().is_ok());
    }

    #[test]
    fn window_must_fill_before_opening() {
        let config = CircuitBreakerConfig {
            window_size: 10,
            failure_threshold_pct: 50,
            ..CircuitBreakerConfig::default()
        };
        let (cb, _clock) = closed(config);
        for _ in 0..9 {
            cb.record_failure();
        }
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Open);
    }

    #[test]
    fn threshold_uses_percentage_of_filled_window() {
        let config = CircuitBreakerConfig {
            window_size: 10,
            failure_threshold_pct: 60,
            ..CircuitBreakerConfig::default()
        };
        let (cb, _clock) = closed(config);
        for _ in 0..7 {
            cb.record_success();
        }
        for _ in 0..3 {
            cb.record_failure();
        }
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Open);
    }

    #[test]
    fn sliding_window_drops_oldest_outcomes() {
        let config = CircuitBreakerConfig {
            window_size: 5,
            failure_threshold_pct: 60,
            ..CircuitBreakerConfig::default()
        };
        let (cb, _clock) = closed(config);
        for _ in 0..3 {
            cb.record_success();
        }
        for _ in 0..2 {
            cb.record_failure();
        }
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Open);
    }

    #[test]
    fn open_rejects_until_duration_elapses() {
        let config = CircuitBreakerConfig {
            window_size: 2,
            failure_threshold_pct: 50,
            open_duration_ms: 10_000,
            ..CircuitBreakerConfig::default()
        };
        let (cb, clock) = closed(config);
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Open);
        assert_eq!(
            cb.before_request(),
            BreakerVerdict::Reject {
                retry_after_ms: 10_000
            }
        );
        clock.advance(4_999);
        assert_eq!(
            cb.before_request(),
            BreakerVerdict::Reject {
                retry_after_ms: 5_001
            }
        );
        clock.advance(5_001);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
        assert_eq!(cb.state_name(), BreakerStateName::HalfOpen);
    }

    #[test]
    fn half_open_probe_success_recloses_and_resets_window() {
        let config = CircuitBreakerConfig {
            window_size: 2,
            failure_threshold_pct: 50,
            open_duration_ms: 1_000,
            ..CircuitBreakerConfig::default()
        };
        let (cb, clock) = closed(config);
        cb.record_failure();
        cb.record_failure();
        clock.advance(1_000);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
        cb.record_success();
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
    }

    #[test]
    fn half_open_probe_failure_reopens() {
        let config = CircuitBreakerConfig {
            window_size: 2,
            failure_threshold_pct: 50,
            open_duration_ms: 1_000,
            ..CircuitBreakerConfig::default()
        };
        let (cb, clock) = closed(config);
        cb.record_failure();
        cb.record_failure();
        clock.advance(1_000);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
        cb.record_failure();
        assert_eq!(cb.state_name(), BreakerStateName::Open);
        assert!(matches!(cb.before_request(), BreakerVerdict::Reject { .. }));
        clock.advance(999);
        assert_eq!(
            cb.before_request(),
            BreakerVerdict::Reject { retry_after_ms: 1 }
        );
        clock.advance(1);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
    }

    #[test]
    fn half_open_limits_concurrent_probes() {
        let config = CircuitBreakerConfig {
            window_size: 2,
            failure_threshold_pct: 50,
            open_duration_ms: 1_000,
            half_open_max_probes: 2,
        };
        let (cb, clock) = closed(config);
        cb.record_failure();
        cb.record_failure();
        clock.advance(1_000);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
        assert_eq!(
            cb.before_request(),
            BreakerVerdict::Reject { retry_after_ms: 1 }
        );
        cb.record_success();
        cb.record_success();
        assert_eq!(cb.state_name(), BreakerStateName::Closed);
    }

    #[test]
    fn registry_isolates_breakers_by_key() {
        let registry = Arc::new(CircuitBreakerRegistry::new(CircuitBreakerConfig {
            window_size: 2,
            failure_threshold_pct: 50,
            ..CircuitBreakerConfig::default()
        }));
        let a = registry.breaker_for(BreakerKey::new("deepseek", "deepseek-responses"));
        let b = registry.breaker_for(BreakerKey::new("openai", "openai-responses"));
        a.record_failure();
        a.record_failure();
        assert_eq!(a.state_name(), BreakerStateName::Open);
        assert_eq!(b.state_name(), BreakerStateName::Closed);
        assert_eq!(b.before_request(), BreakerVerdict::Allow);
        let a_again = registry.breaker_for(BreakerKey::new("deepseek", "deepseek-responses"));
        assert_eq!(a_again.state_name(), BreakerStateName::Open);
    }

    #[test]
    fn registry_clock_is_shared_and_advanceable() {
        let clock = Arc::new(MockClock::default());
        let registry = CircuitBreakerRegistry::with_clock(
            CircuitBreakerConfig {
                window_size: 2,
                failure_threshold_pct: 50,
                open_duration_ms: 5_000,
                ..CircuitBreakerConfig::default()
            },
            clock.clone(),
        );
        let cb = registry.breaker_for(BreakerKey::new("p", "a"));
        cb.record_failure();
        cb.record_failure();
        assert!(matches!(cb.before_request(), BreakerVerdict::Reject { .. }));
        clock.advance(5_000);
        assert_eq!(cb.before_request(), BreakerVerdict::Allow);
    }
}
