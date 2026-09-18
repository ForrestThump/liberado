# Specs & decisions

Detailed design specs and the architecture decision log. Prefer [architecture/](architecture/README.md) for the current narrative; these files hold depth and decision history.

| Spec | Topic |
|------|--------|
| [decisions/](../decisions/README.md) | Architecture Decision Records (ADR-0001…); stub at [architecture-decisions.md](../decisions/README.md) |
| [liberado-architecture.md](liberado-architecture.md) | Early Liberado architecture writeup |
| [config-spec.md](config-spec.md) | Config loading / validation |
| [dispatch-logic-spec.md](dispatch-logic-spec.md) | Dispatcher / decision shape |
| [conversation-store-spec.md](conversation-store-spec.md) | Conversation / session store (D17) |
| [context-policy-spec.md](context-policy-spec.md) | Context policy |
| [inbox-spec.md](inbox-spec.md) | Inbox |
| [testing-and-eval-spec.md](testing-and-eval-spec.md) | Testing & eval |
| [vault-concurrency-spec.md](vault-concurrency-spec.md) | Vault concurrency |
| [vault-maintenance-spec.md](vault-maintenance-spec.md) | Vault maintenance / git |

If a spec conflicts with code or with [architecture/](architecture/README.md), follow **code + architecture living docs**. The authority model in [reference/doc-authority.md](reference/doc-authority.md) is the conflict-resolution policy; historical open conflicts from the 2026-07 docs reorg are kept as record only in [../future-work/archive/project-design-questions-2026-07.md](../future-work/archive/project-design-questions-2026-07.md).
