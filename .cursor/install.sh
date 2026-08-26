#!/usr/bin/env bash
# Cloud Agent bootstrap for the Liberado workspace.
#
# Idempotent: safe to run on a clean checkout and again over cached state. It prepares everything
# `cargo build --locked --workspace` needs, which for this repo is three things the default image
# does not carry as-is:
#   1. OpenSSL + pkg-config headers (reqwest/native-tls links against them via openssl-sys).
#   2. gcc as the default C/C++ compiler. The base image points `cc` at clang 18, which lacks the
#      C++ headers `cxx` needs and crashes (LLVM ICE) compiling the `numkong` SIMD crate pulled in
#      via usearch -> turbovault-vector. The deploy image (rust:trixie) builds with gcc; matching
#      that here fixes both. gcc/g++ ship in the base image and are already registered alternatives.
#   3. The `turbovault` and `turbomcp` sibling checkouts. They are gitignored path dependencies
#      expected *inside* this repo; without them cargo fails at manifest resolution. The exact refs
#      live in `.github/actions/checkout-siblings/action.yml` — the same file CI uses — so this
#      script reads them from there instead of hardcoding, keeping the two in lockstep.
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

# --- 3. Sibling path dependencies ---------------------------------------------------------------
# Parse "<owner>/<repo> <ref>" pairs straight from the CI checkout action so this never drifts from
# what GitHub builds against.
ACTION=".github/actions/checkout-siblings/action.yml"
sync_sibling() {
    local repo="$1" ref="$2" dir="$3"
    if [ -d "$dir/.git" ]; then
        # Already at the pinned ref (e.g. restored from a snapshot)? Do nothing — no network,
        # no mtime churn, so a warm rebuild stays incremental.
        if [ "$(git -C "$dir" rev-parse HEAD 2>/dev/null || true)" = "$ref" ]; then
            log "$dir already at $ref"
            return 0
        fi
        log "Updating $dir -> $ref"
        git -C "$dir" fetch --quiet origin "$ref" 2>/dev/null \
            || git -C "$dir" fetch --quiet origin 2>/dev/null || true
    else
        log "Cloning $repo -> $dir"
        rm -rf "$dir"
        git clone --quiet "https://github.com/$repo" "$dir"
    fi
    git -C "$dir" checkout --quiet "$ref"
}

if [ -f "$ACTION" ]; then
    mapfile -t _repos < <(grep -E 'repository:' "$ACTION" | sed 's/.*repository:[[:space:]]*//')
    mapfile -t _refs  < <(grep -E '^[[:space:]]*ref:' "$ACTION" | sed 's/.*ref:[[:space:]]*//')
    for i in "${!_repos[@]}"; do
        sync_sibling "${_repos[$i]}" "${_refs[$i]}" "$(basename "${_repos[$i]}")"
    done
else
    log "WARNING: $ACTION missing; falling back to pinned refs"
    sync_sibling "ForrestThump/turbovault" "bf9f0baf4a91d9e0718f2b8a3112ca0491bf145f" "turbovault"
    sync_sibling "ForrestThump/turbomcp"   "d5d9a9f8321f07b9693e9869b8afebfecc049e71" "turbomcp"
fi

# --- 4. Toolchain -------------------------------------------------------------------------------
# rust-toolchain.toml pins 1.94.1. If rustup governs this image it materializes the toolchain;
# a non-rustup cargo (as on the base image) already resolves to the pinned version and this is a
# harmless no-op.
if command -v rustup >/dev/null 2>&1; then
    log "Ensuring pinned Rust toolchain"
    rustup show >/dev/null || true
fi

# --- 5. Command runner ---------------------------------------------------------------------------
# `just` drives every dev/CI recipe (`just ci`, `just preflight`, ...). Install it into the writable
# cargo bin dir if absent.
if ! command -v just >/dev/null 2>&1; then
    log "Installing just"
    curl --proto '=https' --tlsv1.2 -sSf https://just.systems/install.sh \
        | bash -s -- --to "${CARGO_HOME:-$HOME/.cargo}/bin"
fi

# --- 6. Warm the build --------------------------------------------------------------------------
# Verifies the locked graph resolves with the siblings present and primes the target dir so the
# agent's first `cargo test`/`clippy` is fast.
log "Building the workspace (cargo build --locked --workspace)"
cargo build --locked --workspace

log "Done."
