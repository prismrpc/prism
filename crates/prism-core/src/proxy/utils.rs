use crate::{
    cache::types::LogFilter,
    utils::hex_buffer::{format_address, format_hash32, format_hex_u64},
};

/// Converts a `LogFilter` back to JSON-RPC parameter format.
///
/// This function transforms the internal `LogFilter` representation back into
/// the JSON format expected by Ethereum RPC methods like `eth_getLogs`. This is
/// primarily used when forwarding cache-miss requests to upstream providers.
///
/// # Arguments
///
/// * `filter` - The log filter to convert
///
/// # Returns
///
/// A JSON array containing a single filter object, formatted according to
/// the Ethereum JSON-RPC specification. The array format matches what
/// `eth_getLogs` expects as its parameters.
///
/// # Note
///
/// Trailing null topics are automatically stripped to match standard RPC behavior.
#[must_use]
pub fn log_filter_to_json_params(filter: &LogFilter) -> serde_json::Value {
    let mut filter_obj = serde_json::Map::new();

    filter_obj.insert(
        "fromBlock".to_string(),
        serde_json::Value::String(format_hex_u64(filter.from_block)),
    );
    filter_obj
        .insert("toBlock".to_string(), serde_json::Value::String(format_hex_u64(filter.to_block)));

    if let Some(address) = filter.address {
        filter_obj
            .insert("address".to_string(), serde_json::Value::String(format_address(&address)));
    }

    let mut topics = Vec::new();
    let mut has_topics = false;
    for topic in &filter.topics {
        if let Some(topic_bytes) = topic {
            topics.push(serde_json::Value::String(format_hash32(topic_bytes)));
            has_topics = true;
        } else {
            topics.push(serde_json::Value::Null);
        }
    }
    if has_topics {
        // Strip trailing null topics to match standard RPC behavior
        while let Some(last) = topics.last() {
            if last.is_null() {
                topics.pop();
            } else {
                break;
            }
        }
        if !topics.is_empty() {
            filter_obj.insert("topics".to_string(), serde_json::Value::Array(topics));
        }
    }

    if !filter.topics_anywhere.is_empty() {
        let mut topics_anywhere = Vec::new();
        for topic in &filter.topics_anywhere {
            topics_anywhere.push(serde_json::Value::String(format_hash32(topic)));
        }
        filter_obj.insert("topicsAnywhere".to_string(), serde_json::Value::Array(topics_anywhere));
    }

    serde_json::Value::Array(vec![serde_json::Value::Object(filter_obj)])
}
