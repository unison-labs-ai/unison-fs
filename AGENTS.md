# AGENTS.md

Guidance for AI agents. This file covers two jobs — jump to yours:

- **Mount and use unisonfs** — you're an agent that needs the brain as a local
  directory → [Install and mount](#install-and-mount)
- **Contribute to this repo** — you're changing unisonfs code →
  [Working in this repo](#working-in-this-repo)

Follows the [AGENTS.md](https://agents.md/) convention. Human contributors: see
[`CONTRIBUTING.md`](./CONTRIBUTING.md).

---

## Install and mount

unisonfs mounts the [Unison brain](https://unisonlabs.ai) as a real local
directory. On Linux it uses FUSE; on macOS it uses an embedded NFS server — no
kernel extension or macFUSE required.

### 1. Install

```bash
cargo install --path crates/unisonfs
# or from the install script:
curl -fsSL https://raw.githubusercontent.com/unison-labs-ai/unison-fs/main/install.sh | bash
```

Requires Rust 1.80+. Confirm: `unisonfs --version`.

### 2. Authenticate

Set `UNISON_TOKEN` to your `usk_...` API key. This overrides any stored config.

```bash
export UNISON_TOKEN=usk_live_...
export UNISON_API_URL=https://brain.unisonlabs.ai   # optional; this is the default
```

To store credentials interactively:

```bash
unisonfs login
```

For headless / CI provisioning (mints a new key, no browser):

```bash
unisonfs provision --email agent@example.com
# enter the OTP from the email to make the account durable
```

### 3. Mount

```bash
unisonfs mount ~/brain
```

The mount exposes the brain namespace as directories:

```
~/brain/
  private/           — your private notes and files
  tenant/            — your whole tenant/org
  teams/<slug>/      — team-scoped documents
  system/            — read-only synthesized views
```

All documents must end in `.md`. Writable roots: `/private/`, `/tenant/`,
`/teams/<slug>/`. Read-only: `/system/`.

### 4. Use it

```bash
# Write
echo "# Decision" > ~/brain/private/notes/my-decision.md

# Read
cat ~/brain/private/notes/my-decision.md

# Browse
ls ~/brain/private/notes/

# Semantic search
unisonfs grep "auth decisions"

# Daemon status
unisonfs status

# Unmount (drains push queue first)
unisonfs unmount ~/brain
```

### Key flags

```
unisonfs mount ~/brain \
  --sync-interval 30   \   # pull interval in seconds (default 30)
  --no-sync            \   # disable pull; writes still push
  --ephemeral              # in-memory cache; nothing persists after unmount
```

### Environment variables

| Variable | Description |
|---|---|
| `UNISON_TOKEN` | `usk_live_...` API key (required) |
| `UNISON_API_URL` | Override API base URL (default: `https://brain.unisonlabs.ai`) |

---

## Working in this repo

unisonfs is a Cargo workspace with two crates:

- `crates/unisonfs-core` — the library: API client, SQLite cache, VFS, sync loops, mount adapters.
- `crates/unisonfs` — the CLI binary: thin dispatch over unisonfs-core.

### Build, test, lint (run before every PR)

```bash
cargo build --release         # must succeed with zero errors
cargo test                    # must pass
cargo clippy -- -D warnings   # must be clean
```

CI runs all three on every pull request to main.

### Key source files

| Path | What it does |
|---|---|
| `crates/unisonfs-core/src/api/mod.rs` | Typed HTTP client for all brain REST endpoints |
| `crates/unisonfs-core/src/cache/db.rs` | SQLite push queue, inode metadata, dirty-tracking |
| `crates/unisonfs-core/src/cache/fs.rs` | `FileSystem` trait implementation (UnisonFs) |
| `crates/unisonfs-core/src/sync/pull.rs` | Pull loop: reconciles remote docs into local cache |
| `crates/unisonfs-core/src/sync/push.rs` | Push loop: drains queue, sends writes to brain |
| `crates/unisonfs-core/src/mount/fuse.rs` | FUSE adapter (Linux) |
| `crates/unisonfs-core/src/mount/nfs.rs` | NFS adapter (macOS / cross-platform) |

### Sync safety — do not break

The dirty-tracking guard is the core correctness invariant:

1. Any local write stamps `dirty_since` on the inode (epoch ms).
2. The pull loop skips pulling a remote doc if `dirty_since >= remote_updated_at`,
   preventing overwrite of in-progress local edits.
3. On a successful push, `dirty_since` is cleared so future pulls can update the inode.

This is wired through `db.set_dirty_since()` / `db.get_dirty_since()` and must
remain coherent. Never bypass the push queue with a direct write that skips step 1.

### PRs

One logical change per PR. Update `CHANGELOG.md` under "Unreleased". Never push
directly to `main` (protected — open a PR). Security issues: see [`SECURITY.md`](./SECURITY.md).
