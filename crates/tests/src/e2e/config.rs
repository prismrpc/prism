//! E2E Test Configuration
//!
//! Configuration constants and types for end-to-end tests.

/// Configuration for E2E tests
#[derive(Debug, Clone)]
pub struct E2eConfig {
    /// Prism proxy URL
    pub proxy_url: String,
    /// Sealer node URL (block producer)
    pub geth_sealer_url: String,
    /// RPC node 1 URL
    pub geth_rpc_1_url: String,
    /// RPC node 2 URL
    pub geth_rpc_2_url: String,
    /// RPC node 3 URL
    pub geth_rpc_3_url: String,
    /// Chain ID for the devnet
    pub chain_id: u64,
    /// Test timeout in seconds
    pub timeout_seconds: u64,
    /// Pre-funded test account address (from deterministic mnemonic)
    pub test_account: String,
    /// Pre-funded test account private key
    pub test_private_key: String,
}

impl Default for E2eConfig {
    fn default() -> Self {
        Self {
            proxy_url: std::env::var("PRISM_PROXY_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3030".to_string()),
            geth_sealer_url: std::env::var("GETH_SEALER_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8545".to_string()),
            geth_rpc_1_url: std::env::var("GETH_RPC_1_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8547".to_string()),
            geth_rpc_2_url: std::env::var("GETH_RPC_2_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8549".to_string()),
            geth_rpc_3_url: std::env::var("GETH_RPC_3_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8551".to_string()),
            chain_id: 1337,
            timeout_seconds: 30,
            // First account from "test test test test test test test test test test test junk"
            test_account: "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266".to_string(),
            test_private_key: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .to_string(),
        }
    }
}

impl E2eConfig {
    /// Create config from environment variables
    #[must_use]
    pub fn from_env() -> Self {
        Self::default()
    }

    /// Get all Geth node URLs as a vector
    #[must_use]
    pub fn all_node_urls(&self) -> Vec<&str> {
        vec![
            &self.geth_sealer_url,
            &self.geth_rpc_1_url,
            &self.geth_rpc_2_url,
            &self.geth_rpc_3_url,
        ]
    }

    /// Get RPC node URLs (excludes sealer for failover tests)
    #[must_use]
    pub fn rpc_node_urls(&self) -> Vec<&str> {
        vec![&self.geth_rpc_1_url, &self.geth_rpc_2_url, &self.geth_rpc_3_url]
    }
}

/// Docker container names for devnet services
pub mod containers {
    pub const SEALER: &str = "prism-geth-sealer";
    pub const RPC_1: &str = "prism-geth-rpc-1";
    pub const RPC_2: &str = "prism-geth-rpc-2";
    pub const RPC_3: &str = "prism-geth-rpc-3";
}

/// Common RPC method names
pub mod methods {
    pub const ETH_BLOCK_NUMBER: &str = "eth_blockNumber";
    pub const ETH_CHAIN_ID: &str = "eth_chainId";
    pub const ETH_GET_BALANCE: &str = "eth_getBalance";
    pub const ETH_GET_BLOCK_BY_NUMBER: &str = "eth_getBlockByNumber";
    pub const ETH_GET_BLOCK_BY_HASH: &str = "eth_getBlockByHash";
    pub const ETH_GET_TRANSACTION_BY_HASH: &str = "eth_getTransactionByHash";
    pub const ETH_GET_TRANSACTION_RECEIPT: &str = "eth_getTransactionReceipt";
    pub const ETH_GET_LOGS: &str = "eth_getLogs";
    pub const ETH_GAS_PRICE: &str = "eth_gasPrice";
}
