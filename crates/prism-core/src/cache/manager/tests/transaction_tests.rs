//! Tests for transaction operations.

use super::*;
use crate::cache::types::{ReceiptRecord, TransactionRecord};

#[test]
fn test_get_transaction_not_found() {
    let cache = create_test_cache_manager();

    let hash = [0u8; 32];
    let result = cache.get_transaction(&hash);
    assert!(result.is_none());
}

#[test]
fn test_get_receipt_not_found() {
    let cache = create_test_cache_manager();

    let hash = [0u8; 32];
    let result = cache.get_receipt(&hash);
    assert!(result.is_none());
}

#[test]
fn test_get_receipt_with_logs_not_found() {
    let cache = create_test_cache_manager();

    let hash = [0u8; 32];
    let result = cache.get_receipt_with_logs(&hash);
    assert!(result.is_none());
}

#[tokio::test]
async fn test_warm_transactions() {
    let cache = create_test_cache_manager();

    let transactions = vec![TransactionRecord {
        hash: [0xAA; 32],
        block_hash: Some([0x10; 32]),
        block_number: Some(100),
        transaction_index: Some(0),
        from: [0; 20],
        to: Some([1; 20]),
        value: [0; 32],
        tx_type: Some(0), // Legacy transaction
        gas_price: Some([0; 32]),
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        max_fee_per_blob_gas: None,
        gas_limit: 21000,
        nonce: 0,
        data: vec![],
        v: 27,
        r: [0; 32],
        s: [0; 32],
    }];

    let receipts = vec![ReceiptRecord {
        transaction_hash: [0xAA; 32],
        block_hash: [0x10; 32],
        block_number: 100,
        transaction_index: 0,
        from: [0; 20],
        to: Some([1; 20]),
        cumulative_gas_used: 21000,
        gas_used: 21000,
        contract_address: None,
        logs: vec![],
        status: 1,
        logs_bloom: vec![],
        effective_gas_price: Some(1_000_000_000),
        tx_type: Some(2),
        blob_gas_price: None,
    }];

    cache.warm_transactions(100, 100, transactions, receipts).await;

    assert!(cache.get_transaction(&[0xAA; 32]).is_some());
    assert!(cache.get_receipt(&[0xAA; 32]).is_some());
}

#[tokio::test]
async fn test_insert_receipt() {
    let cache = create_test_cache_manager();

    let receipt = ReceiptRecord {
        transaction_hash: [0xBB; 32],
        block_hash: [0x17; 32],
        block_number: 1700,
        transaction_index: 0,
        from: [0; 20],
        to: Some([1; 20]),
        cumulative_gas_used: 21000,
        gas_used: 21000,
        contract_address: None,
        logs: vec![],
        status: 1,
        logs_bloom: vec![],
        effective_gas_price: Some(1_000_000_000),
        tx_type: Some(2),
        blob_gas_price: None,
    };

    cache.insert_receipt(receipt).await;
    assert!(cache.get_receipt(&[0xBB; 32]).is_some());
}

#[tokio::test]
async fn test_insert_receipts_bulk_no_stats() {
    let cache = create_test_cache_manager();

    let receipts = vec![ReceiptRecord {
        transaction_hash: [0xCC; 32],
        block_hash: [0x18; 32],
        block_number: 1800,
        transaction_index: 0,
        from: [0; 20],
        to: Some([1; 20]),
        cumulative_gas_used: 21000,
        gas_used: 21000,
        contract_address: None,
        logs: vec![],
        status: 1,
        logs_bloom: vec![],
        effective_gas_price: Some(1_000_000_000),
        tx_type: Some(2),
        blob_gas_price: None,
    }];

    cache.insert_receipts_bulk_no_stats(receipts).await;
}
