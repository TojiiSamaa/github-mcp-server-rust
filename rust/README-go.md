# The original Go server (`README-go.md`)

This repository is a fork of [github/github-mcp-server](https://github.com/github/github-mcp-server). The Rust server (`ghmcp`) is documented in [`README.md`](README.md). The **original Go server stays intact** at the root of the fork: same code, same behavior, same documentation. Nothing in `rust/` changes it, and nothing here replaces it.

When the Rust version is published, the fork keeps the original `README.md` of the Go server under the name `README-go.md` (the same convention as `README-python.md` in the NetBox fork), and the Rust `README.md` takes its place at the root. Read the upstream README for everything about the Go server: installation, remote server, toolsets, flags, Docker, OAuth. The upstream documentation is also in [`docs/`](docs/) of the fork.

*Français : le serveur Go d'origine reste intact dans le fork. Son README d'origine est renommé `README-go.md` ; le `README.md` à la racine décrit la version Rust (dossier `rust/`).*

## Build the Go server

From the root of the fork, with Go installed (the version is in `go.mod`):

```bash
go build -o github-mcp-server ./cmd/github-mcp-server
GITHUB_PERSONAL_ACCESS_TOKEN=... ./github-mcp-server stdio
```

Useful flags of the Go binary: `--toolsets` (enable groups of tools), `--read-only` (offer only read tools). With Docker, use the `Dockerfile` at the root:

```bash
docker build -t github-mcp-server .
```

Build metadata (version shown in the API user agent) can be set with `-ldflags '-X main.version=<release>'` or `-ldflags '-X main.commit=<sha>'`; a build from a Git checkout embeds its revision by itself.

## Which one to use

| | Go server (root) | Rust server (`rust/`) |
|---|---|---|
| Tools | the official set | the same official tools + `gh_*` extensions |
| Accounts | one token per process | several accounts, `account` on every call |
| Webhooks, activity journal, rules | no | yes (`ghmcp serve`) |
| Test mode without network | no | yes (`--test`) |

The Go workflows of the fork are disabled (suffix `.disabled`) because they build at the root and would publish on the fork by mistake; the Rust workflows declare `working-directory: rust`.

License of the Go server: MIT, Copyright (c) 2025 GitHub (see `LICENSE` at the root of the fork).
