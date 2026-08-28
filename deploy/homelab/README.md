# Homelab deployment

Liberado owns deployment policy in Rust. Host-specific values live in an untracked TOML file.
Copy [`config.example/ops.toml`](../../config.example/ops.toml) to `.liberado/ops.toml`, then set
the SSH target, API URL, remote paths, image, and container names.

```bash
# Show every command without changing local or remote state.
just deploy-homelab --dry-run

# Deploy the committed HEAD. A dirty tree is rejected.
just deploy-homelab

# Deploy a named commit, tag, or branch.
just deploy-homelab --ref <git-ref>

# Verify status, build provenance, and in-container configuration.
just smoke-homelab --expected-sha <commit-sha>

# Add one paid chat request only when you mean to spend tokens.
just smoke-homelab --live-chat
```

The deploy command archives a committed Git ref, uploads it, takes a remote `flock`, refreshes the
build directory, builds the configured image, recreates the Compose service, and verifies the exact
live SHA. It does not deploy an uncommitted working tree.

## WebUI

The WebUI is a separate WASM bundle. Deploy it without rebuilding the daemon image:

```bash
just deploy-webui-homelab
just deploy-webui-homelab --skip-build
```

The remote update uses `rsync --delete` into the existing directory. It does not rename the bind
mount directory, because Docker holds the original directory inode. The Rust command also rejects
bundles without an index, bundles without WASM, and WASM that contains debug sections.

## Latency

```bash
just latency-homelab
just latency-homelab --json
```

The command reads the configured journal from the running container. The Rust cost crate calculates
the per-role p50, p95, maximum, time to first token, and token count.

## Configuration and image boundaries

Deployment ships code. The host's `topology.toml` and `policy.toml` remain mounted configuration.
Change those files on the host and recreate the service when configuration changes. The sample
[`docker-compose.yml`](docker-compose.yml) is Docker-specific YAML because Compose requires that
format; operator targets and deployment behavior stay in `ops.toml`.

The vault mount must remain read-write. Liberado applies its path policy before a write, and the
daemon needs the mount to perform approved writes.

The checked-in compose file contains public placeholders only. Put real host paths and secrets in
the host-managed Compose environment. Do not commit them here.
