<div align="center">

<img src="https://raw.githubusercontent.com/unison-labs-ai/unison-brain/main/assets/brain.svg" width="140" alt="Unison" />

# unison-fs

**Mount your agent's brain as a folder. ls, cat, grep, and edit your memory.**

A filesystem for AI agents — mount the [Unison brain](https://unisonlabs.ai) as a real local directory (FUSE on Linux, embedded NFS on macOS) with a local SQLite cache, background sync loop, and semantic `sgrep` command.

The folder is just the surface. Underneath is the [Unison brain](https://github.com/unison-labs-ai/unison-brain#the-hard-part--what-every-memory-system-gets-wrong) — not a flat vector dump but a knowledge graph with **temporal facts that know what changed when**, **entity resolution that knows who's who**, and **one shared source of truth** every agent and teammate reads and writes. So `cat` and `sgrep` return the *current, consistent* answer, not a stale snippet.

[![CI](https://github.com/unison-labs-ai/unison-fs/actions/workflows/ci.yml/badge.svg)](https://github.com/unison-labs-ai/unison-fs/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Stars](https://img.shields.io/github/stars/unison-labs-ai/unison-fs?style=social)](https://github.com/unison-labs-ai/unison-fs)

[**Install**](#install) • [**Quickstart**](#quickstart) • [**Commands**](#commands) • [**Semantic search**](#semantic-search) • [**Architecture**](#architecture)

</div>

---

Read, write, and `sgrep` the Unison brain like any local folder. Editors, scripts, and AI agents that already understand files work without any changes.

```sh
unisonfs login                        # store your UNISON_TOKEN
unisonfs mount ~/brain                # mount the brain at ~/brain
ls ~/brain/private/notes/             # browse your private notes
cat ~/brain/workspace/people/daniel.md   # read a workspace-level doc
echo "# My Note" > ~/brain/private/notes/idea.md  # write syncs to the brain
sgrep "auth decisions"                # semantic search
unisonfs unmount ~/brain              # unmount and drain the push queue
```

Two access flows:

- **Mount it as a directory.** A real local folder for editors, scripts, and any tool that reads files. Works on macOS (NFS backend, no kernel extension) and Linux (FUSE backend).
- **Semantic `sgrep`.** Install once with `unisonfs init`; then `sgrep "natural language query"` anywhere to search the brain.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/unison-labs-ai/unison-fs/main/install.sh | bash
```

Or build from source:

```sh
cargo build --release
./target/release/unisonfs --help
```

Requires Rust 1.80 or newer.

## Environment variables

| Variable | Description |
|---|---|
| `UNISON_TOKEN` | Your `usk_live_...` API token (takes precedence over config file) |
| `UNISON_API_URL` | Override the API base URL (default: `https://brain.unisonlabs.ai`) |

## Quickstart

```sh
# Option 1: set the env var
export UNISON_TOKEN=usk_live_...
unisonfs mount ~/brain

# Option 2: interactive login
unisonfs login
unisonfs mount ~/brain

# Option 3: headless provisioning (no browser)
unisonfs provision --email agent@example.com
# enter the OTP from your email
unisonfs mount ~/brain
```

## Brain virtual tree

The mount exposes the Unison brain namespace as a directory tree:

```
~/brain/
  private/           — your private notes and files
    notes/           — default namespace for /private/notes/*.md
  workspace/         — visible to your whole workspace/org
    people/
    projects/
    teams/
      eng/           — team-scoped documents
      marketing/
  system/            — read-only synthesized views
    search/
      semantic/      — virtual semantic search (read any .md path here)
```

**Writable roots:** `/private/`, `/workspace/`

**Read-only roots:** `/system/`

All documents must end in `.md`.

## Commands

```
unisonfs login                  one-time auth, stores token
unisonfs whoami                 show current user, workspace, API endpoint
unisonfs mount <path>           mount the brain at <path>
unisonfs unmount <path>         unmount and drain pending writes
unisonfs list                   show all running mounts
unisonfs status <tag>           daemon health and push queue depth
unisonfs logs <tag>             tail the daemon log
unisonfs sync <tag>             force a sync cycle now
unisonfs grep "query" [path]    semantic search across the brain
unisonfs init                   install the sgrep shell wrapper
unisonfs logout                 remove stored credentials
unisonfs provision              headless account creation (machine-auth)
```

Run `unisonfs --help` or `unisonfs <command> --help` for full flag listings.

## Mount flags

```
--backend fuse|nfs       defaults: fuse on Linux, nfs on macOS
--foreground             run in foreground instead of detaching
--ephemeral              in-memory cache; nothing persists after unmount
--clean                  wipe local cache before mounting
--sync-interval <secs>   pull interval, default 30
--no-sync                disable the pull side; writes still push
--drain-timeout <secs>   max wait at unmount to drain push queue, default 30
--token <KEY>            API token (otherwise from UNISON_TOKEN / config)
--api-url <URL>          override API base URL
--tag <TAG>              override the daemon tag (derived from path basename)
```

## Semantic search

Run `unisonfs init` once. After that, `sgrep` anywhere searches the brain semantically:

```sh
sgrep "OAuth decisions"           # semantic: finds notes about the topic
sgrep "design review" workspace/     # scoped to a path
sgrep --literal "exact string"    # regex grep over document bodies
```

If you need to search from outside a mount:

```sh
unisonfs grep "query"
```

## Machine-auth (headless, no browser)

```sh
# Create a new account — mints an unverified usk_ key immediately
unisonfs provision --email agent@example.com

# Enter the OTP from the email to make the account durable
# (unverified accounts expire after 72 hours)

# Recover an existing account's key
unisonfs provision --request-key --email agent@example.com
```

The emailed OTP verification flow matches the Unison brain's machine-auth spec exactly:
`POST /v1/auth/provision` → email OTP → `POST /v1/auth/verify`.

## Architecture

```
unisonfs (CLI binary)
└── unisonfs-core (library)
    ├── api/          — typed HTTP client for /v1/brain/* + /v1/auth/*
    ├── cache/        — SQLite-backed VFS (Db + UnisonFs)
    │   ├── db.rs     — push queue, inode metadata, remote path mapping
    │   ├── file.rs   — chunked file read/write via SQLite
    │   └── fs.rs     — FileSystem trait implementation
    ├── vfs/          — FileSystem + File traits, MemFs reference impl
    ├── mount/        — FUSE adapter (Linux) + NFS adapter (macOS/Linux)
    ├── sync/         — pull loop (delta reconcile) + push loop (queue drain)
    ├── daemon/       — pid files, unix socket IPC, protocol
    └── config/       — XDG paths + credential storage
```

**Write path:** editor writes a file → FUSE/NFS delivers the write to `UnisonFs` → SQLite cache updated → `dirty_since` stamped → push queue entry upserted → push loop wakes up → `PUT /v1/brain/doc` sent → success clears the queue row.

**Read path:** first `open()` → SQLite cache hit → bytes returned. Background pull loop syncs remote changes every 30 seconds; `dirty_since` prevents overwriting locally-edited files.

**Sync safety:** optimistic concurrency via `expectedContentHash` (hex-16) on writes; server returns 409 on stale hash.

## Docker

```sh
docker build -t unisonfs:dev .
docker run --rm -it \
  --device /dev/fuse \
  --cap-add SYS_ADMIN \
  -e UNISON_TOKEN="$UNISON_TOKEN" \
  unisonfs:dev mount /mnt/brain
```

## Build from source

```sh
cargo build --release
./target/release/unisonfs --help
```

## Star history

If this is useful, a star helps others find it.

<div align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=unison-labs-ai/unison-fs&type=Date)](https://star-history.com/#unison-labs-ai/unison-fs&Date)

</div>

## License

MIT. See [`LICENSE`](LICENSE).

---

## Part of the Unison Labs constellation

**One brain, every agent.** Every repo below reads from _and writes to_ the same [Unison brain](https://unisonlabs.ai) — no per-tool memory silos.

| Repo | What it does |
|---|---|
| [unison-brain](https://github.com/unison-labs-ai/unison-brain) | CLI · SDK · MCP server — the core |
| [claude-unison](https://github.com/unison-labs-ai/claude-unison) | Memory for Claude Code |
| [cursor-unison](https://github.com/unison-labs-ai/cursor-unison) | Memory for Cursor |
| [codex-unison](https://github.com/unison-labs-ai/codex-unison) | Memory for OpenAI Codex CLI |
| [opencode-unison](https://github.com/unison-labs-ai/opencode-unison) | Memory for OpenCode |
| [openclaw-unison](https://github.com/unison-labs-ai/openclaw-unison) | Memory for OpenClaw |
| [pipecat-unison](https://github.com/unison-labs-ai/pipecat-unison) | Memory for Pipecat voice agents |
| [python-sdk](https://github.com/unison-labs-ai/python-sdk) | Python SDK for the brain |
| [install-mcp](https://github.com/unison-labs-ai/install-mcp) | One-command MCP installer |
| [code-chunk](https://github.com/unison-labs-ai/code-chunk) | AST-aware code chunking |
| **[unison-fs](https://github.com/unison-labs-ai/unison-fs)** | **Mount the brain as a filesystem ← you are here** |
| [backchannel](https://github.com/unison-labs-ai/backchannel) | Async messaging between agents |
| [Unison-evals](https://github.com/unison-labs-ai/Unison-evals) | Open memory benchmark suite |
