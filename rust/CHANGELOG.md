# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-10  <!-- scan:ok -->

First release: a Rust MCP server for GitHub that keeps the official tool schemas and adds extended administration, multi-account support, activity tracking and rules.

### Added

- Workspace and MCP core: multiple accounts, single egress module, test mode (`--test` / `MCP_TEST_MODE=1`) that simulates every call, usage statistics.
- Ports of the official tools: repositories, issues, pull requests, search, git, gists, context, notifications, Actions, discussions, projects.
- Extended administration: repositories, organizations and invitations, webhooks, Actions administration, security, rulesets and Copilot, packages, environments and Pages.
- Permission schema, account audit and creation URLs.
- Activity journal, webhook receiver (`ghmcp serve`, HMAC SHA-256 signature check), activity API and SSE stream.
- Rule engine, destinations, allow list, dashboard and periodic report.
- `token_cmd` per account (command whose output is the token), run once per account and cached.
- SECURITY, CONTRIBUTING, CHANGELOG and CODE_OF_CONDUCT documents.
- Separate rules file (`rules_file`, `GHMCP_RULES`).
- Copilot GraphQL features and native `get_diff`.
- Skill, `AGENTS.md` and README; secrets guide.
- Parity tests against the official server: schema snapshots, method and route per tool checked against the Go source, Go route extractor (`scripts/extraire-routes-go.mjs`), end-to-end test of `serve`, proof of test mode, skill-versus-`tools/list` check.
- `scripts/scan-donnees.sh` (no real data in the repository) and a `.gitignore` covering tokens, keys and local data.

### Security

- `POST /mcp` over HTTP now requires the `[webhook] api_token_env` Bearer token when it is set, and `ghmcp` refuses to listen on a non-loopback address without it.

### Fixed

- `test_mode` marker guaranteed on every response in test mode.
- `sub_issue_write` remove uses the `/sub_issue` route.
- Empty `GHMCP_CONFIG` is ignored.
- `projects_*` methods and `field_names`; `update_project_items` and project items in GraphQL with pagination.
- Actions tools return the 302 redirect URL.
- `add_issue_comment` reacts on the issue without `comment_id` and checks the comment belongs to the issue.
- `list_issue_types` and `update_issue_state` follow the Go routes.
- `issue_read` (get), `search_issues`, `list_issues`, `list_label` and `get_label` follow the Go GraphQL behavior, including `field_filters`.
- `delete_file`, `create_or_update_file` and `get_file_contents` follow the Go git data chain and symbolic-link guard; `get_file_contents` returns the embedded resource.
- Route extractor: `Sprintf` arguments, `url.PathEscape`, path parameters bound by the caller, all candidate cases, go-github resolved from the module cache.
- `token_cmd` runs once per account, concurrent calls wait on the cached result.
- Clippy clean with `-D warnings`.

### Changed

- Route coverage threshold raised to 90 %; documented `CHOIX_REST` exceptions.
- Secrets, README and skill re-read against the code.

[Unreleased]: ../../compare/rust-v0.1.0...HEAD
[0.1.0]: ../../releases/tag/rust-v0.1.0
