# Idea: a Matrix chat surface (self-hosted, sovereignty-aligned)

Status: idea / not scheduled. Captured 2026-07-21.

## Why

Telegram works today but is centralized — it leans on Telegram's servers, which cuts against
Liberado's dependency-sovereignty goal. Matrix is the natural sovereign replacement: an open
federated protocol you can self-host, so the human-input channel stops depending on a third party.

We don't need much new plumbing for it. After the `liberado-messaging` extraction, a new surface is
just `impl MessagingChannel` (duplex chat) and/or `impl Notifier` (one-way push). Nothing in the
approval bot or the face-agent chat bridge changes.

## How it maps onto the existing seam

The `MessagingChannel` trait fits Matrix's client-server API almost suspiciously well:

- `receive(&mut cursor)` ↔ the `/sync` long-poll. The `next_batch` token Matrix returns **is**
  exactly our opaque, channel-owned `cursor` — same contract as the Telegram `getUpdates` offset.
- `send_text` ↔ `PUT /rooms/{id}/send/m.room.message`.
- `edit_message` ↔ an `m.replace` relation (a message edit). Our decision-receipt pattern (edit the
  tapped message to "✅ Approved everywhere — …" and drop the buttons) translates 1:1.
- `request_reply` ↔ just send a prompt message; the human's reply comes back as the next `/sync`
  message event (optionally correlate via an `m.in_reply_to` relation).

### Buttons: use emoji reactions as the tap surface

Matrix has no native inline keyboard like Telegram's. The idiomatic equivalent is **reactions**:
post the request, seed the message with ✅ / 🔁 / ♾️ / ❌ reactions, and treat a reaction on one as
the `InboundEvent::Action` (map emoji → `once` / `session` / `everywhere` / `deny`). On decision,
`edit_message` (m.replace) stamps the receipt; optionally redact the seed reactions to "strip the
buttons." Honest trade-off: reactions are clumsier than a labeled button row, but they cover the
whole approval UX without a custom widget.

(There is also the MSC/"interactive elements" direction and Element's own extensions, but reactions
are the portable, works-today path.)

## Homeserver options (persistence matters here)

Pick the server by how you want to store data — this is the main axis:

| Server | Language | Persistence | Notes |
|---|---|---|---|
| **conduwuit** (maintained Conduit fork) | Rust | **Embedded RocksDB** (a dir of files on disk; LSM key-value store). *No* external DB — SQLite backend was dropped. | Single binary, lightest footprint. The "I don't want to run a database" option. |
| **Synapse** | Python | **Postgres** recommended (SQLite only for toy setups) | Reference server, most complete, heaviest. |
| **Dendrite** | Go | **Postgres** or SQLite | Matrix.org's lighter second-gen server, still developed. |

For *this* homelab (already has infra, fine with Postgres): **Synapse or Dendrite on the existing
Postgres** is probably the better fit than conduwuit — conduwuit's headline advantage (embedded
RocksDB, zero external DB) is worth less when you're happy to run Postgres anyway. conduwuit stays
attractive if minimal footprint / single-binary ever becomes the priority.

E2EE (megolm) is the real complexity in any of them. For a solo operator, start **unencrypted in a
private room** — fine for a one-human setup — and defer encryption.

## Complement, not competitor: `ntfy` for the notify half

Our architecture already splits `Notifier` (one-way, unattended push) from `MessagingChannel`
(duplex chat). `ntfy` is a single self-hosted Go binary that natively supports **action buttons on
push notifications**. So a clean mix is possible:

- `ntfy` as the sovereign **push/approval notifier** — Once/Session/Everywhere/Deny as real tappable
  buttons straight to a phone. Lowest-effort path to a fully self-hosted *approval* loop.
- Matrix as the duplex **free-form chat** surface.

## Rough build sketch (when scheduled)

1. `crates/matrix/` (or fold into a channels crate): a `MatrixChannel { homeserver, access_token,
   room_id, http }` implementing `MessagingChannel`.
2. `receive`: `GET /_matrix/client/v3/sync?since={cursor}&timeout=…`; walk `rooms.join.{room}.timeline`
   for `m.room.message` (→ `InboundEvent::Message`) and `m.reaction` (→ `InboundEvent::Action`,
   emoji→action). Set `cursor = next_batch`.
3. `send_text` / `send_with_actions` (seed reactions) / `edit_message` (m.replace) / `acknowledge`
   (no-op or a read receipt).
4. Compose it exactly where `TelegramNotifier` is wired today; the approval bot is unchanged.

## Open questions

- Reaction taps have no per-tap ack spinner to dismiss (unlike Telegram `callback_query`) — confirm
  the `acknowledge` no-op path reads cleanly.
- Do we want E2EE eventually? If yes, budget for a matrix crypto stack (matrix-rust-sdk) rather than
  raw client-server calls.
- Correlating a request message with its later reaction: reactions carry the target `event_id`, so
  map `event_id → proposal stem` (the same role Telegram `callback_data` plays today).

See also: `[[project_channels_and_interactivity]]` (three-channel model; human-input channel), and
the `liberado-messaging` crate docs for the trait contract.
