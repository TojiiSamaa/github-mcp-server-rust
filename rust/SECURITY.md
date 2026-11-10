# Security Policy

## Reporting a vulnerability

**Do not open a public issue, discussion or pull request for a vulnerability.**

Report it privately through GitHub Security Advisories of this repository: open the **Security** tab, then **Report a vulnerability** ([direct link](../../security/advisories/new)). Include:

- the affected component (see Scope) and version or commit;
- the configuration needed to reproduce (redact every token, secret and address: use `FAKE` tokens and documentation IP ranges);
- step-by-step reproduction, proof of concept if possible;
- the impact and how it could be exploited.

You will get an acknowledgement and a first assessment on a best-effort basis. Fixes are developed in the private advisory and published with it.

## Supported versions

| Version | Supported |
|---------|-----------|
| latest release (0.1.x) | yes |
| `main` branch | yes, fixes land here first |
| anything older | no |

## Scope

In scope:

- **Tokens**: leakage of a GitHub token (logs, errors, responses, audit journal), wrong account used for a call, `token_env` / `token_file` / `token_cmd` resolution (see [`docs/secrets.md`](docs/secrets.md)).
- **`ghmcp serve`**: webhook receiver (`POST /webhook`, `/webhook/{account}`): HMAC SHA-256 signature check, replay handling, the activity API and SSE stream, rule engine and destinations.
- **`token_cmd`**: command injection, shell interpretation (it must run a program with arguments, never a shell), runaway or repeated execution.
- **Test mode** (`--test` / `MCP_TEST_MODE=1`): any path that reaches the network or reads a secret while active.

Out of scope: vulnerabilities in GitHub itself, in the MCP client, or in a secret manager used through `token_cmd`; a token you committed or shared yourself (revoke it); missing hardening of a deployment you control (reverse proxy, TLS, firewall).

## Good practice for users

Give each account the least-privileged token, set a webhook secret before exposing `serve` (without one the receiver refuses everything), and run `scripts/scan-donnees.sh` before publishing anything derived from this repository.
