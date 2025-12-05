//! Misbehavior tracking with sliding window disputes and sit-out penalties.
//!
//! Tracks consensus disputes per upstream within a sliding time window.
//! When an upstream exceeds the dispute threshold, it enters a "sit-out"
//! period where it is temporarily excluded from selection.
//!
//! # Example Configuration
//!
//! ```toml
//! [consensus]
//! dispute_threshold = 3
//! dispute_window_seconds = 300
//! sit_out_penalty_seconds = 60
//! misbehavior_log_path = "/var/log/prism/misbehaviors.jsonl"
//! ```

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{sync::RwLock, time::Instant};
use tracing::{debug, info, warn};

/// Configuration for misbehavior tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MisbehaviorConfig {
    /// Number of disputes within window before triggering sit-out.
    /// If None, misbehavior tracking is disabled.
    pub dispute_threshold: Option<u32>,

    /// Time window in seconds for counting disputes (default: 300).
    #[serde(default = "default_dispute_window")]
    pub dispute_window_seconds: u64,

    /// Duration in seconds for sit-out penalty (default: 60).
    #[serde(default = "default_sit_out_penalty")]
    pub sit_out_penalty_seconds: u64,

    /// Optional path to log misbehavior events (JSONL format).
    #[serde(default)]
    pub log_path: Option<PathBuf>,

    /// Maximum log file size in bytes before rotation (default: 100MB).
    #[serde(default = "default_max_log_size")]
    pub max_log_file_size_bytes: u64,
}

fn default_dispute_window() -> u64 {
    300
}

fn default_sit_out_penalty() -> u64 {
    60
}

fn default_max_log_size() -> u64 {
    100 * 1024 * 1024 // 100MB
}

impl Default for MisbehaviorConfig {
    fn default() -> Self {
        Self {
            dispute_threshold: None,
            dispute_window_seconds: default_dispute_window(),
            sit_out_penalty_seconds: default_sit_out_penalty(),
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        }
    }
}

impl MisbehaviorConfig {
    /// Returns whether misbehavior tracking is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.dispute_threshold.is_some()
    }

    /// Returns the dispute window as a Duration.
    #[must_use]
    pub fn dispute_window(&self) -> Duration {
        Duration::from_secs(self.dispute_window_seconds)
    }

    /// Returns the sit-out penalty as a Duration.
    #[must_use]
    pub fn sit_out_duration(&self) -> Duration {
        Duration::from_secs(self.sit_out_penalty_seconds)
    }
}

/// Internal tracking state for a single upstream.
#[derive(Debug)]
struct UpstreamMisbehavior {
    /// Timestamps of recent disputes within the sliding window.
    disputes: VecDeque<Instant>,
    /// When the current sit-out period ends (if any).
    sit_out_until: Option<Instant>,
}

impl UpstreamMisbehavior {
    fn new() -> Self {
        Self { disputes: VecDeque::new(), sit_out_until: None }
    }

    fn prune_expired(&mut self, window: Duration) {
        let now = Instant::now();
        while let Some(&oldest) = self.disputes.front() {
            if now.duration_since(oldest) > window {
                self.disputes.pop_front();
            } else {
                break;
            }
        }
    }

    /// Returns current dispute count within the window.
    fn dispute_count(&self) -> usize {
        self.disputes.len()
    }

    /// Checks if currently in sit-out.
    fn is_in_sit_out(&self) -> bool {
        self.sit_out_until.is_some_and(|until| Instant::now() < until)
    }
}

/// Tracks misbehavior (consensus disputes) per upstream with sit-out penalties.
///
/// Thread-safe: uses `RwLock` for concurrent access.
pub struct MisbehaviorTracker {
    config: RwLock<MisbehaviorConfig>,
    upstreams: RwLock<HashMap<Arc<str>, UpstreamMisbehavior>>,
}

impl MisbehaviorTracker {
    /// Creates a new tracker with the given configuration.
    #[must_use]
    pub fn new(config: MisbehaviorConfig) -> Self {
        Self { config: RwLock::new(config), upstreams: RwLock::new(HashMap::new()) }
    }

    /// Records a dispute for an upstream.
    ///
    /// Returns `true` if sit-out was triggered by this dispute.
    pub async fn record_dispute(&self, upstream: &str, method: Option<&str>) -> bool {
        let config = self.config.read().await;

        let Some(threshold) = config.dispute_threshold else {
            return false;
        };

        let window = config.dispute_window();
        let sit_out_duration = config.sit_out_duration();
        let log_path = config.log_path.clone();
        drop(config);

        let mut upstreams = self.upstreams.write().await;
        let upstream_key: Arc<str> = upstream.into();

        let misbehavior = upstreams
            .entry(Arc::clone(&upstream_key))
            .or_insert_with(UpstreamMisbehavior::new);

        misbehavior.prune_expired(window);
        misbehavior.disputes.push_back(Instant::now());
        let dispute_count = misbehavior.dispute_count();

        debug!(
            upstream = %upstream,
            dispute_count = dispute_count,
            threshold = threshold,
            "recorded dispute"
        );

        let sit_out_triggered =
            if dispute_count >= threshold as usize && !misbehavior.is_in_sit_out() {
                let sit_out_until = Instant::now() + sit_out_duration;
                misbehavior.sit_out_until = Some(sit_out_until);

                info!(
                    upstream = %upstream,
                    dispute_count = dispute_count,
                    sit_out_seconds = sit_out_duration.as_secs(),
                    "upstream entered sit-out due to excessive disputes"
                );

                true
            } else {
                false
            };

        if let Some(ref path) = log_path {
            let event = MisbehaviorEvent {
                timestamp: chrono::Utc::now(),
                upstream: upstream.to_string(),
                event_type: if sit_out_triggered {
                    MisbehaviorEventType::SitOutStarted
                } else {
                    MisbehaviorEventType::Dispute
                },
                dispute_count,
                sit_out_triggered,
                method: method.map(String::from),
            };
            Self::log_event(path, &event).await;
        }

        sit_out_triggered
    }

    /// Checks if an upstream is currently in sit-out.
    pub async fn is_in_sit_out(&self, upstream: &str) -> bool {
        let upstreams = self.upstreams.read().await;
        upstreams.get(upstream).is_some_and(UpstreamMisbehavior::is_in_sit_out)
    }

    /// Returns the current dispute count for an upstream within the window.
    pub async fn get_dispute_count(&self, upstream: &str) -> usize {
        let config = self.config.read().await;
        let window = config.dispute_window();
        drop(config);

        let mut upstreams = self.upstreams.write().await;
        if let Some(misbehavior) = upstreams.get_mut(upstream) {
            misbehavior.prune_expired(window);
            misbehavior.dispute_count()
        } else {
            0
        }
    }

    /// Filters out upstreams currently in sit-out.
    ///
    /// For inline filtering of `Arc<UpstreamEndpoint>`, prefer using
    /// `is_in_sit_out()` directly to avoid intermediate allocations.
    pub async fn filter_available<T: AsRef<str> + Clone>(&self, upstreams: Vec<T>) -> Vec<T> {
        let state = self.upstreams.read().await;

        upstreams
            .into_iter()
            .filter(|u| !state.get(u.as_ref()).is_some_and(UpstreamMisbehavior::is_in_sit_out))
            .collect()
    }

    pub async fn update_config(&self, config: MisbehaviorConfig) {
        let mut current = self.config.write().await;
        *current = config;
    }

    pub async fn get_config(&self) -> MisbehaviorConfig {
        self.config.read().await.clone()
    }

    pub async fn clear(&self) {
        let mut upstreams = self.upstreams.write().await;
        upstreams.clear();
    }

    pub async fn get_stats(&self, upstream: &str) -> Option<MisbehaviorStats> {
        let config = self.config.read().await;
        let window = config.dispute_window();
        let threshold = config.dispute_threshold;
        drop(config);

        let mut upstreams = self.upstreams.write().await;
        let misbehavior = upstreams.get_mut(upstream)?;

        misbehavior.prune_expired(window);

        Some(MisbehaviorStats {
            dispute_count: misbehavior.dispute_count(),
            threshold,
            is_in_sit_out: misbehavior.is_in_sit_out(),
            sit_out_remaining_secs: misbehavior
                .sit_out_until
                .and_then(|until| until.checked_duration_since(Instant::now()))
                .map(|d| d.as_secs()),
        })
    }

    /// Logs a misbehavior event to the configured file path with automatic rotation.
    ///
    /// Rotates the log file when it exceeds `max_log_file_size_bytes` by renaming
    /// the current file with a `.1` suffix (and rotating existing backups).
    async fn log_event(path: &PathBuf, event: &MisbehaviorEvent) {
        use tokio::{
            fs::{metadata, rename, OpenOptions},
            io::AsyncWriteExt,
        };

        const MAX_LOG_SIZE: u64 = 100 * 1024 * 1024;

        if path.to_str().is_some_and(|s| s.ends_with('/')) {
            warn!(path = %path.display(), "invalid log path: appears to be directory");
            return;
        }

        let json = match serde_json::to_string(event) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "failed to serialize misbehavior event");
                return;
            }
        };

        let mut line = json;
        line.push('\n');

        if let Ok(meta) = metadata(path).await {
            if meta.len() >= MAX_LOG_SIZE {
                for i in (1..=3).rev() {
                    let old_backup = path.with_extension(format!("{i}"));
                    let new_backup = path.with_extension(format!("{}", i + 1));
                    if old_backup.exists() {
                        let _ = rename(&old_backup, &new_backup).await;
                    }
                }

                let backup_path = path.with_extension("1");
                if let Err(e) = rename(path, &backup_path).await {
                    warn!(
                        error = %e,
                        path = %path.display(),
                        "failed to rotate misbehavior log file"
                    );
                } else {
                    debug!(
                        path = %path.display(),
                        backup = %backup_path.display(),
                        "rotated misbehavior log file"
                    );
                }
            }
        }

        match OpenOptions::new().create(true).append(true).open(path).await {
            Ok(mut file) => {
                if let Err(e) = file.write_all(line.as_bytes()).await {
                    warn!(error = %e, path = %path.display(), "failed to write misbehavior event");
                }
            }
            Err(e) => {
                warn!(error = %e, path = %path.display(), "failed to open misbehavior log file");
            }
        }
    }
}

/// Statistics about an upstream's misbehavior state.
#[derive(Debug, Clone)]
pub struct MisbehaviorStats {
    /// Current dispute count within the window.
    pub dispute_count: usize,
    /// Configured threshold (if tracking is enabled).
    pub threshold: Option<u32>,
    /// Whether the upstream is currently in sit-out.
    pub is_in_sit_out: bool,
    /// Seconds remaining in sit-out (if applicable).
    pub sit_out_remaining_secs: Option<u64>,
}

/// Types of misbehavior events for logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MisbehaviorEventType {
    /// A single dispute was recorded.
    Dispute,
    /// Sit-out period started due to threshold exceeded.
    SitOutStarted,
    /// Sit-out period ended (upstream available again).
    SitOutEnded,
}

/// A misbehavior event for JSONL logging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MisbehaviorEvent {
    /// When the event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// The upstream name.
    pub upstream: String,
    /// Type of event.
    pub event_type: MisbehaviorEventType,
    /// Current dispute count at time of event.
    pub dispute_count: usize,
    /// Whether sit-out was triggered.
    pub sit_out_triggered: bool,
    /// Method that triggered the dispute (if known).
    pub method: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_misbehavior_disabled_by_default() {
        let config = MisbehaviorConfig::default();
        assert!(!config.is_enabled());

        let tracker = MisbehaviorTracker::new(config);
        let triggered = tracker.record_dispute("upstream-1", None).await;
        assert!(!triggered);
    }

    #[tokio::test]
    async fn test_misbehavior_counts_disputes() {
        let config = MisbehaviorConfig {
            dispute_threshold: Some(5),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 60,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };

        let tracker = MisbehaviorTracker::new(config);

        tracker.record_dispute("upstream-1", None).await;
        tracker.record_dispute("upstream-1", None).await;
        tracker.record_dispute("upstream-1", None).await;

        let count = tracker.get_dispute_count("upstream-1").await;
        assert_eq!(count, 3);

        let count2 = tracker.get_dispute_count("upstream-2").await;
        assert_eq!(count2, 0);
    }

    #[tokio::test]
    async fn test_misbehavior_triggers_sit_out() {
        let config = MisbehaviorConfig {
            dispute_threshold: Some(3),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 60,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };

        let tracker = MisbehaviorTracker::new(config);

        assert!(!tracker.record_dispute("upstream-1", None).await);
        assert!(!tracker.record_dispute("upstream-1", None).await);
        assert!(!tracker.is_in_sit_out("upstream-1").await);

        assert!(tracker.record_dispute("upstream-1", None).await);
        assert!(tracker.is_in_sit_out("upstream-1").await);
    }

    #[tokio::test]
    async fn test_misbehavior_filter_available() {
        let config = MisbehaviorConfig {
            dispute_threshold: Some(2),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 60,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };

        let tracker = MisbehaviorTracker::new(config);

        tracker.record_dispute("upstream-1", None).await;
        tracker.record_dispute("upstream-1", None).await;

        let upstreams = vec!["upstream-1", "upstream-2", "upstream-3"];
        let available = tracker.filter_available(upstreams).await;

        assert_eq!(available.len(), 2);
        assert!(!available.contains(&"upstream-1"));
        assert!(available.contains(&"upstream-2"));
        assert!(available.contains(&"upstream-3"));
    }

    #[tokio::test]
    async fn test_misbehavior_sit_out_expires() {
        let config = MisbehaviorConfig {
            dispute_threshold: Some(2),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 0,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };

        let tracker = MisbehaviorTracker::new(config);

        tracker.record_dispute("upstream-1", None).await;
        tracker.record_dispute("upstream-1", None).await;

        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!tracker.is_in_sit_out("upstream-1").await);
    }

    #[tokio::test]
    async fn test_misbehavior_stats() {
        let config = MisbehaviorConfig {
            dispute_threshold: Some(5),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 60,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };

        let tracker = MisbehaviorTracker::new(config);

        tracker.record_dispute("upstream-1", None).await;
        tracker.record_dispute("upstream-1", None).await;

        let stats = tracker.get_stats("upstream-1").await.unwrap();
        assert_eq!(stats.dispute_count, 2);
        assert_eq!(stats.threshold, Some(5));
        assert!(!stats.is_in_sit_out);
    }

    #[tokio::test]
    async fn test_misbehavior_clear() {
        let config = MisbehaviorConfig {
            dispute_threshold: Some(2),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 60,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };

        let tracker = MisbehaviorTracker::new(config);

        tracker.record_dispute("upstream-1", None).await;
        tracker.record_dispute("upstream-1", None).await;
        assert!(tracker.is_in_sit_out("upstream-1").await);

        tracker.clear().await;
        assert!(!tracker.is_in_sit_out("upstream-1").await);
        assert_eq!(tracker.get_dispute_count("upstream-1").await, 0);
    }

    #[tokio::test]
    async fn test_misbehavior_config_update() {
        let config = MisbehaviorConfig {
            dispute_threshold: Some(10),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 60,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };

        let tracker = MisbehaviorTracker::new(config);

        let new_config = MisbehaviorConfig {
            dispute_threshold: Some(2),
            dispute_window_seconds: 300,
            sit_out_penalty_seconds: 60,
            log_path: None,
            max_log_file_size_bytes: default_max_log_size(),
        };
        tracker.update_config(new_config).await;

        tracker.record_dispute("upstream-1", None).await;
        tracker.record_dispute("upstream-1", None).await;
        assert!(tracker.is_in_sit_out("upstream-1").await);
    }
}
