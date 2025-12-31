//! Tracing layer that captures log events into the `LogBuffer`.
//!
//! This layer integrates with `tracing-subscriber` to capture log events
//! and store them in the in-memory `LogBuffer` for querying via the admin API.

use std::sync::Arc;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

use super::LogBuffer;
use crate::admin::types::LogEntry;

/// A tracing layer that captures log events into a `LogBuffer`.
///
/// This layer converts tracing events into `LogEntry` objects and pushes
/// them to the shared `LogBuffer`, making them queryable via the admin API.
pub struct LogBufferLayer {
    buffer: Arc<LogBuffer>,
}

impl LogBufferLayer {
    /// Creates a new `LogBufferLayer` with the given buffer.
    #[must_use]
    pub fn new(buffer: Arc<LogBuffer>) -> Self {
        Self { buffer }
    }
}

/// Visitor that extracts fields from a tracing event into a JSON object.
struct FieldVisitor {
    fields: serde_json::Map<String, serde_json::Value>,
    message: Option<String>,
}

impl FieldVisitor {
    fn new() -> Self {
        Self { fields: serde_json::Map::new(), message: None }
    }
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let value_str = format!("{value:?}");
        if field.name() == "message" {
            self.message = Some(value_str);
        } else {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::String(value_str));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::String(value.to_string()));
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Number(value.into()));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if let Some(n) = serde_json::Number::from_f64(value) {
            self.fields.insert(field.name().to_string(), serde_json::Value::Number(n));
        } else {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::String(value.to_string()));
        }
    }
}

impl<S> Layer<S> for LogBufferLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // Extract level
        let level = match *event.metadata().level() {
            Level::TRACE => "trace",
            Level::DEBUG => "debug",
            Level::INFO => "info",
            Level::WARN => "warn",
            Level::ERROR => "error",
        };

        // Extract target
        let target = event.metadata().target().to_string();

        // Extract fields and message
        let mut visitor = FieldVisitor::new();
        event.record(&mut visitor);

        let message = visitor.message.unwrap_or_default();
        let fields = serde_json::Value::Object(visitor.fields);

        // Create and push log entry
        let entry = LogEntry::new(level.to_string(), target, message, fields);
        self.buffer.push(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    #[test]
    fn test_log_buffer_layer_captures_events() {
        let buffer = Arc::new(LogBuffer::new(100));
        let layer = LogBufferLayer::new(buffer.clone());

        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(key = "value", "test message");
        });

        assert_eq!(buffer.len(), 1);

        let params = crate::admin::types::LogQueryParams {
            level: None,
            target: None,
            search: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };
        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].level, "info");
        assert_eq!(response.entries[0].message, "test message");
    }

    #[test]
    fn test_log_buffer_layer_captures_audit_events() {
        let buffer = Arc::new(LogBuffer::new(100));
        let layer = LogBufferLayer::new(buffer.clone());

        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                target: "audit",
                operation = "CREATE",
                resource_type = "upstream",
                "admin_api_audit"
            );
        });

        let params = crate::admin::types::LogQueryParams {
            level: None,
            target: Some("audit".to_string()),
            search: None,
            from: None,
            to: None,
            limit: 10,
            offset: 0,
        };
        let response = buffer.query(&params);
        assert_eq!(response.entries.len(), 1);
        assert_eq!(response.entries[0].target, "audit");
    }
}
