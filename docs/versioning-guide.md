# Versioning and Release Guide for Prism

This guide explains semantic versioning and Git tagging best practices for Prism.

## Semantic Versioning Explained

The format is `MAJOR.MINOR.PATCH` (e.g., `0.1.0`, `1.2.3`):

| Component | When to Bump | Example |
|-----------|--------------|---------|
| **MAJOR** | Breaking changes that require users to modify their code | API endpoint renamed, config format changed |
| **MINOR** | New features that are backwards-compatible | Added new caching strategy, new RPC method support |
| **PATCH** | Bug fixes that are backwards-compatible | Fixed memory leak, corrected error handling |

### Special Rules for 0.x.x Versions

For versions before `1.0.0`, the rules are relaxed:

```
0.x.y = "Development phase, anything can change"
```

- **0.1.0** → Initial public release
- **0.1.1** → Bug fix
- **0.2.0** → New feature OR breaking change (both OK in 0.x)
- **1.0.0** → "Production ready" - committing to stability

### Pre-release Tags

For testing releases before they're stable:

```
0.1.0-alpha.1  → Very early, likely buggy
0.1.0-beta.1   → Feature complete, needs testing
0.1.0-rc.1     → Release candidate, final testing
0.1.0          → Stable release
```

## Git Tagging Best Practices

### Creating Tags

```bash
# Annotated tags (recommended for releases)
git tag -a v0.1.0 -m "Initial public release"

# Push tags to remote
git push origin v0.1.0

# Or push all tags
git push origin --tags
```

### Tag Naming Convention

Always prefix with `v` for version tags:

```
 v0.1.0, v1.0.0, v0.2.0-beta.1
 0.1.0, release-0.1.0, version-0.1.0
```

This is important because:
1. GitHub recognizes `v*.*.*` pattern for releases
2. The release workflow is configured to trigger on `v*.*.*` tags
3. It's the Rust ecosystem convention

## Creating Your First Release (v0.1.0)

### Step 1: Verify Everything is Ready

```bash
# Run the full validation
cargo make pre-commit

# Verify release build works
cargo make build-release
```

### Step 2: Update CHANGELOG Date

Edit `CHANGELOG.md` and change:
```markdown
## [0.1.0] - 2024-XX-XX
```
to the actual release date.

### Step 3: Create the Release

```bash
# 1. Commit any final changes
git add -A
git commit -m "chore: prepare v0.1.0 release"

# 2. Create annotated tag
git tag -a v0.1.0 -m "Initial public release of Prism RPC Aggregator

Features:
- Intelligent request routing with consensus, hedging, and scoring
- Multi-layer caching system with partial range fulfillment
- Load balancing with automatic failover
- API key authentication with rate limiting
- Prometheus metrics and structured logging
- Docker and binary deployment support"

# 3. Push to GitHub
git push origin main      # or your branch
git push origin v0.1.0    # triggers release workflow
```

### What Happens Automatically

Once you push the tag, GitHub Actions will:

1. **Create a GitHub Release** at `github.com/prismrpc/prism/releases`
2. **Build binaries** for Linux (x86_64, ARM64, musl) and macOS (Intel, Apple Silicon)
3. **Push Docker image** to `ghcr.io/prismrpc/prism:0.1.0`
4. **Generate checksums** for security verification

## Version Strategy Recommendation

| Version | Meaning | When |
|---------|---------|------|
| **0.1.0** | Initial release | First public version |
| **0.1.x** | Bug fixes | Fix issues found by users |
| **0.2.0** | New features | Add Kubernetes support, new caching strategies |
| **0.x.0** | Continue iterating | Add features, may have breaking changes |
| **1.0.0** | Stable/Production | When API is stable and confident in the design |

## Staying in 0.x.x

Staying in `0.x.x` is perfectly fine for open-source projects. Many successful projects stay in 0.x for a long time:

- `0.x` signals "still evolving, use with awareness"
- `1.0` is a promise of stability that's hard to take back
- Only go to 1.0 when confident in long-term API stability

## Useful Git Tag Commands

```bash
# List all tags
git tag -l

# List tags matching pattern
git tag -l "v0.1.*"

# Show tag details
git show v0.1.0

# Delete a local tag (if you made a mistake)
git tag -d v0.1.0

# Delete a remote tag (use with caution!)
git push origin --delete v0.1.0

# Checkout a specific tag
git checkout v0.1.0

# Create a branch from a tag (for hotfixes)
git checkout -b hotfix/0.1.1 v0.1.0
```

## See Also

- [Release Process](../.github/RELEASE_PROCESS.md) - Detailed release checklist
- [CHANGELOG.md](../CHANGELOG.md) - Version history
- [Semantic Versioning Specification](https://semver.org/)
