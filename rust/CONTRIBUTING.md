# Contributing

Thanks for helping. Keep changes small, tested and documented.

## Build

```sh
cargo build --workspace
```

Rust 1.80 or newer (workspace `rust-version`).

## Checks before a pull request

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/scan-donnees.sh
```

- **Tests need no token and no network.** The suite runs the server in test mode (`--test` / `MCP_TEST_MODE=1`), where every GitHub call is simulated and network egress is cut. Never make a test depend on a real token.
- `scripts/scan-donnees.sh` must print nothing and exit 0 (it looks for real IPs, MACs, local paths, non-`FAKE` tokens and real names).
- Benchmarks, when the `bench/` directory is present: run `bench/run` and attach the numbers to performance changes.
- A bug fix comes with a test that fails before the fix and passes after it, on the real shape of the data.

## Commits

[Conventional Commits](https://www.conventionalcommits.org/): `feat:`, `fix:`, `docs:`, `test:`, `chore:`, `refactor:`, `perf:`. One subject line, imperative, lower case after the colon.

## Rules

- **No real data in the repository**: no real token, address, IP, MAC, name, e-mail or local path, tests and fixtures included. Use `FAKE` tokens, `*.invalid` hosts and the documentation IP ranges (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24).
- **When a tool changes** (name, parameters, behavior), update in the same pull request the skill under `skill/` and the [README](README.md); `crates/server/tests/skill.rs` checks that the skill lists exactly the tools of `tools/list`.
- Network access goes only through the single egress module of `ghmcp-core`; do not add another HTTP client.
- Dependencies are declared once in the root `Cargo.toml` (`x.workspace = true` in crates).

## Conduct and security

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).
