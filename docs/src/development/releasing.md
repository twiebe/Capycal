# Releasing

Releases are built by GitHub Actions (`.github/workflows/release.yml`) when a tag `v*` is pushed.

1. Set `version` in `Cargo.toml` and update `Cargo.lock` (`cargo build`).
2. Commit, then tag and push. The tag must be `v` plus the version from `Cargo.toml`; the workflow fails otherwise.

   ```bash
   git tag v0.2.0
   git push origin main v0.2.0
   ```

The workflow:

- builds and tests the CLI for `aarch64-apple-darwin`, `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` on native runners, and packs each into `capycal-v<version>-<target>.tar.gz` (binary, `README.md`, `LICENSE`, `CONTRIBUTORS.md`, `THIRD-PARTY-NOTICES.txt`, `capycal.example.toml`);
- builds `Capycal.app` and `capycal-v<version>-macos.dmg`;
- writes a `.sha256` file per archive;
- publishes a GitHub release with generated notes.

A manual run (`workflow_dispatch`) builds and uploads the artifacts without publishing a release.

## Other workflows

| Workflow | Trigger | Does |
|---|---|---|
| `ci.yml` | push to `main`, pull requests | fmt, clippy and tests on Linux and macOS, with and without `ui` |
| `docs.yml` | push to `main` or pull request changing `docs/`, manual | builds this book; on `main` it asks `twiebe/capycal-web` to publish it on capycal.app/docs |
