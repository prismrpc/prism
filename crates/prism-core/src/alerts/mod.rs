//! Alert system for monitoring and notification.
//!
//! Provides alert rule management and alert lifecycle handling for monitoring
//! the health and performance of the Prism RPC proxy.
//!
//! ## Components
//!
//! - **[`AlertManager`]**: Central manager for alerts and alert rules
//! - **[`AlertEvaluator`]**: Background task for evaluating alert rules
//! - **[`Alert`]**: Individual alert instances
//! - **[`AlertRule`]**: Rules defining when alerts should be created
//! - **[`AlertCondition`]**: Conditions that trigger alerts
//!
//! ## Usage
//!
//! ```rust
//! use prism_core::alerts::{Alert, AlertCondition, AlertManager, AlertRule, AlertSeverity};
//!
//! let manager = AlertManager::new();
//!
//! // Create a rule
//! let rule = AlertRule::new(
//!     "high-latency".to_string(),
//!     "High Latency Alert".to_string(),
//!     AlertCondition::HighLatency { threshold_ms: 1000 },
//!     AlertSeverity::Warning,
//!     true,
//!     300,
//! );
//! manager.add_rule(rule);
//!
//! // Create an alert (normally done by background evaluation task)
//! let alert = Alert::new(
//!     "alert-123".to_string(),
//!     "high-latency".to_string(),
//!     AlertSeverity::Warning,
//!     "Average latency exceeded 1000ms".to_string(),
//! );
//! manager.create_alert(alert);
//!
//! // Acknowledge the alert
//! manager.acknowledge_alert("alert-123");
//!
//! // Resolve the alert
//! manager.resolve_alert("alert-123");
//! ```

pub mod evaluator;
pub mod manager;
pub mod types;

pub use evaluator::AlertEvaluator;
pub use manager::AlertManager;
pub use types::{Alert, AlertCondition, AlertRule, AlertSeverity, AlertStatus};
