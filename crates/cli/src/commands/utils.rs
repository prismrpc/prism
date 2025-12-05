use std::fmt;

#[derive(Debug)]
pub enum CliError {
    Config(String),
    Io(String),
    Network(String),
    /// Reserved for future authentication-related errors
    #[allow(dead_code)]
    Auth(String),
    General(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(msg) => write!(f, "Configuration error: {msg}"),
            Self::Io(msg) => write!(f, "IO error: {msg}"),
            Self::Network(msg) => write!(f, "Network error: {msg}"),
            Self::Auth(msg) => write!(f, "Authentication error: {msg}"),
            Self::General(msg) => write!(f, "Error: {msg}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<reqwest::Error> for CliError {
    fn from(error: reqwest::Error) -> Self {
        Self::Network(error.to_string())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::General(error.to_string())
    }
}

pub type CliResult<T> = Result<T, CliError>;

pub fn print_success(message: &str) {
    println!("[SUCCESS] {message}");
}

pub fn print_error(message: &str) {
    eprintln!("[ERROR] {message}");
}

pub fn print_info(message: &str) {
    println!("[INFO] {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_error_display_config() {
        let error = CliError::Config("invalid config".to_string());
        assert_eq!(error.to_string(), "Configuration error: invalid config");
    }

    #[test]
    fn test_cli_error_display_io() {
        let error = CliError::Io("file not found".to_string());
        assert_eq!(error.to_string(), "IO error: file not found");
    }

    #[test]
    fn test_cli_error_display_network() {
        let error = CliError::Network("connection refused".to_string());
        assert_eq!(error.to_string(), "Network error: connection refused");
    }

    #[test]
    fn test_cli_error_display_auth() {
        let error = CliError::Auth("invalid credentials".to_string());
        assert_eq!(error.to_string(), "Authentication error: invalid credentials");
    }

    #[test]
    fn test_cli_error_display_general() {
        let error = CliError::General("something went wrong".to_string());
        assert_eq!(error.to_string(), "Error: something went wrong");
    }

    #[test]
    fn test_cli_error_from_io_error() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let cli_error: CliError = io_error.into();

        match cli_error {
            CliError::Io(msg) => assert!(msg.contains("file not found")),
            _ => panic!("Expected Io variant"),
        }
    }

    #[test]
    fn test_cli_error_from_serde_json_error() {
        let json_str = "{ invalid json }";
        let serde_error = serde_json::from_str::<serde_json::Value>(json_str).unwrap_err();
        let cli_error: CliError = serde_error.into();

        match cli_error {
            CliError::General(msg) => assert!(!msg.is_empty()),
            _ => panic!("Expected General variant"),
        }
    }

    #[test]
    fn test_cli_error_debug_format() {
        let error = CliError::Config("test".to_string());
        let debug_str = format!("{error:?}");
        assert!(debug_str.contains("Config"));
        assert!(debug_str.contains("test"));
    }

    #[test]
    fn test_cli_error_implements_error_trait() {
        let error = CliError::General("test".to_string());
        let _: &dyn std::error::Error = &error;
    }

    #[test]
    fn test_cli_result_ok() {
        let result: CliResult<i32> = Ok(42);
        assert!(result.is_ok());
        if let Ok(val) = result {
            assert_eq!(val, 42);
        }
    }

    #[test]
    fn test_cli_result_err() {
        let result: CliResult<i32> = Err(CliError::General("test".to_string()));
        assert!(result.is_err());
    }
}
