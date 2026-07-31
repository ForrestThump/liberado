# Specs & decisions

Detailed design specs and the architecture decision log. Prefer [architecture/](architecture/README.md) for the current narrative; these files hold depth and decision history.

| Spec | Topic |
|------|--------|
| [liberado-architecture-decisions.md](architecture-decisions.md) | Decision log (numbered) |
| [life-os-architecture.md](life-os-architecture.md) | Early Life OS architecture writeup |
| [liberado-config-spec.md](config-spec.md) | Config loading / validation |
| [liberado-dispatch-logic-spec.md](dispatch-logic-spec.md) | Dispatcher / decision shape |
| [liberado-conversation-store-spec.md](conversation-store-spec.md) | Conversation / session store (D17) |
| [liberado-context-policy-spec.md](context-policy-spec.md) | Context policy |
| [liberado-inbox-spec.md](inbox-spec.md) | Inbox |
| [liberado-testing-and-eval-spec.md](testing-and-eval-spec.md) | Testing & eval |
| [liberado-vault-concurrency-spec.md](vault-concurrency-spec.md) | Vault concurrency |
| [liberado-vault-maintenance-and-git-spec.md](vault-maintenance-spec.md) | Vault maintenance / git |

If a spec conflicts with code or with [architecture/](architecture/README.md), follow **code + architecture living docs**, and log the conflict in [design_questions_for_the_user.md](../project/design_questions.md).
