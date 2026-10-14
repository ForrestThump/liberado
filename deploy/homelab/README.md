# Homelab deployment

Liberado owns deployment policy in Rust. Host-specific values live in an untracked TOML file.
Copy [`config.example/ops.toml`](../../config.example/ops.toml) to `.liberado/ops.toml`, then set
the SSH target, API URL, remote paths, image, and container names.

## Try a PR image (GHCR)

GitHub Actions builds the deploy image from this repo's `Dockerfile` and publishes it to
`ghcr.io/forrestthump/liberado`. Tags include `sha-<full-commit>` on every built commit, `pr-<n>`
on pull requests, and `main` on the default branch. The `main` tag appears only after CI runs on
the merged branch; until then, only `sha-<commit>` and `pr-<n>` exist. The operator path on the
homelab is:

```bash
git fetch origin
git checkout <this-pr-branch>
./deploy/homelab/setup.sh
```

> Check out the **branch head**, not the merge ref. The Actions job stamps and builds
> `github.event.pull_request.head.sha`, and `setup.sh` pulls `sha-$(git rev-parse HEAD)`. Using
> `refs/pull/<n>/merge` computes a different HEAD, so the pull fails as a likely-404.

`setup.sh` pulls `ghcr.io/forrestthump/liberado:sha-<HEAD>`, recreates the existing Compose
service at `~/homelab/services/liberado`, and refuses to write `config/` or `.env`. It is safe
to run again. After recreation it waits for the container healthcheck to report `healthy`
(budget: `LIBERADO_HEALTH_TIMEOUT_SECS`, default 180s; skip with `--no-wait` or
`LIBERADO_NO_WAIT=1`) and verifies the live build SHA. If the image is missing or the pull is
unauthorized, it exits with the next step (wait for the Actions job, or fix package visibility /
`docker login`).

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

### Roll back

`setup.sh` never rewrites the host file, which still names `image: liberado:dev`. That local image
stays cached on the box (the fallback build writes the same tag), so rolling back is a plain
recreate without the GHCR overlays:

```bash
docker compose --project-directory ~/homelab/services/liberado \
  -f ~/homelab/services/liberado/docker-compose.yml \
  up -d --force-recreate --no-build --pull never
```

That reverts to the local `liberado:dev` image (build it first with `just deploy-homelab` if it is
absent) and to the host-mounted WebUI at `/webui`, because the webui overlay no longer sets
`LIBERADO_WEBUI_DIST`.

Override the Compose project with `LIBERADO_HOMELAB_DIR` if it is not `~/homelab/services/liberado`.

## On-box build (fallback)

`just deploy-homelab` still archives a committed Git ref over SSH and builds `liberado:dev` on the
box. Use that path when GHCR is unreachable or you want a local image without Actions. It does not
pull GHCR and it does not run `setup.sh`. This is also the rollback path after a `setup.sh`
deploy — see [Roll back](#roll-back).

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

## Matrix room

Matrix is a second chat next to Telegram. It stays off until the five variables below are set.
With none of them set, the daemon does not listen and does not log past one debug line.
Telegram is unchanged, and it remains the channel for background cards.

The homeserver is Tuwunel. One application service drives the existing user
`@liberado:matrix.homelab.local` by masquerade: every Client-Server call sends the
application-service token and `user_id=@liberado:…`. Tuwunel allows that when the user id
matches the registration namespace. The account can already exist. `LIBERADO_MATRIX_MAIN_USER`
is the localpart (default `liberado`). A full user id is kept as written.

The main room is the existing `#liberado` room. Put its room id in `LIBERADO_MATRIX_MAIN_ROOM`.
An alias is resolved at startup. The daemon joins when `@liberado` is not already in the room.
It does not create that room. Rooms it creates later lock encryption off. This room must stay
unencrypted. Do not send `m.room.encryption`. Element X shows an unencrypted badge. That is expected.

### Tokens and registration

Generate two different tokens on the homelab host:

```bash
openssl rand -hex 32
openssl rand -hex 32
```

Put the first in `as_token` and `LIBERADO_MATRIX_AS_TOKEN`. Put the second in `hs_token` and
`LIBERADO_MATRIX_HS_TOKEN`. The example file is
[`matrix/liberado-appservice.yaml.example`](matrix/liberado-appservice.yaml.example).
`id` is `liberado`. Set it. Do not rely on the file name.

Register it in the Tuwunel admin room. That is the default. Send:

```text
!admin appservices register
```

Then send the YAML in a code block. The same fields can live in `tuwunel.toml` under
`[global.appservice.liberado]`. Set `id` there too. Restart Tuwunel after a file edit.

Tuwunel matches an unanchored expression. The example anchors both patterns:

```text
^@liberado(_[a-z0-9._=-]+)?:matrix\.homelab\.local$
^#liberado_.*:matrix\.homelab\.local$
```

The registration `url` is `http://liberado:4202`. Both containers are on the external `homelab`
network, so that name resolves. If Tuwunel still cannot resolve `liberado`, set
`dns_passthru_appservices = true` in the Tuwunel config. Do not publish port 4202 on the host.
The sample Compose file uses `expose: ["4202"]` only. Do not add `ports:` for 4202, and do not
add `EXPOSE 4202` to the Dockerfile.

### Variables

Set these on the liberado service. Secrets stay in the host environment, not in `config.toml`.

| Variable | Required | Purpose |
|---|---|---|
| `LIBERADO_MATRIX_HOMESERVER` | yes | Base URL, `http://tuwunel:8008` |
| `LIBERADO_MATRIX_AS_TOKEN` | yes | Token the daemon sends to Tuwunel |
| `LIBERADO_MATRIX_HS_TOKEN` | yes | Token Tuwunel sends to the daemon |
| `LIBERADO_MATRIX_OWNER` | yes | Only this user is acted on, `@shiloh:matrix.homelab.local` |
| `LIBERADO_MATRIX_MAIN_ROOM` | yes | Room id, or an alias that resolves at startup |
| `LIBERADO_MATRIX_SERVER_NAME` | no | Defaults to the domain in the owner id |
| `LIBERADO_MATRIX_MAIN_USER` | no | Localpart, default `liberado` |
| `LIBERADO_MATRIX_AS_LISTEN` | no | Default `0.0.0.0:4202` |
| `LIBERADO_MATRIX_AS_ID` | no | Default `liberado` |
| `LIBERADO_MATRIX_SENDER_LOCALPART` | no | Default `liberado_bridge`. Not one of the five. |

If some of the five are set and some are missing, the daemon logs one warning that names the
missing variables and leaves Matrix off. A failed whoami or join does the same. The rest of
the daemon keeps running.

Processed transaction ids are stored in `<data_dir>/matrix-txns.json`. A restart does not
replay them.

### Agent direct chats

Each Agents-shelf agent gets its own Matrix user and a private room with the owner. The
Agent Creator session does not: that chat only makes agents. The main room stays
`@liberado` in `#liberado`.

The user id is `@liberado_<slug>:<server>`. The slug comes from the agent's title, then from
the session id when the title is blank. It is lowercase, and every other character becomes
`_`. `pings` and `bridge` are reserved, so those titles become `pings_2` and `bridge_2`.
The slug is written to `<data_dir>/matrix-agents.json` before the homeserver calls, and a
rename does not change it. The display name and the room name do follow the title.

The room is `private_chat`, `is_direct`, and invites the owner. `m.room.encryption` needs
power level 100. The owner is 50. When the homeserver lists room version 12 in
`m.room_versions.available`, the room is created at that version and the puppet is left
out of `users`, because the creator's power is unbounded. Older servers list the puppet
at 100. The puppet does not turn encryption on. The puppet's `m.direct` account data
points at the room.

The room is bound to that agent's existing session (`matrix`, room id, puppet user id) in
`<data_dir>/channel-bindings.json`. The WebUI and the DM are the same conversation. Only
the configured owner is acted on. The daemon ignores its own puppets, unbound rooms, and
invites from anyone else.

In that DM, `status`, `model`, `stop`, `cancel`, and `help` act on this agent. `help` lists
only those. These are refused, because they would leave the agent or they have no meaning
in a DM:

- `new`, `session`, `sessions` — the DM stays on this agent.
- `fork` — that would move the binding off the agent.
- `join`, `spawn`, `back`, `goal`, `plan`, `explore` — those are goal surfaces.
- `quit`, `exit`, `clear`, `theme` — those need a local client.
- `profile` — the shelf profile is changed in the WebUI.

Provisioning runs at startup and again when an agent is created, renamed, or deleted. The
HTTP response does not wait on the homeserver. A second reconcile reuses the user and the
room. `M_USER_IN_USE` on register means the account is already there. A failed call is
logged and retried on the next reconcile. It is not fatal. With Matrix off, none of this
runs.

There is no archive API. `DELETE /api/conversations/{id}` removes the session. The puppet
then leaves the DM. The binding is kept with `"active": false`, and the directory keeps
the slug and the room id, so they stay reserved. If that same session id is an Agents-shelf
seat again, the puppet rejoins and re-invites the owner.

No appservice re-register is required. The user namespace already matches
`@liberado_<slug>`:

```text
^@liberado(_[a-z0-9._=-]+)?:matrix\.homelab\.local$
```

`@liberado_pings` is not created. The slug `pings` is reserved so a later notification DM
does not collide with an agent. Reminders stay on the reminder bot until that follow-up.

### Check

1. Recreate the liberado container after the variables and the `expose` line are in place.
2. In the Tuwunel admin room, `!admin appservices list` shows `liberado`.
   `!admin appservices show-config liberado` shows the registration, including the anchored patterns.
3. Tuwunel pings `http://liberado:4202/_matrix/app/v1/ping` with the homeserver token.
   A live registration answers `{}`.
4. In Element X, open `#liberado` and send `status`. Liberado replies with the status text.
   `status`, `/status`, and `!status` are the same command. `status of the build` is a normal
   message. `help` lists the commands.

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
