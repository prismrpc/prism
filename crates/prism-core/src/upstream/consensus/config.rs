//! Consensus configuration types and defaults.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for the consensus system.
///
/// This struct has multiple boolean flags which trigger `struct_excessive_bools`.
/// Each flag represents an independent, user-configurable feature toggle that
/// cannot be meaningfully combined into an enum without losing configurability:
/// - `enabled`: Master switch for consensus
/// - `prefer_non_empty`: Response selection preference
/// - `prefer_non_empty_always`: Stricter non-empty preference
/// - `prefer_larger_responses`: Size-based tiebreaker
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct ConsensusConfig {
    /// Whether consensus is enabled
    #[serde(default)]
    pub enabled: bool,

    /// Maximum upstreams to query (default: 3)
    #[serde(default = "default_max_count")]
    pub max_count: usize,

    /// Minimum upstreams required for consensus (default: 2)
    #[serde(default = "default_min_count")]
    pub min_count: usize,

    /// Dispute resolution strategy
    #[serde(default)]
    pub dispute_behavior: DisputeBehavior,

    /// Failure handling strategy
    #[serde(default)]
    pub failure_behavior: FailureBehavior,

    /// Low participant count handling strategy
    #[serde(default)]
    pub low_participants_behavior: LowParticipantsBehavior,

    /// Methods requiring consensus (default: critical read methods)
    #[serde(default = "default_consensus_methods")]
    pub methods: Vec<String>,

    /// Score penalty for disagreeing upstreams (default: 10.0)
    #[serde(default = "default_disagreement_penalty")]
    pub disagreement_penalty: f64,

    /// Consensus query timeout in seconds (default: 10)
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,

    /// Maximum number of concurrent consensus operations (default: 100)
    /// Provides backpressure to prevent overwhelming upstreams during traffic spikes
    #[serde(default = "default_max_concurrent_queries")]
    pub max_concurrent_queries: usize,

    /// Prefer `NonEmpty` responses over `Empty`/`ConsensusError` when selecting consensus winner.
    /// When enabled, actual data takes priority over null results.
    #[serde(default)]
    pub prefer_non_empty: bool,

    /// When true, `NonEmpty` responses win regardless of vote count.
    /// When false (default), `NonEmpty` must still meet `min_count` threshold.
    #[serde(default)]
    pub prefer_non_empty_always: bool,

    /// Prefer larger response bodies among `NonEmpty` responses with equal vote counts.
    /// Useful when upstreams return different levels of detail.
    #[serde(default)]
    pub prefer_larger_responses: bool,

    /// Per-method fields to exclude from response hashing.
    /// Keys are method names, values are field paths to ignore.
    /// Supports dot notation (`"timestamp"`, `"result.blockHash"`) and wildcards
    /// (`"transactions.*"`).
    ///
    /// Example:
    /// ```toml
    /// [consensus.ignore_fields]
    /// eth_getBlockByNumber = ["timestamp", "requestsHash"]
    /// eth_getLogs = ["*.blockTimestamp"]
    /// ```
    #[serde(default)]
    pub ignore_fields: HashMap<String, Vec<String>>,

    // ─────────────────────────────────────────────────────────────────────────
    // Misbehavior tracking configuration
    // ─────────────────────────────────────────────────────────────────────────
    /// Number of disputes within window before triggering sit-out.
    /// If None, misbehavior tracking is disabled.
    #[serde(default)]
    pub dispute_threshold: Option<u32>,

    /// Time window in seconds for counting disputes (default: 300).
    #[serde(default = "default_dispute_window")]
    pub dispute_window_seconds: u64,

    /// Duration in seconds for sit-out penalty (default: 60).
    #[serde(default = "default_sit_out_penalty")]
    pub sit_out_penalty_seconds: u64,

    /// Optional path to log misbehavior events (JSONL format).
    #[serde(default)]
    pub misbehavior_log_path: Option<std::path::PathBuf>,

    /// Maximum log file size in bytes before rotation (default: 100MB).
    #[serde(default = "default_max_log_size")]
    pub misbehavior_max_log_file_size_bytes: u64,
}

fn default_max_count() -> usize {
    3
}

fn default_min_count() -> usize {
    2
}

fn default_consensus_methods() -> Vec<String> {
    vec![
        "eth_getBlockByNumber".to_string(),
        "eth_getBlockByHash".to_string(),
        "eth_getTransactionByHash".to_string(),
        "eth_getTransactionReceipt".to_string(),
        "eth_getLogs".to_string(),
    ]
}

fn default_disagreement_penalty() -> f64 {
    10.0
}

fn default_timeout_seconds() -> u64 {
    10
}

fn default_max_concurrent_queries() -> usize {
    100
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

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_count: default_max_count(),
            min_count: default_min_count(),
            dispute_behavior: DisputeBehavior::default(),
            failure_behavior: FailureBehavior::default(),
            low_participants_behavior: LowParticipantsBehavior::default(),
            methods: default_consensus_methods(),
            disagreement_penalty: default_disagreement_penalty(),
            timeout_seconds: default_timeout_seconds(),
            max_concurrent_queries: default_max_concurrent_queries(),
            prefer_non_empty: false,
            prefer_non_empty_always: false,
            prefer_larger_responses: false,
            ignore_fields: HashMap::new(),
            // Misbehavior tracking (disabled by default)
            dispute_threshold: None,
            dispute_window_seconds: default_dispute_window(),
            sit_out_penalty_seconds: default_sit_out_penalty(),
            misbehavior_log_path: None,
            misbehavior_max_log_file_size_bytes: default_max_log_size(),
        }
    }
}

impl ConsensusConfig {
    /// Creates a [`MisbehaviorConfig`] from the consensus configuration.
    ///
    /// Returns the misbehavior config which can be used to initialize a [`MisbehaviorTracker`].
    ///
    /// [`MisbehaviorConfig`]: crate::upstream::misbehavior::MisbehaviorConfig
    /// [`MisbehaviorTracker`]: crate::upstream::misbehavior::MisbehaviorTracker
    #[must_use]
    pub fn misbehavior_config(&self) -> super::super::misbehavior::MisbehaviorConfig {
        super::super::misbehavior::MisbehaviorConfig {
            dispute_threshold: self.dispute_threshold,
            dispute_window_seconds: self.dispute_window_seconds,
            sit_out_penalty_seconds: self.sit_out_penalty_seconds,
            log_path: self.misbehavior_log_path.clone(),
            max_log_file_size_bytes: self.misbehavior_max_log_file_size_bytes,
        }
    }

    /// Returns whether misbehavior tracking is enabled.
    #[must_use]
    pub fn misbehavior_enabled(&self) -> bool {
        self.dispute_threshold.is_some()
    }
}

/// Dispute resolution strategy when responses don't match.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub enum DisputeBehavior {
    /// Prefer chain tip leader
    #[default]
    PreferBlockHeadLeader,
    /// Return consensus failure error
    ReturnError,
    /// Accept any valid response
    AcceptAnyValid,
    /// Prefer highest score upstream
    PreferHighestScore,
}

/// Failure handling strategy.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub enum FailureBehavior {
    /// Return error on any failure
    ReturnError,
    /// Accept any valid response
    #[default]
    AcceptAnyValid,
    /// Use highest-scoring upstream
    UseHighestScore,
}

/// Low participant count handling strategy.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub enum LowParticipantsBehavior {
    /// Only query block head leader
    OnlyBlockHeadLeader,
    /// Return insufficient upstreams error
    ReturnError,
    /// Accept available upstreams
    #[default]
    AcceptAvailable,
}
