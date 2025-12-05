use clap::{Parser, Subcommand};
use prism_core::auth::repository::SqliteRepository;

mod commands;
use commands::{
    fetch_endpoints, handle_auth_command, handle_config_command, AuthCommands, ConfigCommands,
    FetchOptions, ProtocolFilter, RunMode, SpeedTest,
};

#[derive(Parser)]
#[command(name = "prism-cli")]
#[command(about = "Prism CLI - Comprehensive management tool for the Prism RPC Aggregator")]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(long)]
    database: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// API Key Management
    #[command(subcommand)]
    Auth(AuthCommands),

    /// Configuration Management
    #[command(subcommand)]
    Config(ConfigCommands),

    /// Fetch RPC endpoints from Chainlist
    FetchEndpoints {
        /// Output file path
        #[arg(short, long, default_value = "config/config.toml")]
        output: String,

        /// Filter by chain ID (can be specified multiple times)
        #[arg(short, long)]
        chain_id: Vec<u64>,

        /// Filter by chain name (case insensitive, less precise than chain-id)
        #[arg(long)]
        chain_name: Option<String>,

        /// Only include HTTPS endpoints
        #[arg(long)]
        https_only: bool,

        /// Only include WSS endpoints
        #[arg(long)]
        wss_only: bool,

        /// Filter by tracking level (none, limited, yes)
        #[arg(long)]
        tracking: Option<String>,

        /// Show output without writing to file
        #[arg(long)]
        dry_run: bool,

        /// Test response times and order by speed
        #[arg(long)]
        test_speed: bool,

        /// Limit number of endpoints per chain (0 = no limit)
        #[arg(short, long, default_value = "0")]
        limit: usize,

        /// Timeout for speed tests in seconds
        #[arg(long, default_value = "10")]
        timeout: u64,

        /// Number of concurrent speed tests
        #[arg(long, default_value = "50")]
        concurrency: usize,

        /// List available chains and their IDs
        #[arg(long)]
        list_chains: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Auth(auth_command) => {
            let database_url = cli
                .database
                .or_else(|| std::env::var("DATABASE_URL").ok())
                .unwrap_or_else(|| "sqlite://db/auth.db".to_string());

            let repo = SqliteRepository::new(&database_url).await?;
            handle_auth_command(auth_command, repo).await?;
        }

        Commands::Config(config_command) => {
            handle_config_command(config_command).await?;
        }

        Commands::FetchEndpoints {
            output,
            chain_id,
            chain_name,
            https_only,
            wss_only,
            tracking,
            dry_run,
            test_speed,
            limit,
            timeout,
            concurrency,
            list_chains,
        } => {
            let protocol_filter = if https_only {
                ProtocolFilter::HttpsOnly
            } else if wss_only {
                ProtocolFilter::WssOnly
            } else {
                ProtocolFilter::All
            };

            let run_mode = if dry_run {
                RunMode::DryRun
            } else {
                RunMode::Normal
            };
            let speed_test = if test_speed {
                SpeedTest::Enabled
            } else {
                SpeedTest::Disabled
            };

            let options = FetchOptions {
                output_file: output,
                chain_ids: chain_id,
                chain_name_filter: chain_name,
                protocol_filter,
                tracking_filter: tracking,
                run_mode,
                speed_test,
                limit,
                timeout_secs: timeout,
                concurrency,
                list_chains,
            };

            fetch_endpoints(options).await?;
        }
    }

    Ok(())
}
