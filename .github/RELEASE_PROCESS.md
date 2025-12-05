# Prism Release Process

This document describes how to create a new release of Prism.

## Version Numbering

Prism follows [Semantic Versioning](https://semver.org/):

- **MAJOR.MINOR.PATCH** (e.g., `0.1.0`, `1.2.3`)
- While in `0.x.x`: Breaking changes can happen in MINOR bumps
- After `1.0.0`: Breaking changes require MAJOR bump

### When to Bump Each Component

| Change Type | Version Bump | Example |
|-------------|--------------|---------|
| Breaking API/config change | MINOR (0.x) or MAJOR (1.x+) | Changed config format |
| New feature | MINOR | Added new caching layer |
| Bug fix | PATCH | Fixed memory leak |
| Documentation only | No release needed | - |

## Pre-Release Checklist

Before creating a release:

- [ ] All tests pass: `cargo make test`
- [ ] Clippy clean: `cargo make clippy`
- [ ] Build succeeds: `cargo make build-release`
- [ ] Update version in `Cargo.toml` (workspace section, line 8)
- [ ] Update `CHANGELOG.md` with release notes
- [ ] Ensure `README.md` is up to date

## Creating a Release

### 1. Update Version

Edit `Cargo.toml` workspace section:

```toml
[workspace.package]
version = "0.2.0"  # Update this
```

### 2. Update CHANGELOG

Add release date and move items from `[Unreleased]`:

```markdown
## [Unreleased]

## [0.2.0] - 2024-01-15

### Added
- New feature X

### Fixed
- Bug in Y
```

### 3. Commit Version Bump

```bash
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore: bump version to 0.2.0"
```

### 4. Create and Push Tag

```bash
# Create annotated tag
git tag -a v0.2.0 -m "Release v0.2.0"

# Push commit and tag
git push origin main
git push origin v0.2.0
```

### 5. Verify Release

1. Check GitHub Actions - release workflow should trigger
2. Verify the Releases page shows new release
3. Verify Docker image at `ghcr.io/prism/prism:0.2.0`
4. Download and test a binary

## Pre-release Versions

For testing before stable release:

```bash
# Alpha (early testing)
git tag -a v0.2.0-alpha.1 -m "Alpha release for testing"

# Beta (feature complete, needs testing)
git tag -a v0.2.0-beta.1 -m "Beta release"

# Release candidate (final testing)
git tag -a v0.2.0-rc.1 -m "Release candidate 1"
```

Pre-releases:
- Are marked as "Pre-release" on GitHub
- Docker images tagged but NOT marked as `latest`
- Good for community testing before stable release

## Hotfix Process

For urgent fixes to a released version:

```bash
# Create hotfix branch from tag
git checkout -b hotfix/0.1.1 v0.1.0

# Make fix, commit, then tag
git tag -a v0.1.1 -m "Hotfix: critical bug fix"
git push origin v0.1.1
```

## Version History Guidelines

### 0.1.x Series (Current)
- Initial public release
- API may change between minor versions
- Focus on stability and documentation

### Future 1.0.0
Consider releasing 1.0.0 when:
- API is stable and well-documented
- Production deployments are successful
- Community feedback is incorporated
- You're confident in backwards compatibility commitment

## Troubleshooting

### Release workflow didn't trigger
- Ensure tag matches pattern `v*.*.*`
- Check you pushed the tag: `git push origin v0.1.0`
- Verify workflow file exists in `.github/workflows/release.yml`

### Docker image not published
- Check GitHub Actions logs for errors
- Verify `packages: write` permission in workflow
- Ensure repository is public or package settings allow access

### Binary checksum mismatch
- Re-download the file
- Check for incomplete download
- Verify correct platform binary
