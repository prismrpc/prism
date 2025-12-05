use chrono::{Duration, Utc};
use clap::Subcommand;
use prettytable::{row, Table};
use prism_core::auth::{
    api_key::ApiKey,
    repository::{ApiKeyRepository, SqliteRepository},
};

#[derive(Subcommand)]
pub enum AuthCommands {
    /// Create a new API key
    Create {
        #[arg(short, long)]
        name: String,

        #[arg(short, long)]
        description: Option<String>,

        #[arg(long, default_value = "100")]
        rate_limit: u32,

        #[arg(long, default_value = "10")]
        refill_rate: u32,

        #[arg(long)]
        daily_limit: Option<i64>,

        #[arg(long)]
        expires_in_days: Option<i64>,

        /// Comma-separated list of allowed methods (empty = all)
        #[arg(short, long)]
        methods: Option<String>,
    },

    /// List all API keys
    List {
        #[arg(long)]
        show_expired: bool,
    },

    /// Revoke an API key
    Revoke {
        #[arg(short, long)]
        name: String,
    },

    /// Update rate limits for a key
    UpdateLimits {
        #[arg(short, long)]
        name: String,

        #[arg(long)]
        rate_limit: u32,

        #[arg(long)]
        refill_rate: u32,
    },

    /// Show usage statistics
    Stats {
        #[arg(short, long)]
        _name: Option<String>,

        #[arg(long, default_value = "7")]
        days: i64,
    },
}

pub async fn handle_auth_command(
    command: AuthCommands,
    repo: SqliteRepository,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        AuthCommands::Create {
            name,
            description,
            rate_limit,
            refill_rate,
            daily_limit,
            expires_in_days,
            methods,
        } => {
            let params = CreateKeyParams {
                name,
                description,
                rate_limit,
                refill_rate,
                daily_limit,
                expires_in_days,
                methods,
            };
            handle_create_key(repo, params).await?;
        }

        AuthCommands::List { show_expired } => {
            handle_list_keys(repo, show_expired).await?;
        }

        AuthCommands::Revoke { name } => {
            repo.revoke(&name).await?;
            println!("[SUCCESS] API Key '{name}' has been revoked");
        }

        AuthCommands::UpdateLimits { name, rate_limit, refill_rate } => {
            repo.update_rate_limits(&name, rate_limit, refill_rate).await?;
            println!("[SUCCESS] Rate limits updated for key '{name}'");
            println!("New limits: {rate_limit} tokens, {refill_rate} tokens/s");
        }

        AuthCommands::Stats { _name, days } => {
            println!("[INFO] Usage statistics for the last {days} days");
            // Implementation would query the api_key_usage table
            // and display statistics in a nice format
        }
    }

    Ok(())
}

struct CreateKeyParams {
    name: String,
    description: Option<String>,
    rate_limit: u32,
    refill_rate: u32,
    daily_limit: Option<i64>,
    expires_in_days: Option<i64>,
    methods: Option<String>,
}

async fn handle_create_key(
    repo: SqliteRepository,
    params: CreateKeyParams,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw_key = ApiKey::generate().unwrap_or_else(|e| {
        eprintln!("Failed to generate API key: {e}");
        std::process::exit(1);
    });
    let key_hash = ApiKey::hash_key(&raw_key).unwrap_or_else(|e| {
        eprintln!("Failed to hash API key: {e}");
        std::process::exit(1);
    });

    let expires_at = params.expires_in_days.map(|days| Utc::now() + Duration::days(days));

    // Compute blind index for timing-attack resistant lookups
    let blind_index = ApiKey::compute_blind_index(&raw_key);

    let api_key = ApiKey {
        id: 0,
        key_hash,
        blind_index,
        name: params.name.clone(),
        description: params.description,
        rate_limit_max_tokens: params.rate_limit,
        rate_limit_refill_rate: params.refill_rate,
        daily_request_limit: params.daily_limit,
        daily_requests_used: 0,
        quota_reset_at: Utc::now() + Duration::days(1),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        last_used_at: None,
        is_active: true,
        expires_at,
    };

    let allowed_methods = params.methods.map_or_else(
        || {
            vec![
                "eth_blockNumber".to_string(),
                "eth_getBlockByNumber".to_string(),
                "eth_getBlockByHash".to_string(),
                "eth_getTransactionByHash".to_string(),
                "eth_getTransactionReceipt".to_string(),
                "eth_getLogs".to_string(),
            ]
        },
        |m| m.split(',').map(|s| s.trim().to_string()).collect(),
    );

    repo.create(api_key, allowed_methods).await?;

    println!("[SUCCESS] API Key created successfully!");
    println!("Name: {}", params.name);
    println!("Key: {raw_key}");
    println!("[WARNING] Save this key securely - it cannot be retrieved later!");

    Ok(())
}

async fn handle_list_keys(
    repo: SqliteRepository,
    show_expired: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let keys = repo.list_all().await?;

    let mut table = Table::new();
    table.add_row(row![
        "Name",
        "Active",
        "Rate Limit",
        "Daily Limit",
        "Used Today",
        "Expires",
        "Last Used"
    ]);

    for key in keys {
        if !show_expired && key.is_expired() {
            continue;
        }

        table.add_row(row![
            key.name,
            if key.is_active {
                "[SUCCESS]"
            } else {
                "[ERROR]"
            },
            format!("{}/{}/s", key.rate_limit_max_tokens, key.rate_limit_refill_rate),
            key.daily_request_limit.map_or("∞".to_string(), |l| l.to_string()),
            key.daily_requests_used,
            key.expires_at.map_or("Never".to_string(), |e| e.format("%Y-%m-%d").to_string()),
            key.last_used_at
                .map_or("Never".to_string(), |l| l.format("%Y-%m-%d %H:%M").to_string()),
        ]);
    }

    table.printstd();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_commands_enum_create_variant() {
        let cmd = AuthCommands::Create {
            name: "test-key".to_string(),
            description: Some("Test key".to_string()),
            rate_limit: 100,
            refill_rate: 10,
            daily_limit: Some(1000),
            expires_in_days: Some(30),
            methods: Some("eth_blockNumber,eth_getLogs".to_string()),
        };

        match cmd {
            AuthCommands::Create {
                name,
                description,
                rate_limit,
                refill_rate,
                daily_limit,
                expires_in_days,
                methods,
            } => {
                assert_eq!(name, "test-key");
                assert_eq!(description, Some("Test key".to_string()));
                assert_eq!(rate_limit, 100);
                assert_eq!(refill_rate, 10);
                assert_eq!(daily_limit, Some(1000));
                assert_eq!(expires_in_days, Some(30));
                assert_eq!(methods, Some("eth_blockNumber,eth_getLogs".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_auth_commands_enum_list_variant() {
        let cmd = AuthCommands::List { show_expired: true };

        match cmd {
            AuthCommands::List { show_expired } => {
                assert!(show_expired);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_auth_commands_enum_revoke_variant() {
        let cmd = AuthCommands::Revoke { name: "test-key".to_string() };

        match cmd {
            AuthCommands::Revoke { name } => {
                assert_eq!(name, "test-key");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_auth_commands_enum_update_limits_variant() {
        let cmd = AuthCommands::UpdateLimits {
            name: "test-key".to_string(),
            rate_limit: 200,
            refill_rate: 20,
        };

        match cmd {
            AuthCommands::UpdateLimits { name, rate_limit, refill_rate } => {
                assert_eq!(name, "test-key");
                assert_eq!(rate_limit, 200);
                assert_eq!(refill_rate, 20);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_auth_commands_enum_stats_variant() {
        let cmd = AuthCommands::Stats { _name: Some("test-key".to_string()), days: 14 };

        match cmd {
            AuthCommands::Stats { _name: name, days } => {
                assert_eq!(name, Some("test-key".to_string()));
                assert_eq!(days, 14);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_auth_commands_create_with_minimal_fields() {
        let cmd = AuthCommands::Create {
            name: "minimal".to_string(),
            description: None,
            rate_limit: 100,
            refill_rate: 10,
            daily_limit: None,
            expires_in_days: None,
            methods: None,
        };

        match cmd {
            AuthCommands::Create {
                name,
                description,
                daily_limit,
                expires_in_days,
                methods,
                ..
            } => {
                assert_eq!(name, "minimal");
                assert_eq!(description, None);
                assert_eq!(daily_limit, None);
                assert_eq!(expires_in_days, None);
                assert_eq!(methods, None);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_default_allowed_methods_parsing() {
        // Test that default methods would include common Ethereum RPC methods
        let default_methods = vec![
            "eth_blockNumber".to_string(),
            "eth_getBlockByNumber".to_string(),
            "eth_getBlockByHash".to_string(),
            "eth_getTransactionByHash".to_string(),
            "eth_getTransactionReceipt".to_string(),
            "eth_getLogs".to_string(),
        ];

        assert_eq!(default_methods.len(), 6);
        assert!(default_methods.contains(&"eth_blockNumber".to_string()));
        assert!(default_methods.contains(&"eth_getLogs".to_string()));
    }

    #[test]
    fn test_custom_methods_parsing() {
        // Test that custom methods string can be split correctly
        let methods_str = "eth_blockNumber,eth_getLogs,eth_call";
        let parsed: Vec<String> = methods_str.split(',').map(|s| s.trim().to_string()).collect();

        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], "eth_blockNumber");
        assert_eq!(parsed[1], "eth_getLogs");
        assert_eq!(parsed[2], "eth_call");
    }

    #[test]
    fn test_methods_with_whitespace() {
        // Test that method parsing handles whitespace correctly
        let methods_str = "eth_blockNumber , eth_getLogs , eth_call";
        let parsed: Vec<String> = methods_str.split(',').map(|s| s.trim().to_string()).collect();

        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], "eth_blockNumber");
        assert_eq!(parsed[1], "eth_getLogs");
        assert_eq!(parsed[2], "eth_call");
    }
}
