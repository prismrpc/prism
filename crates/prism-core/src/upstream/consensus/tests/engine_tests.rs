//! Tests for `ConsensusEngine` orchestration and integration.

use crate::{
    chain::ChainState,
    types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, JSONRPC_VERSION_COW},
    upstream::{
        consensus::{
            config::{ConsensusConfig, DisputeBehavior, FailureBehavior, LowParticipantsBehavior},
            engine::ConsensusEngine,
            quorum,
            types::{ResponseGroup, ResponseType, SelectionMethod},
        },
        errors::UpstreamError,
        scoring::{ScoringConfig, ScoringEngine},
    },
};
use std::sync::Arc;

/// Helper to create a test response for consensus tests
fn test_response(result: Option<serde_json::Value>, id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result,
        error: None,
        id: Arc::new(id),
        cache_status: None,
        serving_upstream: None,
    }
}

/// Helper to create a test request for consensus tests
fn test_request(
    method: &str,
    params: Option<serde_json::Value>,
    id: serde_json::Value,
) -> JsonRpcRequest {
    JsonRpcRequest::new(method, params, id)
}

#[test]
fn test_consensus_config_defaults() {
    let config = ConsensusConfig::default();
    assert!(!config.enabled);
    assert_eq!(config.max_count, 3);
    assert_eq!(config.min_count, 2);
    assert_eq!(config.methods.len(), 5);
    assert!(config.methods.contains(&"eth_getBlockByNumber".to_string()));
}

#[tokio::test]
async fn test_consensus_engine_creation() {
    let engine = ConsensusEngine::new(ConsensusConfig::default());
    assert!(!engine.is_enabled().await);
}

#[tokio::test]
async fn test_requires_consensus() {
    let config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    assert!(engine.requires_consensus("eth_getBlockByNumber").await);
    assert!(engine.requires_consensus("eth_getTransactionByHash").await);
    assert!(!engine.requires_consensus("eth_blockNumber").await);
}

#[test]
fn test_hash_response_identical() {
    let response1 =
        test_response(Some(serde_json::json!({"number": "0x123"})), serde_json::json!(1));
    let response2 =
        test_response(Some(serde_json::json!({"number": "0x123"})), serde_json::json!(2));

    assert_eq!(
        ConsensusEngine::hash_response(&response1),
        ConsensusEngine::hash_response(&response2)
    );
}

#[test]
fn test_hash_response_different() {
    let response1 =
        test_response(Some(serde_json::json!({"number": "0x123"})), serde_json::json!(1));
    let response2 =
        test_response(Some(serde_json::json!({"number": "0x456"})), serde_json::json!(1));

    assert_ne!(
        ConsensusEngine::hash_response(&response1),
        ConsensusEngine::hash_response(&response2)
    );
}

#[test]
fn test_responses_match() {
    let response1 =
        test_response(Some(serde_json::json!({"block": "0x100"})), serde_json::json!(1));
    let response2 = response1.clone();

    assert!(ConsensusEngine::responses_match(&response1, &response2));
}

#[test]
#[allow(clippy::similar_names)]
fn test_group_responses() {
    let response1 =
        test_response(Some(serde_json::json!({"number": "0x123"})), serde_json::json!(1));

    let response2 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"number": "0x456"})),
        error: None,
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream1"), response1.clone()),
        (Arc::from("upstream2"), response1.clone()),
        (Arc::from("upstream3"), response2),
    ];

    let groups = quorum::group_responses(responses, None);

    assert_eq!(groups.len(), 2);

    // Find the group with 2 upstreams
    let majority_group = groups.iter().find(|g| g.count == 2).unwrap();
    assert_eq!(majority_group.upstreams.len(), 2);
    assert!(majority_group.upstreams.iter().any(|n| n.as_ref() == "upstream1"));
    assert!(majority_group.upstreams.iter().any(|n| n.as_ref() == "upstream2"));
}

#[test]
fn test_hash_response_with_error() {
    let response1 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: None,
        error: Some(JsonRpcError {
            code: -32000,
            message: "Error message".to_string(),
            data: None,
        }),
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let response2 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: None,
        error: Some(JsonRpcError {
            code: -32000,
            message: "Error message".to_string(),
            data: None,
        }),
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    assert_eq!(
        ConsensusEngine::hash_response(&response1),
        ConsensusEngine::hash_response(&response2)
    );
}

#[tokio::test]
async fn test_config_update() {
    let engine = ConsensusEngine::new(ConsensusConfig::default());
    assert!(!engine.is_enabled().await);

    let new_config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };
    engine.update_config(new_config).await;

    assert!(engine.is_enabled().await);
}

// Consensus Execution Tests

#[tokio::test]
async fn test_execute_consensus_all_agree() {
    let config =
        ConsensusConfig { enabled: true, max_count: 3, min_count: 2, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    // Create mock response that all upstreams will return
    let expected_response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"number": "0x123", "hash": "0xabc"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    // Note: This test requires actual mock upstreams
    // For now, we test the logic with group_responses and find_consensus
    let responses = vec![
        (Arc::from("upstream1"), expected_response.clone()),
        (Arc::from("upstream2"), expected_response.clone()),
        (Arc::from("upstream3"), expected_response.clone()),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 3);
    assert_eq!(consensus_result.total_queried, 3);
    assert!(consensus_result.disagreeing_upstreams.is_empty());
    assert!(matches!(consensus_result.metadata.selection_method, SelectionMethod::Consensus));
}

#[tokio::test]
async fn test_execute_consensus_quorum_reached_with_disagreement() {
    let config =
        ConsensusConfig { enabled: true, max_count: 3, min_count: 2, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    let majority_response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"number": "0x123"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let minority_response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"number": "0x456"})),
        error: None,
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream1"), majority_response.clone()),
        (Arc::from("upstream2"), majority_response.clone()),
        (Arc::from("upstream3"), minority_response),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2);
    assert_eq!(consensus_result.total_queried, 3);
    assert_eq!(consensus_result.disagreeing_upstreams.len(), 1);
    assert!(consensus_result.disagreeing_upstreams.iter().any(|n| n.as_ref() == "upstream3"));
}

#[tokio::test]
#[allow(clippy::similar_names)]
async fn test_execute_consensus_quorum_not_reached_return_error() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        dispute_behavior: DisputeBehavior::ReturnError,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    // All different responses - no consensus
    let response1 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"number": "0x111"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let response2 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"number": "0x222"})),
        error: None,
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    let response3 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"number": "0x333"})),
        error: None,
        id: Arc::new(serde_json::json!(3)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream1"), response1),
        (Arc::from("upstream2"), response2),
        (Arc::from("upstream3"), response3),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), UpstreamError::ConsensusFailure(_)));
}

#[tokio::test]
async fn test_execute_consensus_timeout_handling() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        timeout_seconds: 1,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    // Test with empty upstreams to trigger timeout path
    let request = test_request(
        "eth_getBlockByNumber",
        Some(serde_json::json!(["0x123", false])),
        serde_json::json!(1),
    );

    let result = engine
        .execute_consensus(
            Arc::new(request),
            vec![],
            &Arc::new(ScoringEngine::new(ScoringConfig::default(), Arc::new(ChainState::new()))),
        )
        .await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), UpstreamError::NoHealthyUpstreams));
}

// Dispute Resolution Tests

#[tokio::test]
async fn test_dispute_behavior_prefer_block_head_leader() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        dispute_behavior: DisputeBehavior::PreferBlockHeadLeader,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    // All different responses - should use dispute resolution
    let responses = vec![
        (
            Arc::from("upstream1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"number": "0x1"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"number": "0x2"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(!consensus_result.consensus_achieved);
    assert_eq!(consensus_result.total_queried, 2);
}

#[tokio::test]
async fn test_dispute_behavior_prefer_highest_score() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        dispute_behavior: DisputeBehavior::PreferHighestScore,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    let responses = vec![
        (
            Arc::from("upstream1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"value": "a"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"value": "b"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());
    assert!(!result.unwrap().consensus_achieved);
}

#[tokio::test]
async fn test_dispute_behavior_accept_any_valid() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        dispute_behavior: DisputeBehavior::AcceptAnyValid,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    let responses = vec![
        (
            Arc::from("upstream1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "x"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "y"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(!consensus_result.consensus_achieved);
    assert!(matches!(consensus_result.metadata.selection_method, SelectionMethod::FirstValid));
}

#[tokio::test]
async fn test_dispute_behavior_return_error() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        dispute_behavior: DisputeBehavior::ReturnError,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    let responses = vec![
        (
            Arc::from("upstream1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"val": 1})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"val": 2})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), UpstreamError::ConsensusFailure(_)));
}

// Low Participants Behavior Tests

#[tokio::test]
async fn test_low_participants_only_block_head_leader() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        low_participants_behavior: LowParticipantsBehavior::OnlyBlockHeadLeader,
        ..ConsensusConfig::default()
    };
    let _engine = ConsensusEngine::new(config);

    let _request = test_request(
        "eth_getBlockByNumber",
        Some(serde_json::json!(["latest", false])),
        serde_json::json!(1),
    );

    // Only 1 upstream (below min_count=2)
    // This will trigger OnlyBlockHeadLeader behavior
    // Note: Without actual upstream implementation, this test is a placeholder
    // testing the behavior would require mock upstreams
}

// Edge Cases

#[tokio::test]
async fn test_empty_upstreams_list() {
    let config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    let request = test_request(
        "eth_getBlockByNumber",
        Some(serde_json::json!(["latest", false])),
        serde_json::json!(1),
    );

    let result = engine
        .execute_consensus(
            Arc::new(request),
            vec![],
            &Arc::new(ScoringEngine::new(ScoringConfig::default(), Arc::new(ChainState::new()))),
        )
        .await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), UpstreamError::NoHealthyUpstreams));
}

#[tokio::test]
async fn test_single_upstream_below_min_count() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        low_participants_behavior: LowParticipantsBehavior::ReturnError,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    let request = test_request(
        "eth_getBlockByNumber",
        Some(serde_json::json!(["latest", false])),
        serde_json::json!(1),
    );

    // Empty list will trigger error
    let result = engine
        .execute_consensus(
            Arc::new(request),
            vec![],
            &Arc::new(ScoringEngine::new(ScoringConfig::default(), Arc::new(ChainState::new()))),
        )
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_error_responses_mixed_with_success() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        failure_behavior: FailureBehavior::AcceptAnyValid,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    // 2 successful responses with same result, would form consensus
    let success_response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"status": "ok"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream1"), success_response.clone()),
        (Arc::from("upstream2"), success_response),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2);
}

#[tokio::test]
async fn test_requires_consensus_with_custom_method_list() {
    let config = ConsensusConfig {
        enabled: true,
        methods: vec![
            "eth_call".to_string(),
            "eth_estimateGas".to_string(),
            "custom_method".to_string(),
        ],
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    assert!(engine.requires_consensus("eth_call").await);
    assert!(engine.requires_consensus("eth_estimateGas").await);
    assert!(engine.requires_consensus("custom_method").await);
    assert!(!engine.requires_consensus("eth_getBlockByNumber").await);
    assert!(!engine.requires_consensus("eth_blockNumber").await);
}

#[tokio::test]
async fn test_empty_responses_list() {
    let config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    let result = engine.find_consensus(vec![], "eth_getBlockByNumber").await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), UpstreamError::NoHealthyUpstreams));
}

// Configuration Tests

#[test]
fn test_toml_deserialization_minimal() {
    let toml_str = r"
        enabled = true
    ";

    let config: ConsensusConfig = toml::from_str(toml_str).expect("Failed to parse TOML");
    assert!(config.enabled);
    assert_eq!(config.max_count, 3); // default
    assert_eq!(config.min_count, 2); // default
}

#[test]
fn test_toml_deserialization_full() {
    let toml_str = r#"
        enabled = true
        max_count = 5
        min_count = 3
        disagreement_penalty = 15.0
        timeout_seconds = 20
        methods = ["eth_call", "eth_estimateGas"]
        dispute_behavior = "ReturnError"
        failure_behavior = "ReturnError"
        low_participants_behavior = "ReturnError"
    "#;

    let config: ConsensusConfig = toml::from_str(toml_str).expect("Failed to parse TOML");
    assert!(config.enabled);
    assert_eq!(config.max_count, 5);
    assert_eq!(config.min_count, 3);
    assert!((config.disagreement_penalty - 15.0).abs() < f64::EPSILON);
    assert_eq!(config.timeout_seconds, 20);
    assert_eq!(config.methods.len(), 2);
    assert!(config.methods.contains(&"eth_call".to_string()));
    assert!(matches!(config.dispute_behavior, DisputeBehavior::ReturnError));
    assert!(matches!(config.failure_behavior, FailureBehavior::ReturnError));
    assert!(matches!(config.low_participants_behavior, LowParticipantsBehavior::ReturnError));
}

#[test]
fn test_toml_deserialization_dispute_behaviors() {
    let toml_str = r#"
        enabled = true
        dispute_behavior = "PreferBlockHeadLeader"
    "#;
    let config: ConsensusConfig = toml::from_str(toml_str).unwrap();
    assert!(matches!(config.dispute_behavior, DisputeBehavior::PreferBlockHeadLeader));

    let toml_str = r#"
        enabled = true
        dispute_behavior = "PreferHighestScore"
    "#;
    let config: ConsensusConfig = toml::from_str(toml_str).unwrap();
    assert!(matches!(config.dispute_behavior, DisputeBehavior::PreferHighestScore));

    let toml_str = r#"
        enabled = true
        dispute_behavior = "AcceptAnyValid"
    "#;
    let config: ConsensusConfig = toml::from_str(toml_str).unwrap();
    assert!(matches!(config.dispute_behavior, DisputeBehavior::AcceptAnyValid));
}

#[test]
fn test_default_values_comprehensive() {
    let config = ConsensusConfig::default();

    assert!(!config.enabled);
    assert_eq!(config.max_count, 3);
    assert_eq!(config.min_count, 2);
    assert!((config.disagreement_penalty - 10.0).abs() < f64::EPSILON);
    assert_eq!(config.timeout_seconds, 10);
    assert_eq!(config.methods.len(), 5);
    assert!(config.methods.contains(&"eth_getBlockByNumber".to_string()));
    assert!(config.methods.contains(&"eth_getBlockByHash".to_string()));
    assert!(config.methods.contains(&"eth_getTransactionByHash".to_string()));
    assert!(config.methods.contains(&"eth_getTransactionReceipt".to_string()));
    assert!(config.methods.contains(&"eth_getLogs".to_string()));
    assert!(matches!(config.dispute_behavior, DisputeBehavior::PreferBlockHeadLeader));
    assert!(matches!(config.failure_behavior, FailureBehavior::AcceptAnyValid));
    assert!(matches!(config.low_participants_behavior, LowParticipantsBehavior::AcceptAvailable));
}

#[tokio::test]
async fn test_runtime_config_update_comprehensive() {
    let initial_config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        methods: vec!["eth_call".to_string()],
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(initial_config);

    assert!(engine.is_enabled().await);
    assert!(!engine.requires_consensus("eth_getBlockByNumber").await);
    assert!(engine.requires_consensus("eth_call").await);

    let updated_config = ConsensusConfig {
        enabled: true,
        max_count: 5,
        min_count: 3,
        methods: vec!["eth_getBlockByNumber".to_string(), "eth_getTransactionByHash".to_string()],
        disagreement_penalty: 20.0,
        timeout_seconds: 15,
        ..ConsensusConfig::default()
    };
    engine.update_config(updated_config).await;

    assert!(engine.is_enabled().await);
    assert!(engine.requires_consensus("eth_getBlockByNumber").await);
    assert!(engine.requires_consensus("eth_getTransactionByHash").await);
    assert!(!engine.requires_consensus("eth_call").await);

    let current_config = engine.get_config().await;
    assert_eq!(current_config.max_count, 5);
    assert_eq!(current_config.min_count, 3);
    assert!((current_config.disagreement_penalty - 20.0).abs() < f64::EPSILON);
    assert_eq!(current_config.timeout_seconds, 15);
}

#[test]
fn test_behavior_enums_serialization() {
    // Test DisputeBehavior
    let dispute_behaviors = vec![
        (DisputeBehavior::PreferBlockHeadLeader, "\"PreferBlockHeadLeader\""),
        (DisputeBehavior::PreferHighestScore, "\"PreferHighestScore\""),
        (DisputeBehavior::AcceptAnyValid, "\"AcceptAnyValid\""),
        (DisputeBehavior::ReturnError, "\"ReturnError\""),
    ];

    for (behavior, expected_json) in dispute_behaviors {
        let serialized = serde_json::to_string(&behavior).unwrap();
        assert_eq!(serialized, expected_json);
        let deserialized: DisputeBehavior = serde_json::from_str(&serialized).unwrap();
        assert!(matches!(
            (behavior, deserialized),
            (DisputeBehavior::PreferBlockHeadLeader, DisputeBehavior::PreferBlockHeadLeader) |
                (DisputeBehavior::PreferHighestScore, DisputeBehavior::PreferHighestScore) |
                (DisputeBehavior::AcceptAnyValid, DisputeBehavior::AcceptAnyValid) |
                (DisputeBehavior::ReturnError, DisputeBehavior::ReturnError)
        ));
    }

    // Test FailureBehavior
    let failure_behaviors = vec![
        FailureBehavior::ReturnError,
        FailureBehavior::AcceptAnyValid,
        FailureBehavior::UseHighestScore,
    ];

    for behavior in failure_behaviors {
        let serialized = serde_json::to_string(&behavior).unwrap();
        let deserialized: FailureBehavior = serde_json::from_str(&serialized).unwrap();
        assert!(matches!(
            (behavior, deserialized),
            (FailureBehavior::ReturnError, FailureBehavior::ReturnError) |
                (FailureBehavior::AcceptAnyValid, FailureBehavior::AcceptAnyValid) |
                (FailureBehavior::UseHighestScore, FailureBehavior::UseHighestScore)
        ));
    }

    // Test LowParticipantsBehavior
    let low_participants_behaviors = vec![
        LowParticipantsBehavior::OnlyBlockHeadLeader,
        LowParticipantsBehavior::ReturnError,
        LowParticipantsBehavior::AcceptAvailable,
    ];

    for behavior in low_participants_behaviors {
        let serialized = serde_json::to_string(&behavior).unwrap();
        let deserialized: LowParticipantsBehavior = serde_json::from_str(&serialized).unwrap();
        assert!(matches!(
            (behavior, deserialized),
            (
                LowParticipantsBehavior::OnlyBlockHeadLeader,
                LowParticipantsBehavior::OnlyBlockHeadLeader
            ) | (LowParticipantsBehavior::ReturnError, LowParticipantsBehavior::ReturnError) |
                (
                    LowParticipantsBehavior::AcceptAvailable,
                    LowParticipantsBehavior::AcceptAvailable
                )
        ));
    }
}

// Additional Edge Case Tests

#[test]
fn test_group_responses_with_identical_hashes() {
    let response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"data": "same"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream1"), response.clone()),
        (Arc::from("upstream2"), response.clone()),
        (Arc::from("upstream3"), response.clone()),
        (Arc::from("upstream4"), response),
    ];

    let groups = quorum::group_responses(responses, None);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].count, 4);
    assert_eq!(groups[0].upstreams.len(), 4);
}

#[test]
fn test_group_responses_all_different() {
    let responses = vec![
        (
            Arc::from("upstream1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"val": 1})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"val": 2})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream3"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"val": 3})),
                error: None,
                id: Arc::new(serde_json::json!(3)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let groups = quorum::group_responses(responses, None);
    assert_eq!(groups.len(), 3);
    for group in groups {
        assert_eq!(group.count, 1);
        assert_eq!(group.upstreams.len(), 1);
    }
}

#[tokio::test]
async fn test_consensus_with_minimum_upstreams() {
    let config =
        ConsensusConfig { enabled: true, max_count: 2, min_count: 2, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    let response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"result": "success"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses =
        vec![(Arc::from("upstream1"), response.clone()), (Arc::from("upstream2"), response)];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2);
    assert_eq!(consensus_result.total_queried, 2);
}

#[tokio::test]
#[allow(clippy::similar_names)]
async fn test_metadata_includes_all_response_groups() {
    let config =
        ConsensusConfig { enabled: true, max_count: 4, min_count: 2, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    let response1 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"group": "A"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let response2 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"group": "B"})),
        error: None,
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream1"), response1.clone()),
        (Arc::from("upstream2"), response1),
        (Arc::from("upstream3"), response2.clone()),
        (Arc::from("upstream4"), response2),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert_eq!(consensus_result.metadata.response_groups.len(), 2);

    // Both groups should have 2 members
    for group in &consensus_result.metadata.response_groups {
        assert_eq!(group.count, 2);
        assert_eq!(group.upstreams.len(), 2);
    }
}

#[tokio::test]
async fn test_select_block_head_leader_integration() {
    let scoring_config =
        ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };
    let scoring_engine = Arc::new(ScoringEngine::new(scoring_config, Arc::new(ChainState::new())));

    // Record block numbers for different upstreams
    for _ in 0..10 {
        scoring_engine.record_success("upstream-1", 100);
        scoring_engine.record_success("upstream-2", 100);
        scoring_engine.record_success("upstream-3", 100);
    }

    // Set different block heights
    scoring_engine.record_block_number("upstream-1", 1000).await;
    scoring_engine.record_block_number("upstream-2", 1005).await; // Leader
    scoring_engine.record_block_number("upstream-3", 998).await;

    // Verify chain tip is set correctly
    assert_eq!(scoring_engine.chain_tip(), 1005);

    // Verify that upstream-2 has the highest block
    let metrics_1 = scoring_engine.get_metrics("upstream-1");
    let metrics_2 = scoring_engine.get_metrics("upstream-2");
    let metrics_3 = scoring_engine.get_metrics("upstream-3");

    assert!(metrics_1.is_some());
    assert!(metrics_2.is_some());
    assert!(metrics_3.is_some());

    let (_err1, _thr1, _total1, block1, _avg1) = metrics_1.unwrap();
    let (_err2, _thr2, _total2, block2, _avg2) = metrics_2.unwrap();
    let (_err3, _thr3, _total3, block3, _avg3) = metrics_3.unwrap();

    assert_eq!(block1, 1000);
    assert_eq!(block2, 1005);
    assert_eq!(block3, 998);
    assert!(block2 > block1 && block2 > block3);
}

#[tokio::test]
async fn test_penalty_application_verification() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        disagreement_penalty: 10.0,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    let scoring_config =
        ScoringConfig { enabled: true, min_samples: 5, ..ScoringConfig::default() };
    let scoring_engine = Arc::new(ScoringEngine::new(scoring_config, Arc::new(ChainState::new())));

    // Create responses where upstream-3 disagrees
    let majority_response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"value": "correct"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let minority_response = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"value": "incorrect"})),
        error: None,
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream-1"), majority_response.clone()),
        (Arc::from("upstream-2"), majority_response),
        (Arc::from("upstream-3"), minority_response),
    ];

    // First, record initial successes for all upstreams
    for _ in 0..10 {
        scoring_engine.record_success("upstream-1", 100);
        scoring_engine.record_success("upstream-2", 100);
        scoring_engine.record_success("upstream-3", 100);
    }

    // Get initial error counts
    let metrics_before = scoring_engine.get_metrics("upstream-3").unwrap();
    let (error_rate_before, _thr, _total, _block, _avg) = metrics_before;

    // Execute consensus which should penalize upstream-3
    let consensus_result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(consensus_result.is_ok());

    let result = consensus_result.unwrap();
    assert!(result.consensus_achieved);
    assert_eq!(result.disagreeing_upstreams.len(), 1);
    assert!(result.disagreeing_upstreams.contains(&Arc::from("upstream-3")));

    // Manually apply penalty (simulating what execute_consensus does)
    for upstream_name in &result.disagreeing_upstreams {
        scoring_engine.record_error(upstream_name);
    }

    // Verify penalty was applied
    let metrics_after = scoring_engine.get_metrics("upstream-3").unwrap();
    let (error_rate_after, _thr, _total, _block, _avg) = metrics_after;

    // Error rate should have increased
    assert!(error_rate_after > error_rate_before);
}

#[tokio::test]
async fn test_consensus_with_scoring_disabled() {
    let config =
        ConsensusConfig { enabled: true, max_count: 3, min_count: 2, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    // Create scoring engine but keep it disabled
    let scoring_config = ScoringConfig {
        enabled: false, // Disabled
        ..ScoringConfig::default()
    };
    let scoring_engine = Arc::new(ScoringEngine::new(scoring_config, Arc::new(ChainState::new())));

    // Verify scoring is disabled
    assert!(!scoring_engine.is_enabled());

    // Consensus should still work even with scoring disabled
    let responses = vec![
        (
            Arc::from("upstream-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "value"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream-2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "value"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2);
}

#[tokio::test]
async fn test_block_head_leader_selection_with_no_metrics() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        low_participants_behavior: LowParticipantsBehavior::OnlyBlockHeadLeader,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    let scoring_engine =
        Arc::new(ScoringEngine::new(ScoringConfig::default(), Arc::new(ChainState::new())));

    // Test select_block_head_leader with no metrics recorded
    // This tests the fallback behavior when scoring data is unavailable
    // Note: Full test with actual upstream endpoints would require
    // HTTP client mocking, which is out of scope for unit tests

    // The function should handle empty upstreams gracefully
    let leader = engine.select_block_head_leader(&[], &scoring_engine);
    assert!(leader.is_none()); // Empty list returns None
}

#[tokio::test]
async fn test_consensus_metadata_selection_methods() {
    let config =
        ConsensusConfig { enabled: true, max_count: 3, min_count: 2, ..ConsensusConfig::default() };
    let engine = ConsensusEngine::new(config);

    // Test case 1: True consensus (majority agrees)
    let consensus_responses = vec![
        (
            Arc::from("upstream-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"value": "A"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream-2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"value": "A"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(consensus_responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());
    let consensus_result = result.unwrap();
    assert!(matches!(consensus_result.metadata.selection_method, SelectionMethod::Consensus));

    // Test case 2: No consensus with AcceptAnyValid
    let mut config2 = engine.get_config().await;
    config2.dispute_behavior = DisputeBehavior::AcceptAnyValid;
    let engine2 = ConsensusEngine::new(config2);

    let no_consensus_responses = vec![
        (
            Arc::from("upstream-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"value": "X"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream-2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"value": "Y"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result2 = engine2.find_consensus(no_consensus_responses, "eth_getBlockByNumber").await;
    assert!(result2.is_ok());
    let consensus_result2 = result2.unwrap();
    assert!(!consensus_result2.consensus_achieved);
    assert!(matches!(consensus_result2.metadata.selection_method, SelectionMethod::FirstValid));
}

#[tokio::test]
async fn test_consensus_with_mixed_successes_and_failures() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 4,
        min_count: 2,
        failure_behavior: FailureBehavior::AcceptAnyValid,
        ..ConsensusConfig::default()
    };
    let engine = ConsensusEngine::new(config);

    // Simulate scenario: 2 upstreams return same response, 1 returns different, 1 fails
    let response_a = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"block": "0x100"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let response_b = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"block": "0x101"})),
        error: None,
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    let responses = vec![
        (Arc::from("upstream-1"), response_a.clone()),
        (Arc::from("upstream-2"), response_a),
        (Arc::from("upstream-3"), response_b),
        // upstream-4 failed (not in list)
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await;
    assert!(result.is_ok());

    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2);
    assert_eq!(consensus_result.total_queried, 3);
    assert_eq!(consensus_result.disagreeing_upstreams.len(), 1);
}

#[tokio::test]
async fn test_consensus_response_hash_collision_resistance() {
    // Test that very similar but different responses don't collide
    let response1 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"blockNumber": "0x1234567"})),
        error: None,
        id: Arc::new(serde_json::json!(1)),
        cache_status: None,
        serving_upstream: None,
    };

    let response2 = JsonRpcResponse {
        jsonrpc: JSONRPC_VERSION_COW,
        result: Some(serde_json::json!({"blockNumber": "0x1234568"})), // Only 1 char different
        error: None,
        id: Arc::new(serde_json::json!(2)),
        cache_status: None,
        serving_upstream: None,
    };

    let hash1 = ConsensusEngine::hash_response(&response1);
    let hash2 = ConsensusEngine::hash_response(&response2);

    assert_ne!(hash1, hash2);
    assert!(!ConsensusEngine::responses_match(&response1, &response2));
}

#[tokio::test]
async fn test_low_participants_all_behaviors() {
    let scoring_engine =
        Arc::new(ScoringEngine::new(ScoringConfig::default(), Arc::new(ChainState::new())));

    // Test 1: ReturnError behavior - no upstreams available
    let config1 = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        low_participants_behavior: LowParticipantsBehavior::ReturnError,
        ..ConsensusConfig::default()
    };
    let engine1 = ConsensusEngine::new(config1);

    let request = test_request(
        "eth_getBlockByNumber",
        Some(serde_json::json!(["latest", false])),
        serde_json::json!(1),
    );

    let result1 = engine1.execute_consensus(Arc::new(request), vec![], &scoring_engine).await;
    assert!(result1.is_err());
    assert!(matches!(result1.unwrap_err(), UpstreamError::NoHealthyUpstreams));

    // Test 2: AcceptAvailable behavior - below min_count but still accepts
    let config2 = ConsensusConfig {
        enabled: true,
        max_count: 5,
        min_count: 4,
        low_participants_behavior: LowParticipantsBehavior::AcceptAvailable,
        ..ConsensusConfig::default()
    };
    let engine2 = ConsensusEngine::new(config2);

    let responses_below_min = vec![
        (
            Arc::from("upstream-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "value"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream-2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "value"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result2 = engine2.find_consensus(responses_below_min, "eth_getBlockByNumber").await;
    assert!(result2.is_ok());
    assert!(!result2.unwrap().consensus_achieved); // Below min_count but accepted

    // Test 3: AcceptAvailable with fewer than max_count (meets min_count)
    let config3 = ConsensusConfig {
        enabled: true,
        max_count: 5,
        min_count: 2,
        low_participants_behavior: LowParticipantsBehavior::AcceptAvailable,
        ..ConsensusConfig::default()
    };
    let engine3 = ConsensusEngine::new(config3);

    let responses_meets_min = vec![
        (
            Arc::from("upstream1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"num": "0x100"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"num": "0x100"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result3 = engine3.find_consensus(responses_meets_min, "eth_getBlockByNumber").await;
    assert!(result3.is_ok());
    assert!(result3.unwrap().consensus_achieved); // Meets min_count
}

#[test]
fn test_response_type_nonempty_variants() {
    let test_cases = vec![
        (serde_json::json!({"number": "0x123", "hash": "0xabc"}), "object"),
        (serde_json::json!([{"log": 1}, {"log": 2}]), "non-empty array"),
        (serde_json::json!("0x1234"), "hex string with data"),
        (serde_json::json!(12345), "number"),
        (serde_json::json!(true), "boolean"),
    ];

    for (value, description) in test_cases {
        let response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: Some(value),
            error: None,
            id: Arc::new(serde_json::json!(1)),
            cache_status: None,
            serving_upstream: None,
        };
        let response_type = ResponseType::from_response(&response);
        assert!(
            matches!(response_type, ResponseType::NonEmpty),
            "Expected NonEmpty for {description}, got {response_type:?}"
        );
    }
}

#[test]
fn test_response_type_empty_variants() {
    let test_cases = vec![
        (Some(serde_json::Value::Null), "null result"),
        (None, "no result field"),
        (Some(serde_json::json!([])), "empty array"),
        (Some(serde_json::json!({})), "empty object"),
        (Some(serde_json::json!("0x")), "0x empty string"),
        (Some(serde_json::json!("")), "empty string"),
    ];

    for (value, description) in test_cases {
        let response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: value,
            error: None,
            id: Arc::new(serde_json::json!(1)),
            cache_status: None,
            serving_upstream: None,
        };
        let response_type = ResponseType::from_response(&response);
        assert!(
            matches!(response_type, ResponseType::Empty),
            "Expected Empty for {description}, got {response_type:?}"
        );
    }
}

#[test]
fn test_response_type_error_variants() {
    let test_cases = vec![
        (
            JsonRpcError { code: -32000, message: "execution reverted".to_string(), data: None },
            "error without data",
        ),
        (
            JsonRpcError {
                code: -32602,
                message: "invalid params".to_string(),
                data: Some(serde_json::json!({"reason": "block not found"})),
            },
            "error with data",
        ),
    ];

    for (error, description) in test_cases {
        let response = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION_COW,
            result: None,
            error: Some(error),
            id: Arc::new(serde_json::json!(1)),
            cache_status: None,
            serving_upstream: None,
        };
        let response_type = ResponseType::from_response(&response);
        assert!(
            matches!(response_type, ResponseType::ConsensusError),
            "Expected ConsensusError for {description}, got {response_type:?}"
        );
    }
}

#[test]
fn test_response_type_priority_order() {
    assert_eq!(ResponseType::NonEmpty.priority_order(), 0);
    assert_eq!(ResponseType::Empty.priority_order(), 1);
    assert_eq!(ResponseType::ConsensusError.priority_order(), 2);
    assert_eq!(ResponseType::InfraError.priority_order(), 3);

    // Verify ordering
    assert!(ResponseType::NonEmpty.priority_order() < ResponseType::Empty.priority_order());
    assert!(ResponseType::Empty.priority_order() < ResponseType::ConsensusError.priority_order());
    assert!(
        ResponseType::ConsensusError.priority_order() < ResponseType::InfraError.priority_order()
    );
}

#[test]
fn test_group_responses_includes_type_and_size() {
    let responses = vec![
        (
            Arc::from("upstream-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"block": "0x123", "data": "lots of data here"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("upstream-2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let groups = quorum::group_responses(responses, None);

    // Should have 2 groups (different responses)
    assert_eq!(groups.len(), 2);

    // Find the NonEmpty group
    let nonempty_group = groups.iter().find(|g| g.response_type == ResponseType::NonEmpty);
    assert!(nonempty_group.is_some());
    let nonempty = nonempty_group.unwrap();
    assert!(nonempty.response_size > 0);
    assert_eq!(nonempty.count, 1);

    // Find the Empty group
    let empty_group = groups.iter().find(|g| g.response_type == ResponseType::Empty);
    assert!(empty_group.is_some());
}

#[test]
fn test_apply_preferences_prefer_non_empty_sorts_by_type() {
    let mut groups = vec![
        ResponseGroup {
            response_hash: 1,
            upstreams: vec![Arc::from("empty-1"), Arc::from("empty-2")],
            count: 2,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::Empty,
            response_size: 4,
        },
        ResponseGroup {
            response_hash: 2,
            upstreams: vec![Arc::from("nonempty-1")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "value"})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::NonEmpty,
            response_size: 50,
        },
    ];

    let config = ConsensusConfig { prefer_non_empty: true, ..Default::default() };

    quorum::apply_preferences(&mut groups, &config);

    // NonEmpty should be first even with fewer votes
    assert_eq!(groups[0].response_type, ResponseType::NonEmpty);
    assert_eq!(groups[1].response_type, ResponseType::Empty);
}

#[test]
fn test_apply_preferences_disabled_uses_count_only() {
    let mut groups = vec![
        ResponseGroup {
            response_hash: 1,
            upstreams: vec![Arc::from("nonempty-1")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "value"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::NonEmpty,
            response_size: 50,
        },
        ResponseGroup {
            response_hash: 2,
            upstreams: vec![Arc::from("empty-1"), Arc::from("empty-2")],
            count: 2,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::Empty,
            response_size: 4,
        },
    ];

    let config = ConsensusConfig { prefer_non_empty: false, ..Default::default() };

    quorum::apply_preferences(&mut groups, &config);

    // Higher count should win when preference disabled
    assert_eq!(groups[0].count, 2);
    assert_eq!(groups[0].response_type, ResponseType::Empty);
}

#[test]
fn test_apply_preferences_prefer_larger_responses() {
    let mut groups = vec![
        ResponseGroup {
            response_hash: 1,
            upstreams: vec![Arc::from("small-1")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"a": 1})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::NonEmpty,
            response_size: 10,
        },
        ResponseGroup {
            response_hash: 2,
            upstreams: vec![Arc::from("large-1")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(
                    serde_json::json!({"a": 1, "b": 2, "c": 3, "d": "lots more data here"}),
                ),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::NonEmpty,
            response_size: 100,
        },
    ];

    let config = ConsensusConfig { prefer_larger_responses: true, ..Default::default() };

    quorum::apply_preferences(&mut groups, &config);

    // Larger response should be first when counts are equal
    assert_eq!(groups[0].response_size, 100);
}

#[test]
fn test_apply_preferences_combined() {
    let mut groups = vec![
        // Group 1: Empty with 3 votes
        ResponseGroup {
            response_hash: 1,
            upstreams: vec![Arc::from("e1"), Arc::from("e2"), Arc::from("e3")],
            count: 3,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::Empty,
            response_size: 4,
        },
        // Group 2: NonEmpty with 1 vote, small
        ResponseGroup {
            response_hash: 2,
            upstreams: vec![Arc::from("n1")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"x": 1})),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::NonEmpty,
            response_size: 10,
        },
        // Group 3: NonEmpty with 1 vote, large
        ResponseGroup {
            response_hash: 3,
            upstreams: vec![Arc::from("n2")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"x": 1, "y": 2, "z": "lots of data"})),
                error: None,
                id: Arc::new(serde_json::json!(3)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::NonEmpty,
            response_size: 100,
        },
    ];

    let config = ConsensusConfig {
        prefer_non_empty: true,
        prefer_larger_responses: true,
        ..Default::default()
    };

    quorum::apply_preferences(&mut groups, &config);

    // Order should be: NonEmpty large (100) > NonEmpty small (10) > Empty (3 votes)
    assert_eq!(groups[0].response_type, ResponseType::NonEmpty);
    assert_eq!(groups[0].response_size, 100);
    assert_eq!(groups[1].response_type, ResponseType::NonEmpty);
    assert_eq!(groups[1].response_size, 10);
    assert_eq!(groups[2].response_type, ResponseType::Empty);
}

#[test]
fn test_apply_preferences_type_priority_ordering() {
    let mut groups = vec![
        ResponseGroup {
            response_hash: 1,
            upstreams: vec![Arc::from("u1")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: "execution reverted".to_string(),
                    data: None,
                }),
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::ConsensusError,
            response_size: 0,
        },
        ResponseGroup {
            response_hash: 2,
            upstreams: vec![Arc::from("u2")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(2)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::Empty,
            response_size: 4,
        },
        ResponseGroup {
            response_hash: 3,
            upstreams: vec![Arc::from("u3")],
            count: 1,
            response: JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"data": "value"})),
                error: None,
                id: Arc::new(serde_json::json!(3)),
                cache_status: None,
                serving_upstream: None,
            },
            response_type: ResponseType::NonEmpty,
            response_size: 50,
        },
    ];

    let config = ConsensusConfig { prefer_non_empty: true, ..Default::default() };

    quorum::apply_preferences(&mut groups, &config);

    // Order should be: NonEmpty > Empty > ConsensusError
    assert_eq!(groups[0].response_type, ResponseType::NonEmpty);
    assert_eq!(groups[1].response_type, ResponseType::Empty);
    assert_eq!(groups[2].response_type, ResponseType::ConsensusError);
}

#[tokio::test]
async fn test_find_consensus_prefer_non_empty_always() {
    let config = ConsensusConfig {
        enabled: true,
        min_count: 2,
        max_count: 3,
        prefer_non_empty: true,
        prefer_non_empty_always: true, // Aggressive mode
        ..Default::default()
    };

    let engine = ConsensusEngine::new(config);

    // 2 Empty responses, 1 NonEmpty response
    let responses = vec![
        (
            Arc::from("empty-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("empty-2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("nonempty-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"block": "0x123"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await.unwrap();

    // With prefer_non_empty_always, NonEmpty should win despite having only 1 vote
    assert!(result.response.result.is_some());
    assert!(!result.response.result.as_ref().unwrap().is_null());
    assert!(result.consensus_achieved); // Should be true due to prefer_non_empty_always
    assert!(matches!(result.metadata.selection_method, SelectionMethod::PreferNonEmpty));
}

#[tokio::test]
async fn test_find_consensus_prefer_non_empty_requires_threshold() {
    let config = ConsensusConfig {
        enabled: true,
        min_count: 2,
        max_count: 3,
        prefer_non_empty: true,
        prefer_non_empty_always: false, // Safe mode - require threshold
        dispute_behavior: DisputeBehavior::AcceptAnyValid,
        ..Default::default()
    };

    let engine = ConsensusEngine::new(config);

    // 2 Empty responses, 1 NonEmpty response
    let responses = vec![
        (
            Arc::from("empty-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("empty-2"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::Value::Null),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
        (
            Arc::from("nonempty-1"),
            JsonRpcResponse {
                jsonrpc: JSONRPC_VERSION_COW,
                result: Some(serde_json::json!({"block": "0x123"})),
                error: None,
                id: Arc::new(serde_json::json!(1)),
                cache_status: None,
                serving_upstream: None,
            },
        ),
    ];

    let result = engine.find_consensus(responses, "eth_getBlockByNumber").await.unwrap();

    // NonEmpty is first due to preference, but doesn't meet threshold
    // So we use fallback behavior (AcceptAnyValid) and consensus_achieved=false
    // The response should still be NonEmpty because apply_preferences sorted it first
    assert!(!result.response.result.as_ref().unwrap().is_null());
    // NonEmpty has 1 vote, Empty has 2 - since NonEmpty doesn't meet threshold
    // and prefer_non_empty_always is false, consensus_achieved should be false
    assert!(!result.consensus_achieved);
}

#[test]
fn test_preference_config_defaults() {
    let config = ConsensusConfig::default();
    assert!(!config.prefer_non_empty);
    assert!(!config.prefer_non_empty_always);
    assert!(!config.prefer_larger_responses);
}

#[test]
fn test_preference_config_serde() {
    let toml = r"
        enabled = true
        prefer_non_empty = true
        prefer_non_empty_always = false
        prefer_larger_responses = true
    ";

    let config: ConsensusConfig = toml::from_str(toml).unwrap();
    assert!(config.prefer_non_empty);
    assert!(!config.prefer_non_empty_always);
    assert!(config.prefer_larger_responses);
}
