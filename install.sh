#!/usr/bin/env bash
set -euo pipefail

# unisonfs installer
# Usage: curl -fsSL https://raw.githubusercontent.com/unison-labs-ai/unison-fs/main/install.sh | bash

REPO="unison-labs-ai/unison-fs"
BIN="unisonfs"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)  os="linux" ;;
        Darwin) os="macos" ;;
        *)
            echo "Unsupported OS: $os" >&2
            exit 1
            ;;
    esac
    case "$arch" in
        x86_64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *)
            echo "Unsupported architecture: $arch" >&2
            exit 1
            ;;
    esac
    echo "${os}-${arch}"
}

install_from_release() {
    local platform version url tmpdir

    platform="$(detect_platform)"
    version="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name"' | head -1 | cut -d'"' -f4)"

    if [ -z "$version" ]; then
        echo "Could not determine latest release version." >&2
        echo "Falling back to building from source..." >&2
        install_from_source
        return
    fi

    url="https://github.com/${REPO}/releases/download/${version}/${BIN}-${platform}.tar.gz"
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    echo "Downloading unisonfs ${version} for ${platform}..."
    if ! curl -fsSL "$url" -o "${tmpdir}/${BIN}.tar.gz"; then
        echo "Download failed. Falling back to building from source..." >&2
        install_from_source
        return
    fi

    tar -xzf "${tmpdir}/${BIN}.tar.gz" -C "$tmpdir"
    mkdir -p "$INSTALL_DIR"
    install -m755 "${tmpdir}/${BIN}" "${INSTALL_DIR}/${BIN}"
    echo "Installed ${BIN} to ${INSTALL_DIR}/${BIN}"
}

install_from_source() {
    if ! command -v cargo &>/dev/null; then
        echo "cargo not found. Install Rust from https://rustup.rs/ and try again." >&2
        exit 1
    fi
    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    echo "Cloning repository..."
    git clone --depth=1 "https://github.com/${REPO}.git" "$tmpdir/unisonfs"
    echo "Building from source (this may take a few minutes)..."
    cargo build --release --manifest-path "${tmpdir}/unisonfs/Cargo.toml"
    mkdir -p "$INSTALL_DIR"
    install -m755 "${tmpdir}/unisonfs/target/release/${BIN}" "${INSTALL_DIR}/${BIN}"
    echo "Installed ${BIN} to ${INSTALL_DIR}/${BIN}"
}

ensure_path() {
    local rc_file
    case "${SHELL:-}" in
        */zsh)  rc_file="$HOME/.zshrc" ;;
        */fish) rc_file="$HOME/.config/fish/config.fish" ;;
        *)      rc_file="$HOME/.bashrc" ;;
    esac

    if [[ ":${PATH}:" != *":${INSTALL_DIR}:"* ]]; then
        echo "" >> "$rc_file"
        echo "export PATH=\"\$PATH:${INSTALL_DIR}\"" >> "$rc_file"
        echo "Added ${INSTALL_DIR} to PATH in ${rc_file}"
        echo "Restart your shell or run: export PATH=\"\$PATH:${INSTALL_DIR}\""
    fi
}

main() {
    echo "Installing unisonfs..."
    install_from_release
    ensure_path
    echo ""
    echo "Done! Run 'unisonfs --help' to get started."
    echo "Quick start:"
    echo "  unisonfs login"
    echo "  unisonfs mount ~/brain"
    echo "  unisonfs init   # install sgrep shell wrapper"
}

main
