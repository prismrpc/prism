pub mod auth;
pub mod config;
pub mod fetch_endpoints;
pub mod utils;

pub use auth::{handle_auth_command, AuthCommands};
pub use config::{handle_config_command, ConfigCommands};
pub use fetch_endpoints::{fetch_endpoints, FetchOptions, ProtocolFilter, RunMode, SpeedTest};
