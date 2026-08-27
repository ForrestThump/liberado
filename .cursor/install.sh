#!/usr/bin/env bash
# Cloud Agent bootstrap for the Liberado workspace.
#
# Idempotent: safe to run on a clean checkout and again over cached state. It prepares everything
# `cargo build --locked --workspace` needs, which for this repo is two things the default image
# does not carry as-is:
#   1. OpenSSL + pkg-config headers (reqwest/native-tls links against them via openssl-sys).
#   2. gcc as the default C/C++ compiler. The base image points `cc` at clang 18, which lacks the
#      C++ headers `cxx` needs and crashes (LLVM ICE) compiling the `numkong` SIMD crate pulled in
#      via usearch -> turbovault-vector. The deploy image (rust:trixie) builds with gcc; matching
#      that here fixes both. gcc/g++ ship in the base image and are already registered alternatives.
#
# `turbovault*` and `turbomcp*` are git+tag pins. Cargo fetches tag `liberado-2026-08-27` from the
# public ForrestThump forks; nested sibling clones are not required.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

log() { printf '\n[install] %s\n' "$*"; }

# --- 1. System build dependencies ---------------------------------------------------------------
# The default image lacks the OpenSSL dev headers openssl-sys needs. git/curl are usually present;
# they are cheap to assert.
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then SUDO="sudo"; fi
fi
if command -v apt-get >/dev/null 2>&1; then
    log "Installing system build dependencies (pkg-config, libssl-dev, git, curl)"
    $SUDO apt-get update -qq
    $SUDO apt-get install -y --no-install-recommends pkg-config libssl-dev git curl
else
    log "apt-get not found; assuming pkg-config + OpenSSL dev headers are already present"
fi

# --- 2. C/C++ compiler ---------------------------------------------------------------------------
# Prefer gcc over the image's default clang for native (-sys) crates. Both are pre-registered
# alternatives; select gcc when it exists so `cc`/`c++` resolve to it for every later cargo build.
if command -v gcc >/dev/null 2>&1 && command -v update-alternatives >/dev/null 2>&1; then
    if update-alternatives --list cc 2>/dev/null | grep -q '/usr/bin/gcc'; then
        log "Selecting gcc as the default C/C++ compiler"
        $SUDO update-alternatives --set cc /usr/bin/gcc || true
        $SUDO update-alternatives --set c++ /usr/bin/g++ || true
    fi
fi

# --- 3. Toolchain -------------------------------------------------------------------------------
# rust-toolchain.toml pins 1.94.1. If rustup governs this image it materializes the toolchain;
# a non-rustup cargo (as on the base image) already resolves to the pinned version and this is a
# harmless no-op.
if command -v rustup >/dev/null 2>&1; then
    log "Ensuring pinned Rust toolchain"
    rustup show >/dev/null || true
fi

# --- 4. Command runner ---------------------------------------------------------------------------
# `just` drives every dev/CI recipe (`just ci`, `just preflight`, ...). Install it into the writable
# cargo bin dir if absent.
if ! command -v just >/dev/null 2>&1; then
    log "Installing just"
    curl --proto '=https' --tlsv1.2 -sSf https://just.systems/install.sh \
        | bash -s -- --to "${CARGO_HOME:-$HOME/.cargo}/bin"
fi

# --- 5. Warm the build --------------------------------------------------------------------------
# Verifies the locked graph resolves (Cargo fetches the git+tag forks) and primes the target dir
# so the agent's first `cargo test`/`clippy` is fast.
log "Building the workspace (cargo build --locked --workspace)"
cargo build --locked --workspace

log "Done."
