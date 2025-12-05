# Contributing to Prism

Thank you for your interest in contributing to Prism! This document provides guidelines and instructions for contributing to the project.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Development Workflow](#development-workflow)
- [Code Style and Formatting](#code-style-and-formatting)
- [Testing Guidelines](#testing-guidelines)
- [Pull Request Process](#pull-request-process)
- [Commit Message Guidelines](#commit-message-guidelines)
- [Documentation](#documentation)
- [Project-Specific Guidelines](#project-specific-guidelines)
- [Getting Help](#getting-help)
- [License](#license)

## Code of Conduct

This project adheres to the Rust Code of Conduct. By participating, you are expected to uphold this code. Please report unacceptable behavior to the project maintainers.

## Getting Started

### Prerequisites

Before you begin, ensure you have the following installed:

- **Rust nightly toolchain** - Required for specific feature flags used in the project
  ```bash
  rustup toolchain install nightly
  rustup default nightly
  ```
- **[cargo-make](https://github.com/sagiegurari/cargo-make)** - Task runner used throughout the project
  ```bash
  cargo install cargo-make
  ```
- **[cargo-nextest](https://nexte.st/)** - Fast test runner (optional but recommended)
  ```bash
  cargo install cargo-nextest
  ```
- **Docker and Docker Compose** - Required for running the local devnet and E2E tests
  - Install Docker: https://docs.docker.com/get-docker/
  - Docker Compose is typically included with Docker Desktop

### Forking and Cloning

1. Fork the repository on GitHub
2. Clone your fork:
   ```bash
   git clone https://github.com/yourfork/prism.git
   cd rpc-aggregator
   ```
3. Add the upstream repository:
   ```bash
   git remote add upstream https://github.com/prismrpc/prism.git
   ```

## Development Setup

### Initial Build

Build the project to ensure everything is set up correctly:

```bash
# Quick development build
cargo make build

# Or build in release mode
cargo make build-release
```

### Running the Development Server

Start the server with development configuration:

```bash
# Development config (pretty logs)
cargo make run-server-dev

# Or with devnet configuration
cargo make run-server-devnet
```

### Setting Up the Devnet

For E2E testing, you'll need the local Ethereum devnet:

```bash
# Start the devnet (4 Geth nodes in PoA mode)
cargo make devnet-start

# Check health status
cargo make devnet-health

# Stop when done
cargo make devnet-stop
```

## Development Workflow

### 1. Create a Branch

Always create a new branch for your work:

```bash
git checkout -b feature/your-feature-name
# or
git checkout -b fix/your-bug-fix
# or
git checkout -b docs/your-documentation-update
```

Use descriptive branch names that indicate the type of change:
- `feature/` - New features
- `fix/` - Bug fixes
- `docs/` - Documentation updates
- `refactor/` - Code refactoring
- `test/` - Test additions or improvements
- `perf/` - Performance improvements

### 2. Make Your Changes

- Write clear, maintainable code
- Follow existing code patterns and conventions
- Add tests for new functionality
- Update documentation as needed
- Keep commits focused and logical

### 3. Run Pre-Commit Checks

Before committing, run the pre-commit validation:

```bash
cargo make pre-commit
```

This runs:
- `format-check` - Verifies code formatting
- `clippy` - Lints the code
- `test` - Runs all tests (excluding E2E)

### 4. Commit Your Changes

Follow the [commit message guidelines](#commit-message-guidelines) below.

### 5. Push and Create a Pull Request

```bash
git push origin feature/your-feature-name
```

Then create a pull request on GitHub.

## Code Style and Formatting

### Formatting

We use `rustfmt` with project-specific configuration. Always format your code:

```bash
# Format all code
cargo make format

# Check formatting without modifying files
cargo make format-check
```

The project uses `rustfmt.toml` for formatting rules. Key settings:
- Import granularity: Crate-level
- Comment width: 100 characters
- Code block width in docs: 100 characters

### Linting

We use `clippy` with strict warnings. All clippy warnings must be addressed:

```bash
# Run clippy (excludes benchmarks)
cargo make clippy

# Run clippy on everything including benchmarks
cargo make clippy-all
```

The project uses `clippy.toml` for linting configuration. All warnings are treated as errors in CI.

### Code Style Guidelines

1. **Use meaningful names**: Variable, function, and type names should clearly express their purpose
2. **Keep functions focused**: Functions should do one thing well
3. **Document public APIs**: All public functions, types, and modules should have doc comments
4. **Handle errors explicitly**: Use `Result` types and proper error handling; avoid `unwrap()` in production code
5. **Prefer `?` operator**: Use the `?` operator for error propagation when appropriate
6. **Use async/await**: This is an async project; use async/await patterns consistently
7. **Follow Rust idioms**: Leverage Rust's type system, ownership, and borrowing

### Example: Adding a New Module

When adding a new module to `prism-core`, follow this structure:

```rust
//! Module-level documentation explaining the purpose and key concepts
//!
//! # Overview
//!
//! Brief description of what this module provides.

use crate::other_module::Type;

/// Public type with comprehensive documentation
///
/// # Examples
///
/// ```rust
/// // Example usage
/// ```
pub struct MyType {
    // Fields
}

impl MyType {
    /// Method documentation
    pub fn new() -> Self {
        // Implementation
    }
}
```

## Testing Guidelines

### Test Organization

Tests are organized as follows:

- **Unit tests**: In the same file as the code they test (using `#[cfg(test)]`)
- **Integration tests**: In `crates/tests/` for cross-crate functionality
- **E2E tests**: In `crates/tests/src/e2e/` for full system tests requiring devnet

### Running Tests

```bash
# Run all unit tests (excludes E2E)
cargo make test

# Run specific test suite
cargo make test-core          # prism-core tests only
cargo make test-performance   # Performance correctness tests

# Run E2E tests (requires devnet + server running)
cargo make devnet-start
cargo make run-server-devnet
cargo make e2e-rust           # All E2E tests
cargo make e2e-rust-cache     # Cache tests only
cargo make e2e-rust-failover  # Failover tests
```

### Writing Tests

1. **Test naming**: Use descriptive test names that explain what is being tested
   ```rust
   #[tokio::test]
   async fn test_cache_hit_returns_cached_block() {
       // Test implementation
   }
   ```

2. **Test organization**: Group related tests in modules
   ```rust
   #[cfg(test)]
   mod tests {
       use super::*;
       
       #[tokio::test]
       async fn test_feature_a() {}
       
       #[tokio::test]
       async fn test_feature_b() {}
   }
   ```

3. **E2E tests**: Mark E2E tests with the `e2e` feature flag
   ```rust
   #[cfg(feature = "e2e")]
   #[tokio::test]
   async fn test_end_to_end_flow() {
       // E2E test implementation
   }
   ```

4. **Test data**: Use realistic test data that reflects real-world usage
5. **Cleanup**: Ensure tests clean up after themselves (especially E2E tests)

### Performance Tests

For performance-critical code, add benchmarks using Criterion:

```bash
# Run benchmarks
cargo make bench

# Run specific benchmark
cargo make bench-cache

# Save baseline for comparison
cargo make bench-save

# Compare against baseline
cargo make bench-compare
```

## Pull Request Process

### Before Submitting

1. **Ensure all checks pass**:
   ```bash
   cargo make pre-commit
   ```

2. **Run E2E tests** (if your changes affect core functionality):
   ```bash
   cargo make devnet-start
   cargo make run-server-devnet
   cargo make e2e-rust
   ```

3. **Update documentation** if you've changed APIs or behavior

4. **Rebase on latest main**:
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```

### PR Description Template

When creating a PR, include:

```markdown
## Description
Brief description of what this PR does.

## Type of Change
- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation update
- [ ] Performance improvement
- [ ] Refactoring

## Testing
- [ ] Unit tests added/updated
- [ ] Integration tests added/updated
- [ ] E2E tests added/updated
- [ ] Manual testing performed

## Checklist
- [ ] Code follows project style guidelines
- [ ] Self-review completed
- [ ] Comments added for complex code
- [ ] Documentation updated
- [ ] No new warnings generated
- [ ] Tests pass locally
- [ ] PR description is clear and complete
```

### Review Process

1. **Automated checks**: All PRs must pass CI checks (formatting, clippy, tests)
2. **Code review**: At least one maintainer will review your code
3. **Address feedback**: Respond to review comments and make requested changes
4. **Keep PRs focused**: Large changes should be split into smaller, reviewable PRs

### PR Size Guidelines

- **Small PRs (< 200 lines)**: Preferred for faster review
- **Medium PRs (200-500 lines)**: Acceptable with good organization
- **Large PRs (> 500 lines)**: Consider splitting into multiple PRs

## Commit Message Guidelines

We follow conventional commit message format:

```
<type>(<scope>): <subject>

<body>

<footer>
```

### Types

- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `test`: Test additions or changes
- `chore`: Maintenance tasks

### Examples

```
feat(cache): add partial range fulfillment for eth_getLogs

Implement bitmap-based tracking to enable partial cache hits
for log queries spanning multiple blocks. When a query covers
blocks 100-110 and blocks 100-105 are cached, only fetch
106-110 from upstream.

Closes #123
```

```
fix(upstream): handle WebSocket reconnection failures

Add exponential backoff and circuit breaker for WebSocket
connections that fail to reconnect after multiple attempts.

Fixes #456
```

```
docs(proxy): update routing strategy documentation

Clarify consensus validation quorum requirements and add
examples for different use cases.
```

## Documentation

### Code Documentation

- **Public APIs**: All public functions, types, and modules must have doc comments
- **Complex logic**: Document non-obvious algorithms and design decisions
- **Examples**: Include usage examples in doc comments where helpful

```rust
/// Fetches a block by number with caching support.
///
/// This method first checks the cache for the requested block. If not found,
/// it fetches from upstream and caches the result for future requests.
///
/// # Arguments
///
/// * `block_number` - The block number to fetch (as hex string or decimal)
///
/// # Returns
///
/// Returns `Ok(Some(Block))` if the block is found, `Ok(None)` if the block
/// doesn't exist, or an error if the request fails.
///
/// # Examples
///
/// ```rust
/// let block = fetch_block("0x1234").await?;
/// ```
pub async fn fetch_block(block_number: &str) -> Result<Option<Block>> {
    // Implementation
}
```

### Architecture Documentation

- Update `docs/architecture.md` for significant architectural changes
- Add component-specific docs in `docs/components/` when adding new modules
- Keep diagrams and data flow descriptions up to date

### README Updates

- Update the main README if you add features or change behavior
- Keep the "Getting Started" section accurate
- Update configuration examples if you change config structure

## Project-Specific Guidelines

### Ethereum RPC Knowledge

This project deals with Ethereum JSON-RPC APIs. Familiarize yourself with:

- Ethereum JSON-RPC specification
- Block structure and chain reorganizations
- Transaction and receipt formats
- Event log structure
- Common RPC methods (`eth_getBlockByNumber`, `eth_getLogs`, etc.)

### Caching Semantics

When working with the cache system:

- **Immutable data**: Blocks, transactions, and receipts can be cached indefinitely after sufficient confirmations
- **Chain reorgs**: Cache must handle reorganizations correctly
- **Partial fulfillment**: Log cache supports partial range fulfillment
- **Cache invalidation**: Understand when and why cache entries are invalidated

### Async Programming

This is a high-performance async Rust project:

- Use `tokio` for async runtime
- Prefer `Arc` for shared state in async contexts
- Be mindful of blocking operations in async code
- Use appropriate synchronization primitives (`Arc`, `Mutex`, `RwLock`, etc.)

### Error Handling

- Use `thiserror` for structured error types
- Provide context with `anyhow::Context` when appropriate
- Log errors at appropriate levels (error, warn, debug)
- Return meaningful error messages to clients

### Performance Considerations

- Cache operations should be O(1) or O(log n) where possible
- Avoid unnecessary allocations in hot paths
- Use appropriate data structures (e.g., `DashMap` for concurrent access)
- Profile before optimizing

## Getting Help

### Resources

- **Project README**: Start here for overview and setup
- **Architecture docs**: `docs/architecture.md` for system design
- **Component docs**: `docs/components/` for detailed module documentation
- **Issues**: Search existing issues before creating new ones
- **Discussions**: Use GitHub Discussions for questions and ideas

### Asking Questions

When asking for help:

1. **Search first**: Check existing issues, discussions, and documentation
2. **Be specific**: Include relevant code, error messages, and context
3. **Provide reproduction steps**: For bugs, include steps to reproduce
4. **Share environment**: Rust version, OS, and relevant configuration

### Reporting Bugs

Include in your bug report:

- **Description**: Clear description of the issue
- **Steps to reproduce**: Detailed steps to reproduce the bug
- **Expected behavior**: What should happen
- **Actual behavior**: What actually happens
- **Environment**: Rust version, OS, configuration
- **Logs**: Relevant log output (with sensitive data redacted)
- **Minimal reproduction**: If possible, a minimal test case

## License

By contributing to this project, you agree that your contributions are licensed under the same **MIT OR Apache 2.0** dual license model as the project.

This means:
- Your contributions will be available under both MIT and Apache 2.0 licenses
- Users can choose either license when using the project
- You retain copyright to your contributions

See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE) for the full license texts.

---

Thank you for contributing to Prism! Your efforts help make this project better for everyone.

