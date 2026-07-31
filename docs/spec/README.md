# Specs & decisions

Detailed design specs and the architecture decision log. Prefer [architecture/](spec/architecture/README.md) for the current narrative; these files hold depth and decision history.

| Spec | Topic |
|------|--------|
| [liberado-architecture-decisions.md](liberado-architecture-decisions.md) | Decision log (numbered) |
| [life-os-architecture.md](life-os-architecture.md) | Early Life OS architecture writeup |
| [liberado-config-spec.md](liberado-config-spec.md) | Config loading / validation |
| [liberado-dispatch-logic-spec.md](liberado-dispatch-logic-spec.md) | Dispatcher / decision shape |
| [liberado-conversation-store-spec.md](liberado-conversation-store-spec.md) | Conversation / session store (D17) |
| [liberado-context-policy-spec.md](liberado-context-policy-spec.md) | Context policy |
| [liberado-inbox-spec.md](liberado-inbox-spec.md) | Inbox |
| [liberado-testing-and-eval-spec.md](liberado-testing-and-eval-spec.md) | Testing & eval |
| [liberado-vault-concurrency-spec.md](liberado-vault-concurrency-spec.md) | Vault concurrency |
| [liberado-vault-maintenance-and-git-spec.md](liberado-vault-maintenance-and-git-spec.md) | Vault maintenance / git |

If a spec conflicts with code or with [architecture/](spec/architecture/README.md), follow **code + architecture living docs**, and log the conflict in [design_questions_for_the_user.md](../design_questions_for_the_user.md).
