# Liberado daemon — deploy image (P1: the automation daemon).
#
# Multi-stage: build the workspace with the official `rust` image, ship `liberado` and
# `liberado-conformance` on a slim Debian runtime. GitHub Actions also bakes the WASM WebUI into
# `/usr/share/liberado/webui` (`BAKE_WEBUI=1`) so the homelab can pull a complete image. The on-box
# `just deploy-homelab` path leaves `BAKE_WEBUI` at 0: that build stays daemon-only, and Compose can
# still mount a host bundle over `/webui`. With no bundle at `LIBERADO_WEBUI_DIST`, `serve` 404s the
# static route cleanly and the API still works.
#
# Building in this container IS the Debian shakeout: the Unix code paths that have only ever been
# compiled on Windows finally compile *and run* on the target platform, isolated where a bug can do no
# harm. Build/runtime are both Debian trixie so the binary's glibc matches the homelab exactly.

# ---- builder ----
FROM rust:1-trixie AS builder
WORKDIR /build

# Build-time system deps: openssl (reqwest/native-tls), pkg-config, git (build scripts + the coder
# pack shells out to it), curl (optional pinned Dioxus CLI download when BAKE_WEBUI=1).
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev git curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Cap parallel codegen so a release build of the full workspace does not spike past the box's RAM
# (the homelab has ~11 GiB; GitHub-hosted runners are often tighter). Override with
# `--build-arg CARGO_BUILD_JOBS=1` on those runners.
#
# Override the workspace's `lto = true, codegen-units = 1` release profile for the deploy build:
# LTO roughly triples build time and its final link is the RAM peak most likely to OOM an 11 GiB box.
# A daemon does not need LTO's marginal runtime win, and a fast, reliable first build matters more —
# re-enable it for a tagged "release" image later if ever worth it.
ARG CARGO_BUILD_JOBS=2
ENV CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS \
    CARGO_NET_GIT_FETCH_WITH_CLI=true \
    CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16

COPY . .

# `-p liberado-cli` pulls in the whole needed graph (server -> daemon -> every kernel/pack crate) and
# emits the `liberado` binary. Also ship `liberado-conformance` (Tier 3 live path runner) so the
# hand-run suite lives beside the daemon on the box without a second toolchain install. The WebUI is
# a separate `dx` build, so it is naturally excluded.
#
# The two cache mounts are what make a redeploy incremental. `COPY . .` above invalidates this
# layer on any source change, so without them every deploy recompiled 43 crates plus turbovault
# from scratch - measured at 888s (14.8 min) for a one-line change. The mounts persist the cargo
# registry and the target dir across builds, so only what actually changed is rebuilt.
#
# `sharing=locked` because two concurrent deploys sharing one target dir is a corrupted target
# dir; the second build waits instead.
#
# The binaries are copied to /out **inside this RUN**, and that is not incidental: a cache mount
# is not part of the image, so /build/target is empty again the moment this step ends. Copying
# out here is the only way the runtime stage can still find them - a
# `COPY --from=builder /build/target/...` would start failing the moment the mount was added.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release -p liberado-cli -p liberado-conformance \
    && mkdir -p /out/webui \
    && touch /out/webui/.keep \
    && cp target/release/liberado target/release/liberado-conformance /out/ \
    && (strip /out/liberado /out/liberado-conformance || true)

# Bake the WebUI after the daemon link so the two RAM peaks do not overlap. Default off: on-box
# `docker build` stays a daemon-only compile. GitHub Actions passes BAKE_WEBUI=1. The Dioxus CLI is a
# pinned GitHub release (hash-verified), not `cargo install`, so this stage does not compile `dx`.
ARG BAKE_WEBUI=0
ARG DX_VERSION=0.7.10
ARG DX_SHA256=4363e4ed2a3f1eb7f4d38d2d59aed59ce43271c44c16b425e92c89a64761fbe7
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    if [ "$BAKE_WEBUI" != "1" ]; then \
        echo "skipping WebUI bake (BAKE_WEBUI=$BAKE_WEBUI)"; \
    else \
        rustup target add wasm32-unknown-unknown \
        && curl -fsSL -o /tmp/dx.tar.gz \
            "https://github.com/DioxusLabs/dioxus/releases/download/v${DX_VERSION}/dx-x86_64-unknown-linux-gnu.tar.gz" \
        && echo "${DX_SHA256}  /tmp/dx.tar.gz" | sha256sum -c - \
        && tar -xzf /tmp/dx.tar.gz -C /tmp \
        && install -m 0755 /tmp/dx /usr/local/bin/dx \
        && dx build -r -p liberado-webui --web \
        && cp -a target/dx/liberado-webui/release/web/public/. /out/webui/ \
        && test -f /out/webui/index.html; \
    fi

# ---- runtime ----
FROM debian:trixie-slim AS runtime

# Runtime deps: TLS roots (outbound provider/MCP HTTPS), openssl runtime, git (coder pack; harmless
# for the automation daemon), and a shell for healthchecks.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 git curl \
    && rm -rf /var/lib/apt/lists/*

# From /out, not /build/target - the target dir is a cache mount and does not survive its RUN.
COPY --from=builder /out/liberado /usr/local/bin/liberado
COPY --from=builder /out/liberado-conformance /usr/local/bin/liberado-conformance
COPY --from=builder /out/webui /usr/share/liberado/webui

# Build provenance — the single answer to "what commit is actually running?".
# `liberado deploy homelab` passes the deployed Git SHA here; it lands both in a file the daemon
# container can print and in an image LABEL. Read it without guessing:
#   docker exec liberado cat /etc/liberado-build-sha
#   docker inspect -f '{{ index .Config.Labels "org.liberado.git-sha" }}' liberado:dev
# Defaults to "unknown" for a bare `docker build` with no --build-arg (i.e. someone bypassed the script).
ARG GIT_SHA=unknown
RUN printf '%s\n' "$GIT_SHA" > /etc/liberado-build-sha
LABEL org.liberado.git-sha="$GIT_SHA"

# Config, data, and the vault are all mounts (see the compose service). Config and data are named so
# the daemon finds them without flags; the vault path is set in topology.toml.
ENV LIBERADO_CONFIG_DIR=/config \
    LIBERADO_DATA_DIR=/data \
    LIBERADO_PORT=4201 \
    LIBERADO_BUILD_SHA=$GIT_SHA \
    LIBERADO_WEBUI_DIST=/usr/share/liberado/webui

EXPOSE 4201

# `serve` with an empty vault arg falls back to topology.vault_path (= /vault, mounted read-write).
ENTRYPOINT ["liberado"]
CMD ["serve"]
