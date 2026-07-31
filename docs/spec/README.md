# Specs & decisions

Detailed design specs and the architecture decision log. Prefer [architecture/](architecture/README.md) for the current narrative; these files hold depth and decision history.

| Spec | Topic |
|------|--------|
| [architecture-decisions.md](architecture-decisions.md) | Decision log (numbered) |
| [life-os-architecture.md](life-os-architecture.md) | Early Life OS architecture writeup |
| [config-spec.md](config-spec.md) | Config loading / validation |
| [dispatch-logic-spec.md](dispatch-logic-spec.md) | Dispatcher / decision shape |
| [conversation-store-spec.md](conversation-store-spec.md) | Conversation / session store (D17) |
| [context-policy-spec.md](context-policy-spec.md) | Context policy |
| [inbox-spec.md](inbox-spec.md) | Inbox |
| [testing-and-eval-spec.md](testing-and-eval-spec.md) | Testing & eval |
| [vault-concurrency-spec.md](vault-concurrency-spec.md) | Vault concurrency |
| [vault-maintenance-spec.md](vault-maintenance-spec.md) | Vault maintenance / git |

If a spec conflicts with code or with [architecture/](architecture/README.md), follow **code + architecture living docs**, and log the conflict in [design_questions.md](../project/design_questions.md).
