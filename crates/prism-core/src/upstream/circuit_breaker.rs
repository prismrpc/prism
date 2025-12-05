use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

/// Internal mutable state protected by a single `RwLock`.
///
/// Consolidates `failure_count`, `last_failure_time`, and `state` to prevent
/// race conditions during state transitions. All fields are updated atomically
/// within a single lock acquisition.
#[derive(Debug)]
struct CircuitBreakerInternalState {
    /// Number of consecutive failures.
    failure_count: u32,
    /// Timestamp of the last recorded failure.
    last_failure_time: Option<Instant>,
    /// Current state of the circuit breaker FSM.
    state: CircuitBreakerState,
}

impl CircuitBreakerInternalState {
    fn new() -> Self {
        Self { failure_count: 0, last_failure_time: None, state: CircuitBreakerState::Closed }
    }
}

/// Circuit breaker pattern implementation for protecting upstream endpoints.
///
/// Prevents cascading failures by temporarily blocking requests to failing upstreams.
/// Uses a three-state model (`Closed`, `Open`, `HalfOpen`) to manage failure recovery.
///
/// # Thread Safety
///
/// All mutable state is protected by a single `RwLock` to ensure atomic state transitions.
/// This eliminates race conditions that could occur with separate locks for each field.
pub struct CircuitBreaker {
    /// All mutable state under a single lock to prevent race conditions.
    inner: Arc<RwLock<CircuitBreakerInternalState>>,
    /// Immutable: Number of consecutive failures before opening the circuit.
    threshold: u32,
    /// Immutable: Time to wait in Open state before transitioning to `HalfOpen`.
    timeout: Duration,
}

/// Circuit breaker state machine.
///
/// Transitions between states based on failure count and recovery timeout:
/// - `Closed` -> `Open`: When failure count reaches threshold
/// - `Open` -> `HalfOpen`: When timeout expires
/// - `HalfOpen` -> `Closed`: On successful request
/// - `HalfOpen` -> `Open`: On failed request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitBreakerState {
    /// Normal operation, requests are allowed through.
    Closed,
    /// Failures exceeded threshold, requests are blocked.
    Open,
    /// Recovery mode, testing if the upstream has recovered.
    HalfOpen,
}

impl CircuitBreaker {
    /// Creates a new circuit breaker with the specified failure threshold and timeout.
    ///
    /// # Parameters
    /// - `threshold`: Number of consecutive failures before opening the circuit
    /// - `timeout_seconds`: Time to wait in `Open` state before transitioning to `HalfOpen`
    #[must_use]
    pub fn new(threshold: u32, timeout_seconds: u64) -> Self {
        Self {
            inner: Arc::new(RwLock::new(CircuitBreakerInternalState::new())),
            threshold,
            timeout: Duration::from_secs(timeout_seconds),
        }
    }

    /// Determines whether a request should be allowed through.
    ///
    /// Returns `true` if the circuit is `Closed` or `HalfOpen`, or if the circuit is `Open`
    /// but the timeout has expired (automatically transitioning to `HalfOpen`).
    /// Returns `false` if the circuit is `Open` and the timeout has not expired.
    ///
    /// Uses double-checked locking pattern: first tries with read lock, then acquires
    /// write lock only if state transition is needed.
    pub async fn can_execute(&self) -> bool {
        // First try with read lock for the common case (Closed or HalfOpen)
        {
            let inner = self.inner.read().await;
            match inner.state {
                CircuitBreakerState::Closed | CircuitBreakerState::HalfOpen => return true,
                CircuitBreakerState::Open => {
                    // Check if timeout has expired
                    if let Some(failure_time) = inner.last_failure_time {
                        if failure_time.elapsed() < self.timeout {
                            return false;
                        }
                    } else {
                        return false;
                    }
                    // Timeout expired, need to transition to HalfOpen
                    // Fall through to acquire write lock
                }
            }
        }

        // Timeout expired, acquire write lock and re-check (double-checked locking)
        let mut inner = self.inner.write().await;

        // Re-check state after acquiring write lock (another thread may have transitioned)
        match inner.state {
            CircuitBreakerState::Closed | CircuitBreakerState::HalfOpen => true,
            CircuitBreakerState::Open => {
                if let Some(failure_time) = inner.last_failure_time {
                    if failure_time.elapsed() >= self.timeout {
                        inner.state = CircuitBreakerState::HalfOpen;
                        tracing::warn!("circuit breaker transitioning to half-open state");
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Records a successful request execution.
    ///
    /// Resets the failure count and transitions from `HalfOpen` or `Open` back to `Closed` state,
    /// indicating the upstream has recovered.
    pub async fn on_success(&self) {
        let mut inner = self.inner.write().await;
        match inner.state {
            CircuitBreakerState::Closed => {
                inner.failure_count = 0;
            }
            CircuitBreakerState::HalfOpen | CircuitBreakerState::Open => {
                inner.state = CircuitBreakerState::Closed;
                inner.failure_count = 0;
                inner.last_failure_time = None;
                tracing::info!("circuit breaker closed after successful request");
            }
        }
    }

    /// Records a failed request execution.
    ///
    /// Increments the failure count and records the failure timestamp. If the failure count
    /// reaches the threshold, transitions to `Open` state and blocks subsequent requests.
    pub async fn on_failure(&self) {
        let mut inner = self.inner.write().await;
        inner.failure_count += 1;
        inner.last_failure_time = Some(Instant::now());

        if inner.failure_count >= self.threshold {
            inner.state = CircuitBreakerState::Open;
            tracing::warn!(
                threshold = self.threshold,
                "circuit breaker opened after reaching failure threshold"
            );
        }
    }

    /// Returns the current circuit breaker state.
    pub async fn get_state(&self) -> CircuitBreakerState {
        self.inner.read().await.state
    }

    /// Returns the current failure count.
    pub async fn get_failure_count(&self) -> u32 {
        self.inner.read().await.failure_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_circuit_breaker() {
        let breaker = CircuitBreaker::new(3, 60);

        assert!(breaker.can_execute().await);
        assert_eq!(breaker.get_state().await, CircuitBreakerState::Closed);

        for _ in 0..3 {
            breaker.on_failure().await;
        }

        assert_eq!(breaker.get_state().await, CircuitBreakerState::Open);
        assert!(!breaker.can_execute().await);

        breaker.on_success().await;
        assert_eq!(breaker.get_state().await, CircuitBreakerState::Closed);
        assert!(breaker.can_execute().await);
    }

    #[tokio::test]
    async fn test_circuit_breaker_half_open() {
        let breaker = CircuitBreaker::new(2, 1);

        breaker.on_failure().await;
        breaker.on_failure().await;
        assert_eq!(breaker.get_state().await, CircuitBreakerState::Open);

        tokio::time::sleep(Duration::from_secs(2)).await;

        assert!(breaker.can_execute().await);
        assert_eq!(breaker.get_state().await, CircuitBreakerState::HalfOpen);

        breaker.on_success().await;
        assert_eq!(breaker.get_state().await, CircuitBreakerState::Closed);
    }

    #[tokio::test]
    async fn test_circuit_breaker_threshold() {
        let breaker = CircuitBreaker::new(5, 60);

        for i in 0..4 {
            breaker.on_failure().await;
            assert_eq!(breaker.get_state().await, CircuitBreakerState::Closed);
            assert_eq!(breaker.get_failure_count().await, i + 1);
        }

        breaker.on_failure().await;
        assert_eq!(breaker.get_state().await, CircuitBreakerState::Open);
        assert_eq!(breaker.get_failure_count().await, 5);
    }
}
