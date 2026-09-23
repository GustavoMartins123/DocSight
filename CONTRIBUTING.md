# Contributing to DocSight

Thank you for your interest in contributing to DocSight. DocSight is a deterministic, headless evidence layer for inspecting DOCX and PDF documents from a terminal, designed for autonomous agents and developer tooling.

## Architectural Principles

Before contributing code, please review the core project principles:

1. **Specification as Source of Truth:** Features, schemas, and behavior adhere to the formal specification and `AGENTS.md`.
2. **Fail-Closed Design:** Never add functional fallbacks, degraded alternative routes, or silent approximations. If canonical processing fails, return an explicit typed error with diagnostics.
3. **Determinism:** Identical input bytes, engine versions, and options must produce byte-identical JSON outputs and rendered artifacts across platforms.
4. **No Code Comments:** Do not add line comments, block comments, TODOs, FIXMEs, or doc comments to Rust code unless explicitly requested by the maintainer. Code must be self-documenting through precise types, descriptive identifiers, exhaustive matching, and small single-purpose functions.
5. **No Network Dependencies at Runtime:** DocSight runs entirely local and offline. Never introduce network access, telemetry, or remote dependencies into the engine or runtime CLI.

## Prerequisites

- **Rust:** Stable 1.96.0 (declared in `rust-toolchain.toml`). Install via `rustup`:
  ```bash
  rustup show
  ```
- **Git:** With LF line endings configured (`core.autocrlf = input` or `false`).
- Supported operating systems: Linux (x86_64), Windows (x86_64), and macOS (aarch64).

## Building and Testing

Build the workspace in debug or release mode:

```bash
cargo build --locked
cargo build --locked --release
```

Run unit and integration tests across all workspace crates:

```bash
cargo test --locked --workspace --all-features
```

### Pre-Submission Verification

Every pull request must pass the automated maintainer verification suite:

1. **Formatting:**
   ```bash
   cargo fmt --all --check
   ```

2. **Linting:**
   ```bash
   cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
   ```

3. **Tooling & Architecture Contracts:**
   ```bash
   cargo test --locked -p xtask --all-targets
   cargo xtask rust-only
   cargo xtask corpus validate
   ```

## Pull Request Guidelines

- **Atomic Changes:** Keep pull requests focused on a single change, issue, or feature vertical.
- **Conventional Commits:** Use standard conventional commit prefixes (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `perf:`).
- **No Unsafe Code:** `unsafe_code` is strictly denied across the workspace.
- **Error Propagation:** Do not use `unwrap()`, `expect()`, `panic!()`, `todo!()`, or `unimplemented!()` in production code. Propagate errors using typed error structures.
- **Contract Integrity:** Changes to CLI flags, agent schemas, IDs, exit codes, or canonical JSON serialization require corresponding contract tests and documentation updates.
