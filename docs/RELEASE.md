# Cutting a source-only alpha

The release workflow is `.github/workflows/release.yml`. It runs on a `v*` tag
and checks the tagged source before publishing a GitHub pre-release.

## Preconditions

- The version in `Cargo.toml` is `X.Y.Z` and the tag is `vX.Y.Z`.
- `docs/releases/vX.Y.Z.md` exists and is the curated release note.
- `main` is green and the release issues in the `Release` milestone are closed.
- The tag is on the reviewed commit.

## Cut the release

```bash
git checkout main
git pull --ff-only
git tag -a v0.1.0-alpha.1 -m "Berlin Wallet v0.1.0-alpha.1"
git push origin v0.1.0-alpha.1
```

The workflow then:

1. verifies the tag and workspace version;
2. runs formatting, clippy, tests, and the status check;
3. creates a source archive and SHA-256 checksum;
4. publishes a GitHub **pre-release** with the checked-in notes and assets.

It does not build a binary, publish to crates.io, or mark the release as latest.

## Verify

```bash
gh release view v0.1.0-alpha.1 --json isPrerelease,assets,url
gh release download v0.1.0-alpha.1 --pattern '*.sha256' --dir /tmp/bw-release
sha256sum -c /tmp/bw-release/*.sha256
```

If the workflow fails before the release exists, fix the cause and delete/recreate
the local and remote tag. Once the pre-release is published, do not move the tag;
cut a new alpha instead.

## Manual fallback

```bash
mkdir -p dist
git archive --format=tar.gz --prefix=berlin-wallet-v0.1.0-alpha.1/ \
  --output=dist/berlin-wallet-v0.1.0-alpha.1.tar.gz HEAD
sha256sum dist/*.tar.gz > dist/berlin-wallet-v0.1.0-alpha.1.tar.gz.sha256
gh release create v0.1.0-alpha.1 dist/* --verify-tag --prerelease --latest=false \
  --title "Berlin Wallet v0.1.0-alpha.1" \
  --notes-file docs/releases/v0.1.0-alpha.1.md
```
