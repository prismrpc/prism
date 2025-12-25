//! # Prism Core
//!
//! Core library for the Prism high-performance Ethereum RPC aggregator.
//!
//! This crate provides the foundational components for:
//!
//! - **[`cache`]**: Advanced caching system with block, transaction, and log caching, including
//!   partial-range fulfillment for `eth_getLogs` and reorg-aware invalidation.
//!
//! - **[`proxy`]**: Request routing and processing engine with support for hedging, consensus
//!   validation, and intelligent upstream selection.
//!
//! - **[`upstream`]**: Multi-provider management with health checking, circuit breakers,
//!   scoring-based selection, and WebSocket chain tip subscription.
//!
//! - **[`auth`]**: API key authentication with `SQLite` backend and per-key rate limiting.
//!
//! - **[`metrics`]**: Prometheus metrics collection for monitoring and observability.
//!
//! - **[`middleware`]**: Request validation, rate limiting, and method filtering.
//!
//! - **[`chain`]**: Shared chain state management for tip and finality tracking.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                         ProxyEngine                         │
//! │  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────┐  │
//! │  │   CacheManager  │  │ UpstreamManager │  │   Metrics   │  │
//! │  └────────┬────────┘  └────────┬────────┘  └──────┬──────┘  │
//! │           │                    │                  │         │
//! │  ┌────────▼────────┐  ┌────────▼────────┐  ┌──────▼──────┐  │
//! │  │   LogCache      │  │  ConsensusEngine│  │ Prometheus  │  │
//! │  │   BlockCache    │  │  HedgeExecutor  │  │  Exporter   │  │
//! │  │   TxCache       │  │  ScoringEngine  │  └─────────────┘  │
//! │  └─────────────────┘  └─────────────────┘                   │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Request Flow
//!
//! ```text
//! Client Request
//!       │
//!       ▼
//! ┌─────────────┐
//! │  Validation │ ─── Invalid ──► Error Response
//! └──────┬──────┘
//!        │ Valid
//!        ▼
//! ┌─────────────┐
//! │ ProxyEngine │
//! │  (dispatch) │
//! └──────┬──────┘
//!        │
//!        ▼
//! ┌─────────────┐
//! │ Cache Check │ ─── Hit ──► Cached Response
//! └──────┬──────┘
//!        │ Miss
//!        ▼
//! ┌──────────────────┐
//! │  UpstreamManager │
//! │  (SmartRouter)   │
//! └────────┬─────────┘
//!          │
//!    ┌─────┴─────┐
//!    ▼           ▼
//! Consensus?  Scoring?  ──► Hedging? ──► LoadBalancer
//!    │           │              │              │
//!    ▼           ▼              ▼              ▼
//! Quorum     Top-N         Parallel       Response-time
//! Validation Selection     Racing         Selection
//!    │           │              │              │
//!    └─────┬─────┴──────────────┴──────────────┘
//!          │
//!          ▼
//! ┌─────────────────┐
//! │ Upstream HTTP   │
//! │   Request(s)    │
//! └────────┬────────┘
//!          │
//!          ▼
//! ┌─────────────────┐
//! │   Validation    │ ─── Invalid ──► Skip Cache
//! │ (cache/validation)
//! └────────┬────────┘
//!          │ Valid
//!          ▼
//! ┌─────────────────┐
//! │  Cache Insert   │
//! └────────┬────────┘
//!          │
//!          ▼
//!   Response to Client
//! ```
//!
//! ## Feature Flags
//!
//! - `verbose-logging`: Enhanced logging output for debugging
//! - `enable-stats`: Statistics collection for performance analysis
//! - `benchmarks`: Benchmark utilities (enabled in bench builds)

#![feature(stmt_expr_attributes)]

pub mod alerts;
pub mod auth;
pub mod cache;
pub mod chain;
pub mod config;
pub mod metrics;
pub mod middleware;
pub mod proxy;
pub mod runtime;
pub mod types;
pub mod upstream;
pub mod utils;
