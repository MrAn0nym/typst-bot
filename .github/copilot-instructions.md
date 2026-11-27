# Copilot instructions

## Project architecture

- This is a Rust workspace with three crates: `bot`, `protocol`, and `worker`.
- `crates/bot` connects to Discord through Poise/Serenity, handles commands and inline-math message events, and manages the worker process.
- `crates/protocol` defines the requests and responses serialized between the bot and worker with bincode over the worker's stdin/stdout.
- `crates/worker` compiles Typst, renders PNG pages, formats diagnostics, and provides the sandboxed Typst world.
- `typst-bot` starts the worker automatically. See `README.md` for runtime configuration and deployment.

## Build and test

Rust is not installed on the developer's host, and the developer does not want host packages or dependencies installed. Do not install Rust, Cargo, or project dependencies on the host. Use Docker for builds and tests.

Implement the requested code changes first, then build and test the completed change in Docker. Mount only the repository into the container; do not mount host Cargo directories, session state, or other paths, and do not write project-related files outside the repository.

Run the full workspace tests with:

```sh
docker run --rm -v "$PWD:/workspace" -w /workspace \
  lukemathwalker/cargo-chef:latest-rust-1 cargo test --workspace
```

Run a focused bot test by replacing `test_name_fragment` with part of the test function's name:

```sh
docker run --rm -v "$PWD:/workspace" -w /workspace \
  lukemathwalker/cargo-chef:latest-rust-1 cargo test -p typst-bot test_name_fragment
```

The CI test workflow runs `cargo test` (the workspace's default members). Use `--workspace` when validating all crates.

Build the production image with:

```sh
docker compose build bot
```

CI checks formatting with nightly rustfmt. To run that check without installing tools on the host:

```sh
docker run --rm -v "$PWD:/workspace" -w /workspace \
  lukemathwalker/cargo-chef:latest-rust-1 \
  sh -c 'rustup toolchain install nightly --component rustfmt && cargo +nightly fmt -- --check'
```
