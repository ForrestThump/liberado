# Homelab deployment

Liberado owns deployment policy in Rust. Host-specific values live in an untracked TOML file.
Copy [`config.example/ops.toml`](../../config.example/ops.toml) to `.liberado/ops.toml`, then set
the SSH target, API URL, remote paths, image, and container names.

## Try a PR image (GHCR)

GitHub Actions builds the deploy image from this repo's `Dockerfile` and publishes it to
`ghcr.io/forrestthump/liberado`. Tags include `sha-<full-commit>` on every built commit, `pr-<n>`
on pull requests, and `main` on the default branch. The operator path on the homelab is:

```bash
git fetch origin
git checkout <this-pr-branch>
./deploy/homelab/setup.sh
```

`setup.sh` pulls `ghcr.io/forrestthump/liberado:sha-<HEAD>`, recreates the existing Compose
service at `~/homelab/services/liberado`, and refuses to write `config/` or `.env`. It is safe
to run again. If the image is missing or the pull is unauthorized, it exits with the next step
(wait for the Actions job, or fix package visibility / `docker login`).

The first GHCR package is often **private** even when the repository is public. Anonymous pulls
then look like a 404. One-time, with no token in git:

1. GitHub → Packages → `liberado` → Package settings → Change visibility → Public
2. Or: `printf '%s' "$GITHUB_TOKEN" | docker login ghcr.io -u ForrestThump --password-stdin`
   (token needs `read:packages`)

The host Compose file can keep `image: liberado:dev`. `setup.sh` applies
[`docker-compose.ghcr.yml`](docker-compose.ghcr.yml) only for that invoke. It does not rewrite the
host file. When the pulled image contains a baked WebUI, it also applies
[`docker-compose.ghcr-webui.yml`](docker-compose.ghcr-webui.yml) so `LIBERADO_WEBUI_DIST` points at
`/usr/share/liberado/webui` instead of an empty host mount.

Override the Compose project with `LIBERADO_HOMELAB_DIR` if it is not `~/homelab/services/liberado`.

## On-box build (fallback)

`just deploy-homelab` still archives a committed Git ref over SSH and builds `liberado:dev` on the
box. Use that path when GHCR is unreachable or you want a local image without Actions. It does not
pull GHCR and it does not run `setup.sh`.

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

GitHub Actions bakes the WASM bundle into the published image (`BAKE_WEBUI=1`). `setup.sh` serves
that bundle. The on-box `just deploy-homelab` build leaves baking off, so a host mount still
supplies the UI there.

Ship a newer host bundle without rebuilding the daemon image:

```bash
just deploy-webui-homelab
just deploy-webui-homelab --skip-build
```

The remote update uses `rsync --delete` into the existing directory. It does not rename the bind
mount directory, because Docker holds the original directory inode. The Rust command also rejects
bundles without an index, bundles without WASM, and WASM that contains debug sections.

After a GHCR `setup.sh` run, Compose may point `LIBERADO_WEBUI_DIST` at the baked path. To use a
host bundle again, recreate without the WebUI overlay (or set `LIBERADO_WEBUI_DIST=/webui` in the
host Compose environment — not by editing `config/`).

## Latency

```bash
just latency-homelab
just latency-homelab --json
```

The command reads the configured journal from the running container. The Rust cost crate calculates
the per-role p50, p95, maximum, time to first token, and token count.

## Configuration and image boundaries

Deployment ships code. The host's `topology.toml` and `policy.toml` remain mounted configuration.
`setup.sh` and `just deploy-homelab` do not overwrite those files. Change them on the host and
recreate the service when configuration changes. The sample
[`docker-compose.yml`](docker-compose.yml) is Docker-specific YAML because Compose requires that
format; operator targets and deployment behavior stay in `ops.toml`.

The vault mount must remain read-write. Liberado applies its path policy before a write, and the
daemon needs the mount to perform approved writes.

The checked-in compose file contains public placeholders only. Put real host paths and secrets in
the host-managed Compose environment. Do not commit them here.
