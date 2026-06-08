-- unisonfs local cache schema.

-- Inode metadata. Every file, directory, and symlink gets a row here.
-- ino is AUTOINCREMENT so inode numbers are never reused.
-- dirty_since: epoch-ms when the user last wrote this inode locally; pull reconciler
-- skips an inode whose dirty_since is newer than the remote updatedAt.
CREATE TABLE IF NOT EXISTS fs_inode (
    ino          INTEGER PRIMARY KEY AUTOINCREMENT,
    mode         INTEGER NOT NULL,
    nlink        INTEGER NOT NULL DEFAULT 0,
    uid          INTEGER NOT NULL DEFAULT 0,
    gid          INTEGER NOT NULL DEFAULT 0,
    size         INTEGER NOT NULL DEFAULT 0,
    atime        INTEGER NOT NULL,
    mtime        INTEGER NOT NULL,
    ctime        INTEGER NOT NULL,
    rdev         INTEGER NOT NULL DEFAULT 0,
    atime_nsec   INTEGER NOT NULL DEFAULT 0,
    mtime_nsec   INTEGER NOT NULL DEFAULT 0,
    ctime_nsec   INTEGER NOT NULL DEFAULT 0,
    dirty_since  INTEGER,
    derived      INTEGER NOT NULL DEFAULT 0
);

-- Directory entries: maps (parent_ino, name) → child ino.
CREATE TABLE IF NOT EXISTS fs_dentry (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    parent_ino INTEGER NOT NULL,
    ino        INTEGER NOT NULL,
    UNIQUE(parent_ino, name)
);
CREATE INDEX IF NOT EXISTS idx_dentry_parent ON fs_dentry(parent_ino, name);

-- Chunked file data. Files are split into fixed-size chunks (default 4096).
CREATE TABLE IF NOT EXISTS fs_data (
    ino         INTEGER NOT NULL,
    chunk_index INTEGER NOT NULL,
    data        BLOB    NOT NULL,
    PRIMARY KEY (ino, chunk_index)
);

-- Symlink targets.
CREATE TABLE IF NOT EXISTS fs_symlink (
    ino    INTEGER PRIMARY KEY,
    target TEXT NOT NULL
);

-- Key-value configuration (chunk_size, schema_version, etc.).
CREATE TABLE IF NOT EXISTS fs_config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Remote brain path tracking. Maps local inode → Unison brain document path.
-- Populated on first successful push and on pull reconciliation.
CREATE TABLE IF NOT EXISTS fs_remote (
    ino                  INTEGER PRIMARY KEY,
    remote_path          TEXT    NOT NULL,
    mirrored_updated_at  INTEGER,
    last_status          TEXT,
    last_status_at       INTEGER
);
CREATE INDEX IF NOT EXISTS idx_remote_path ON fs_remote(remote_path);

-- Persistent push queue. One row per brain_path enforces latest-wins coalescing.
CREATE TABLE IF NOT EXISTS push_queue (
    brain_path           TEXT PRIMARY KEY,
    op                   TEXT NOT NULL,
    content_ino          INTEGER,
    rename_to            TEXT,
    inflight_started_at  INTEGER,
    pending_op           TEXT,
    pending_content_ino  INTEGER,
    pending_rename_to    TEXT,
    last_error           TEXT,
    attempt              INTEGER NOT NULL DEFAULT 0,
    updated_at           INTEGER NOT NULL,
    poisoned             INTEGER NOT NULL DEFAULT 0,
    last_status          INTEGER
);
CREATE INDEX IF NOT EXISTS idx_push_queue_updated ON push_queue(updated_at);

-- General KV for sync timestamps.
--   last_pull_at — watermark for delta pull
CREATE TABLE IF NOT EXISTS sync_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
