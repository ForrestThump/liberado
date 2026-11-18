FROM rust:latest AS chef
WORKDIR /app
# Install cargo-chef for dependency caching
RUN cargo install cargo-chef
# Copy manifests and source (context is reduced via .dockerignore)
COPY . .
# Compute the dependency graph recipe
RUN cargo chef prepare --recipe-path recipe.json

FROM rust:latest AS builder
WORKDIR /app
RUN cargo install cargo-chef
# Leverage cached dependency layers
COPY --from=chef /app/recipe.json recipe.json
RUN cargo chef cook --release --workspace --recipe-path recipe.json
# Now copy the full source and build
COPY . .
# Enable sccache for faster rebuilds
RUN cargo install sccache && \
    echo '[build]\nrustc-wrapper = "sccache"' >> /usr/local/cargo/config.toml && \
    sccache --version
# Build workspace; skip macro crate to avoid rebuilding proc-macros unnecessarily in runtime image
RUN cargo build --release --workspace --exclude turbomcp --exclude turbomcp-macros

# Runtime image: run server binary
FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata curl git adduser \
    && adduser --system --group --no-create-home mcp \
    && rm -rf /var/lib/apt/lists/*
USER mcp:mcp
WORKDIR /home/mcp
ENV RUST_LOG=info
# Copy compiled turbomcp-server binary
COPY --from=builder /app/target/release/turbomcp-server /usr/local/bin/turbomcp-server
ENTRYPOINT ["/usr/local/bin/turbomcp-server"]

