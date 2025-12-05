//! Integration tests for consensus and data validation.
//!
//! These tests verify that consensus queries work correctly with actual mock upstreams:
//! - Quorum requirements are enforced
//! - Dispute resolution behaviors work as expected
//! - Timeout and failure handling is correct
//! - Byzantine/disagreeing upstreams are handled properly
//! - Penalty application works correctly

use prism_core::{
    chain::ChainState,
    types::{JsonRpcRequest, UpstreamConfig},
    upstream::{
        consensus::{
            ConsensusConfig, ConsensusEngine, DisputeBehavior, FailureBehavior,
            LowParticipantsBehavior,
        },
        endpoint::UpstreamEndpoint,
        errors::UpstreamError,
        http_client::HttpClient,
        scoring::{ScoringConfig, ScoringEngine},
    },
};
use std::sync::Arc;

fn create_upstream_config(name: &str, url: &str) -> UpstreamConfig {
    UpstreamConfig {
        name: Arc::from(name),
        url: url.to_string(),
        ws_url: None,
        chain_id: 1,
        weight: 1,
        timeout_seconds: 5,
        supports_websocket: false,
        circuit_breaker_threshold: 5,
        circuit_breaker_timeout_seconds: 60,
    }
}

fn create_test_request(method: &str) -> JsonRpcRequest {
    JsonRpcRequest::new(method, Some(serde_json::json!(["0x123", false])), serde_json::json!(1))
}

fn create_scoring_engine() -> Arc<ScoringEngine> {
    Arc::new(ScoringEngine::new(
        ScoringConfig { enabled: true, min_samples: 1, ..ScoringConfig::default() },
        Arc::new(ChainState::new()),
    ))
}

fn create_upstream_endpoint(name: &str, url: &str) -> Arc<UpstreamEndpoint> {
    let config = create_upstream_config(name, url);
    let http_client = Arc::new(HttpClient::new().unwrap());
    Arc::new(UpstreamEndpoint::new(config, http_client))
}

#[tokio::test]
async fn test_consensus_disabled_ignores_all_methods() {
    let engine = ConsensusEngine::new(ConsensusConfig::default());

    assert!(!engine.is_enabled().await);

    let sample_methods = ["eth_getBlockByNumber", "eth_call", "eth_getLogs", "any_method_at_all"];
    for method in sample_methods {
        assert!(
            !engine.requires_consensus(method).await,
            "When disabled, {method} should NOT require consensus",
        );
    }
}

#[tokio::test]
async fn test_consensus_custom_methods_replace_defaults() {
    let custom_methods = vec!["eth_call".to_string(), "eth_estimateGas".to_string()];
    let config = ConsensusConfig {
        enabled: true,
        methods: custom_methods.clone(),
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);

    for method in &custom_methods {
        assert!(
            engine.requires_consensus(method).await,
            "Configured method {method} should require consensus",
        );
    }

    let non_configured = ["eth_getBlockByNumber", "eth_blockNumber", "eth_chainId"];
    for method in non_configured {
        assert!(
            !engine.requires_consensus(method).await,
            "Non-configured method {method} should NOT require consensus",
        );
    }
}

#[tokio::test]
async fn test_consensus_method_matching_edge_cases() {
    let config = ConsensusConfig {
        enabled: true,
        methods: vec!["eth_call".to_string()],
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);

    assert!(engine.requires_consensus("eth_call").await);

    assert!(
        !engine.requires_consensus("ETH_CALL").await,
        "Method matching should be case-sensitive"
    );
    assert!(
        !engine.requires_consensus("Eth_Call").await,
        "Method matching should be case-sensitive"
    );

    assert!(
        !engine.requires_consensus("eth_callData").await,
        "Prefix match should not trigger consensus"
    );
    assert!(
        !engine.requires_consensus("pre_eth_call").await,
        "Suffix match should not trigger consensus"
    );

    assert!(!engine.requires_consensus("").await, "Empty method should not require consensus");
    assert!(
        !engine.requires_consensus(" eth_call").await,
        "Method with leading space should not match"
    );
}

#[tokio::test]
async fn test_consensus_config_update() {
    let engine = ConsensusEngine::new(ConsensusConfig::default());

    assert!(!engine.is_enabled().await);

    let new_config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };

    engine.update_config(new_config).await;

    assert!(engine.is_enabled().await);
}

#[tokio::test]
async fn test_consensus_default_configuration() {
    let config = ConsensusConfig::default();

    assert!(!config.enabled);
    assert_eq!(config.max_count, 3);
    assert_eq!(config.min_count, 2);
    assert_eq!(config.methods.len(), 5);
    assert!((config.disagreement_penalty - 10.0).abs() < f64::EPSILON);
    assert_eq!(config.timeout_seconds, 10);
    assert!(matches!(config.dispute_behavior, DisputeBehavior::PreferBlockHeadLeader));
    assert!(matches!(config.failure_behavior, FailureBehavior::AcceptAnyValid));
    assert!(matches!(config.low_participants_behavior, LowParticipantsBehavior::AcceptAvailable));
}

#[tokio::test]
async fn test_execute_consensus_no_upstreams_returns_error() {
    let config = ConsensusConfig { enabled: true, ..ConsensusConfig::default() };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, vec![], &scoring_engine).await;

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), UpstreamError::NoHealthyUpstreams));
}

#[tokio::test]
async fn test_execute_consensus_insufficient_upstreams_return_error_behavior() {
    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 3,
        low_participants_behavior: LowParticipantsBehavior::ReturnError,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    // Only provide 2 upstreams when min_count requires 3
    let upstreams = vec![
        create_upstream_endpoint("upstream-1", "http://localhost:9991"),
        create_upstream_endpoint("upstream-2", "http://localhost:9992"),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        UpstreamError::ConsensusFailure(msg) => {
            assert!(msg.contains("Insufficient upstreams"));
        }
        e => panic!("Expected ConsensusFailure, got {e:?}"),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_consensus_all_upstreams_agree() {
    let mut correct_server_1 = mockito::Server::new_async().await;
    let _mock1 = correct_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "number": "0x123456",
                    "hash": "0xCORRECT_MAJORITY",
                    "transactions": []
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut correct_server_2 = mockito::Server::new_async().await;
    let _mock2 = correct_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "number": "0x123456",
                    "hash": "0xCORRECT_MAJORITY",
                    "transactions": []
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut byzantine_server = mockito::Server::new_async().await;
    let _mock3 = byzantine_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "number": "0xBADBAD",
                    "hash": "0xBAD_BYZANTINE_DATA",
                    "transactions": ["0xfake"]
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("byzantine-first", &byzantine_server.url()),
        create_upstream_endpoint("correct-1", &correct_server_1.url()),
        create_upstream_endpoint("correct-2", &correct_server_2.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2, "Should have 2 agreeing upstreams");
    assert_eq!(consensus_result.total_queried, 3);

    assert_eq!(
        consensus_result.disagreeing_upstreams.len(),
        1,
        "Should identify 1 Byzantine upstream"
    );
    assert!(
        consensus_result
            .disagreeing_upstreams
            .iter()
            .any(|name| name.as_ref() == "byzantine-first"),
        "Should identify the byzantine-first upstream as disagreeing"
    );

    let selected_hash = consensus_result
        .response
        .result
        .as_ref()
        .and_then(|r| r.get("hash"))
        .and_then(|h| h.as_str());
    assert_eq!(
        selected_hash,
        Some("0xCORRECT_MAJORITY"),
        "Should select the MAJORITY response, not Byzantine data"
    );
}

#[tokio::test]
async fn test_consensus_byzantine_minority_disagreement() {
    let mut correct_server_1 = mockito::Server::new_async().await;
    let _mock1 = correct_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xcorrect"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut correct_server_2 = mockito::Server::new_async().await;
    let _mock2 = correct_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100", "hash": "0xcorrect"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut byzantine_server = mockito::Server::new_async().await;
    let _mock3 = byzantine_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x999", "hash": "0xbyzantine"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("correct-1", &correct_server_1.url()),
        create_upstream_endpoint("correct-2", &correct_server_2.url()),
        create_upstream_endpoint("byzantine", &byzantine_server.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2);
    assert_eq!(consensus_result.total_queried, 3);
    assert_eq!(consensus_result.disagreeing_upstreams.len(), 1);

    let block_hash = consensus_result
        .response
        .result
        .as_ref()
        .and_then(|r| r.get("hash"))
        .and_then(|h| h.as_str());
    assert_eq!(block_hash, Some("0xcorrect"));
}

#[tokio::test]
async fn test_consensus_all_disagree_return_error() {
    let mut server1 = mockito::Server::new_async().await;
    let _mock1 = server1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x111"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut server2 = mockito::Server::new_async().await;
    let _mock2 = server2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x222"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut server3 = mockito::Server::new_async().await;
    let _mock3 = server3
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x333"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        dispute_behavior: DisputeBehavior::ReturnError,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("upstream-1", &server1.url()),
        create_upstream_endpoint("upstream-2", &server2.url()),
        create_upstream_endpoint("upstream-3", &server3.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        UpstreamError::ConsensusFailure(msg) => {
            assert!(msg.contains("Consensus not reached") || msg.contains("best group"));
        }
        e => panic!("Expected ConsensusFailure, got {e:?}"),
    }
}

#[tokio::test]
async fn test_consensus_all_disagree_accept_any_valid() {
    let mut server1 = mockito::Server::new_async().await;
    let _mock1 = server1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x111"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut server2 = mockito::Server::new_async().await;
    let _mock2 = server2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x222"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 2,
        min_count: 2,
        dispute_behavior: DisputeBehavior::AcceptAnyValid,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("upstream-1", &server1.url()),
        create_upstream_endpoint("upstream-2", &server2.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();
    assert!(!consensus_result.consensus_achieved);
    assert!(consensus_result.response.result.is_some());
}

#[tokio::test]
async fn test_consensus_partial_timeout() {
    let mut fast_server_1 = mockito::Server::new_async().await;
    let _mock1 = fast_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut fast_server_2 = mockito::Server::new_async().await;
    let _mock2 = fast_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut slow_server = mockito::Server::new_async().await;
    let _mock_slow = slow_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0xslow"}
            })
            .to_string(),
        )
        .with_chunked_body(|w| {
            std::thread::sleep(std::time::Duration::from_secs(10));
            w.write_all(b"delayed")
        })
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        timeout_seconds: 2, // Short timeout
        low_participants_behavior: LowParticipantsBehavior::AcceptAvailable,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("fast-1", &fast_server_1.url()),
        create_upstream_endpoint("fast-2", &fast_server_2.url()),
        create_upstream_endpoint("slow", &slow_server.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));

    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    match result {
        Ok(consensus_result) => {
            assert!(consensus_result.agreement_count >= 2);
        }
        Err(UpstreamError::Timeout) => {}
        Err(e) => panic!("Unexpected error: {e:?}"),
    }
}

#[tokio::test]
async fn test_consensus_with_http_errors() {
    let mut good_server = mockito::Server::new_async().await;
    let _mock1 = good_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x100"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut error_server = mockito::Server::new_async().await;
    let _mock2 = error_server
        .mock("POST", "/")
        .with_status(500)
        .with_body("Internal Server Error")
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 2,
        min_count: 1,
        failure_behavior: FailureBehavior::AcceptAnyValid,
        low_participants_behavior: LowParticipantsBehavior::AcceptAvailable,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("good", &good_server.url()),
        create_upstream_endpoint("error", &error_server.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();
    assert!(consensus_result.response.result.is_some());
}

#[tokio::test]
async fn test_consensus_all_upstreams_fail() {
    let mut error_server_1 = mockito::Server::new_async().await;
    let _mock1 = error_server_1.mock("POST", "/").with_status(503).create_async().await;

    let mut error_server_2 = mockito::Server::new_async().await;
    let _mock2 = error_server_2.mock("POST", "/").with_status(503).create_async().await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 2,
        min_count: 2,
        failure_behavior: FailureBehavior::ReturnError,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("error-1", &error_server_1.url()),
        create_upstream_endpoint("error-2", &error_server_2.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        UpstreamError::ConsensusFailure(_) | UpstreamError::NoHealthyUpstreams => {}
        e => panic!("Expected ConsensusFailure or NoHealthyUpstreams, got {e:?}"),
    }
}

#[tokio::test]
async fn test_consensus_with_rpc_errors_treated_as_failures() {
    let mut server1 = mockito::Server::new_async().await;
    let _mock1 = server1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32000,
                    "message": "execution reverted"
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut server2 = mockito::Server::new_async().await;
    let _mock2 = server2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32000,
                    "message": "execution reverted"
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 2,
        min_count: 2,
        failure_behavior: FailureBehavior::ReturnError,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("upstream-1", &server1.url()),
        create_upstream_endpoint("upstream-2", &server2.url()),
    ];

    let request = Arc::new(create_test_request("eth_call"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        UpstreamError::ConsensusFailure(_) | UpstreamError::NoHealthyUpstreams => {}
        e => panic!("Expected ConsensusFailure or NoHealthyUpstreams, got {e:?}"),
    }
}

#[tokio::test]
async fn test_consensus_with_mixed_success_and_rpc_errors() {
    let mut success_server_1 = mockito::Server::new_async().await;
    let _mock1 = success_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x1234"
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut success_server_2 = mockito::Server::new_async().await;
    let _mock2 = success_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": "0x1234"
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut error_server = mockito::Server::new_async().await;
    let _mock3 = error_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32000,
                    "message": "execution reverted"
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        failure_behavior: FailureBehavior::AcceptAnyValid,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("success-1", &success_server_1.url()),
        create_upstream_endpoint("success-2", &success_server_2.url()),
        create_upstream_endpoint("error", &error_server.url()),
    ];

    let request = Arc::new(create_test_request("eth_call"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();
    assert!(consensus_result.consensus_achieved);
    assert_eq!(consensus_result.agreement_count, 2);
    assert!(consensus_result.response.result.is_some());
}

#[tokio::test]
async fn test_consensus_penalty_application() {
    let mut correct_server = mockito::Server::new_async().await;
    let _mock1 = correct_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"correct": true}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut correct_server_2 = mockito::Server::new_async().await;
    let _mock2 = correct_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"correct": true}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut byzantine_server = mockito::Server::new_async().await;
    let _mock3 = byzantine_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"correct": false, "byzantine": true}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        disagreement_penalty: 15.0,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    for name in &["correct-1", "correct-2", "byzantine"] {
        for _ in 0..10 {
            scoring_engine.record_success(name, 100);
        }
    }

    let upstreams = vec![
        create_upstream_endpoint("correct-1", &correct_server.url()),
        create_upstream_endpoint("correct-2", &correct_server_2.url()),
        create_upstream_endpoint("byzantine", &byzantine_server.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();

    assert!(consensus_result.disagreeing_upstreams.iter().any(|n| n.as_ref() == "byzantine"));

    let metrics = scoring_engine.get_metrics("byzantine");
    assert!(metrics.is_some());
    let (error_rate, _, _, _, _) = metrics.unwrap();
    assert!(error_rate > 0.0, "Byzantine upstream should have increased error rate");
}

#[tokio::test]
async fn test_consensus_strict_quorum_all_must_agree() {
    let mut server1 = mockito::Server::new_async().await;
    let _mock1 = server1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"data": "same"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut server2 = mockito::Server::new_async().await;
    let _mock2 = server2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"data": "same"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut server3 = mockito::Server::new_async().await;
    let _mock3 = server3
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"data": "different"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 3,
        dispute_behavior: DisputeBehavior::ReturnError,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("upstream-1", &server1.url()),
        create_upstream_endpoint("upstream-2", &server2.url()),
        create_upstream_endpoint("upstream-3", &server3.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_low_participants_block_head_leader() {
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"number": "0x123"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 3,
        low_participants_behavior: LowParticipantsBehavior::OnlyBlockHeadLeader,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    scoring_engine.record_block_number("leader", 1000).await;
    for _ in 0..5 {
        scoring_engine.record_success("leader", 100);
    }

    let upstreams = vec![create_upstream_endpoint("leader", &server.url())];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();
    assert!(!consensus_result.consensus_achieved);
    assert_eq!(consensus_result.total_queried, 1);
}

#[tokio::test]
async fn test_consensus_prefer_non_empty() {
    let mut data_server = mockito::Server::new_async().await;
    let _mock1 = data_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"blockNumber": "0x123", "data": "actual_data"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut null_server_1 = mockito::Server::new_async().await;
    let _mock2 = null_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": null
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut null_server_2 = mockito::Server::new_async().await;
    let _mock3 = null_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": null
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 1,
        prefer_non_empty: true,
        prefer_non_empty_always: true,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("data", &data_server.url()),
        create_upstream_endpoint("null-1", &null_server_1.url()),
        create_upstream_endpoint("null-2", &null_server_2.url()),
    ];

    let request = Arc::new(create_test_request("eth_getTransactionByHash"));
    let result = engine.execute_consensus(request, upstreams, &scoring_engine).await;

    assert!(result.is_ok());
    let consensus_result = result.unwrap();

    let has_data = consensus_result
        .response
        .result
        .as_ref()
        .is_some_and(|r| r.get("data").is_some());
    assert!(has_data, "Should have selected the non-empty response");
}

#[tokio::test]
async fn test_misbehavior_tracking_dispute_threshold() {
    let mut correct_server_1 = mockito::Server::new_async().await;
    let _mock1 = correct_server_1
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"value": "correct"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut correct_server_2 = mockito::Server::new_async().await;
    let _mock2 = correct_server_2
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"value": "correct"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut misbehaving_server = mockito::Server::new_async().await;
    let _mock3 = misbehaving_server
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"value": "wrong"}
            })
            .to_string(),
        )
        .create_async()
        .await;

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        dispute_threshold: Some(2),
        dispute_window_seconds: 60,
        sit_out_penalty_seconds: 30,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(config);
    let scoring_engine = create_scoring_engine();

    let upstreams = vec![
        create_upstream_endpoint("correct-1", &correct_server_1.url()),
        create_upstream_endpoint("correct-2", &correct_server_2.url()),
        create_upstream_endpoint("misbehaving", &misbehaving_server.url()),
    ];

    let request = Arc::new(create_test_request("eth_getBlockByNumber"));

    for _ in 0..2 {
        let result = engine
            .execute_consensus(Arc::clone(&request), upstreams.clone(), &scoring_engine)
            .await;
        assert!(result.is_ok());
        let consensus_result = result.unwrap();
        assert!(consensus_result
            .disagreeing_upstreams
            .iter()
            .any(|n| n.as_ref() == "misbehaving"));
    }
}

#[tokio::test]
async fn test_concurrent_consensus_queries() {
    let mut servers = Vec::new();
    for i in 0..3 {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"number": "0x100"}
                })
                .to_string(),
            )
            .create_async()
            .await;
        servers.push((i, server));
    }

    let config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 2,
        max_concurrent_queries: 10,
        timeout_seconds: 5,
        ..ConsensusConfig::default()
    };

    let engine = Arc::new(ConsensusEngine::new(config));
    let scoring_engine = create_scoring_engine();

    let upstreams: Vec<_> = servers
        .iter()
        .map(|(i, server)| create_upstream_endpoint(&format!("upstream-{i}"), &server.url()))
        .collect();

    // Spawn multiple concurrent consensus queries
    let mut handles = Vec::new();
    for _ in 0..5 {
        let engine_clone = Arc::clone(&engine);
        let scoring_clone = Arc::clone(&scoring_engine);
        let upstreams_clone = upstreams.clone();

        let handle = tokio::spawn(async move {
            let request = Arc::new(create_test_request("eth_getBlockByNumber"));
            engine_clone.execute_consensus(request, upstreams_clone, &scoring_clone).await
        });
        handles.push(handle);
    }

    // All queries should succeed
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
        assert!(result.unwrap().consensus_achieved);
    }
}

/// Test runtime config updates affect subsequent queries
#[tokio::test]
async fn test_runtime_config_update_affects_queries() {
    let engine = ConsensusEngine::new(ConsensusConfig {
        enabled: true,
        methods: vec!["eth_call".to_string()],
        ..ConsensusConfig::default()
    });

    // Initially, only eth_call requires consensus
    assert!(engine.requires_consensus("eth_call").await);
    assert!(!engine.requires_consensus("eth_getBlockByNumber").await);

    // Update config
    engine
        .update_config(ConsensusConfig {
            enabled: true,
            methods: vec!["eth_getBlockByNumber".to_string(), "eth_getLogs".to_string()],
            ..ConsensusConfig::default()
        })
        .await;

    // Now eth_call doesn't require consensus, but eth_getBlockByNumber does
    assert!(!engine.requires_consensus("eth_call").await);
    assert!(engine.requires_consensus("eth_getBlockByNumber").await);
    assert!(engine.requires_consensus("eth_getLogs").await);
}

// Quorum Configuration Tests

#[tokio::test]
async fn test_consensus_quorum_requirements_for_partial_responses() {
    // Scenario 1: Allow partial consensus (2 of 3)
    let partial_config = ConsensusConfig {
        enabled: true,
        max_count: 3, // Query 3 upstreams
        min_count: 2, // But only need 2 to agree
        ..ConsensusConfig::default()
    };

    let engine = ConsensusEngine::new(partial_config);
    let config = engine.get_config().await;

    assert!(config.max_count > config.min_count, "Should allow partial consensus (2/3)");
    assert_eq!(config.max_count - config.min_count, 1, "Can tolerate 1 timeout");

    // Scenario 2: Require all responses (no tolerance for timeouts)
    let strict_config = ConsensusConfig {
        enabled: true,
        max_count: 3,
        min_count: 3, // Need all 3 to respond
        ..ConsensusConfig::default()
    };

    let strict_engine = ConsensusEngine::new(strict_config);
    let strict_retrieved = strict_engine.get_config().await;

    assert_eq!(
        strict_retrieved.max_count, strict_retrieved.min_count,
        "Strict config requires all upstreams to respond"
    );

    // Scenario 3: High tolerance (only need 1 of 5)
    let high_tolerance_config = ConsensusConfig {
        enabled: true,
        max_count: 5,
        min_count: 1, // Very lenient - any single response is enough
        low_participants_behavior: LowParticipantsBehavior::AcceptAvailable,
        ..ConsensusConfig::default()
    };

    let high_tolerance_engine = ConsensusEngine::new(high_tolerance_config);
    let ht_config = high_tolerance_engine.get_config().await;

    assert_eq!(ht_config.max_count - ht_config.min_count, 4, "Can tolerate up to 4 timeouts");
}

#[tokio::test]
async fn test_consensus_dispute_behaviors() {
    // Test all dispute behavior variants
    let behaviors = vec![
        DisputeBehavior::PreferBlockHeadLeader,
        DisputeBehavior::ReturnError,
        DisputeBehavior::AcceptAnyValid,
        DisputeBehavior::PreferHighestScore,
    ];

    for behavior in behaviors {
        let config = ConsensusConfig {
            enabled: true,
            dispute_behavior: behavior,
            ..ConsensusConfig::default()
        };

        let engine = ConsensusEngine::new(config);
        assert!(engine.is_enabled().await);
    }
}

#[tokio::test]
async fn test_consensus_failure_behaviors() {
    let behaviors = vec![
        FailureBehavior::ReturnError,
        FailureBehavior::AcceptAnyValid,
        FailureBehavior::UseHighestScore,
    ];

    for behavior in behaviors {
        let config = ConsensusConfig {
            enabled: true,
            failure_behavior: behavior,
            ..ConsensusConfig::default()
        };

        let engine = ConsensusEngine::new(config);
        assert!(engine.is_enabled().await);
    }
}

#[tokio::test]
async fn test_consensus_low_participants_behaviors() {
    let behaviors = vec![
        LowParticipantsBehavior::OnlyBlockHeadLeader,
        LowParticipantsBehavior::ReturnError,
        LowParticipantsBehavior::AcceptAvailable,
    ];

    for behavior in behaviors {
        let config = ConsensusConfig {
            enabled: true,
            low_participants_behavior: behavior,
            ..ConsensusConfig::default()
        };

        let engine = ConsensusEngine::new(config);
        assert!(engine.is_enabled().await);
    }
}
