# Liberado daemon — headless deploy image (P1: the automation daemon).
#
# Multi-stage: build the whole Rust workspace with the official `rust` image, ship only the
# `liberado` binary on a slim Debian runtime. The WASM WebUI is deliberately NOT in this image: it
# needs the wasm32 toolchain and the Dioxus CLI, which would roughly double the builder stage for a
# frontend that changes on a different cadence than the daemon. It is built on a dev machine and
# mounted in instead (`LIBERADO_WEBUI_DIST`, see deploy/homelab/docker-compose.yml). With no bundle
# mounted, `serve` 404s the static route cleanly and the API still works.
#
# Building in this container IS the Debian shakeout: the Unix code paths that have only ever been
# compiled on Windows finally compile *and run* on the target platform, isolated where a bug can do no
# harm. Build/runtime are both Debian trixie so the binary's glibc matches the homelab exactly.

# ---- builder ----
FROM rust:1-trixie AS builder
WORKDIR /build

# Build-time system deps: openssl (reqwest/native-tls), pkg-config, git (build scripts + the coder
# pack shells out to it).
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev git \
    && rm -rf /var/lib/apt/lists/*

# Cap parallel codegen so a release build of the full workspace does not spike past the box's RAM
# (the homelab has ~11 GiB; linking several crates at once is the peak).
#
# Override the workspace's `lto = true, codegen-units = 1` release profile for the deploy build:
# LTO roughly triples build time and its final link is the RAM peak most likely to OOM an 11 GiB box.
# A daemon does not need LTO's marginal runtime win, and a fast, reliable first build matters more —
# re-enable it for a tagged "release" image later if ever worth it.
ENV CARGO_BUILD_JOBS=2 \
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
    && mkdir -p /out \
    && cp target/release/liberado target/release/liberado-conformance /out/ \
    && (strip /out/liberado /out/liberado-conformance || true)

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

# Build provenance — the single answer to "what commit is actually running?".
# `deploy/homelab/deploy.sh` passes the deployed git SHA here; it lands both in a file the daemon
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
    LIBERADO_BUILD_SHA=$GIT_SHA

EXPOSE 4201

# `serve` with an empty vault arg falls back to topology.vault_path (= /vault, mounted read-write).
ENTRYPOINT ["liberado"]
CMD ["serve"]
