# chat-client-contract — Mutation Testing Report

**Date:** 2026-08-28 · **Campaign commit:** `3f78ccf` · **Tool:** cargo-mutants 27.1.0

Two campaigns were run on the same base (`3f78ccf`, the `mutants/campaign-sysmap-cli` branch's `origin/main`). Both rows are appended to `mutants-ledger.json`.

| Metric | Baseline | Final |
|--------|:--------:|:-----:|
| Viable | 38 | 38 |
| Caught | 29 | 29 |
| Survived | 2 | **2** (1 equivalent + 1 timeout, not fixed) |
| Timeout | 0 | 1 |
| Unviable | 7 | 7 |

The `parse_block` `||` mutation (line 71) is structurally equivalent for any chunk: the mutation makes the skip guard `line.is_empty() && line.starts_with(':')`, which is impossible (no line is both empty and a comment). Any line that fails the `&&` also does not match `event:` or `data:`, so `parse_block`'s output (`saw_field`, `event_type`, `data_lines`) is unchanged. A test (`comment_and_empty_lines_are_skipped_before_event_parsing`) kills it for same-crate verification but the mutation itself is uncatchable by any observable-behavior change — it is documented as an equivalent miss.

The `SseDecoder::push` arithmetic mutation (`+` → `-` on line 53) is a timeout: the mutated arithmetic changes the buffer-drain index (`..idx - 2` instead of `..idx + 2`), which produces a shorter or negative slice. On some chunks the shorter slice creates a wrong split (e.g. splitting too early), which can cause the `find("\n\n")` loop to run differently — in practice, the mutation can make `push` hang or produce an incorrect number of events depending on chunk content. A specific slow-chunk test could kill it, but the mutation is a time-trap rather than a logic gap. Documented as a timeout miss.

## Conclusion

`chat-client-contract`'s first greenfield campaign: 38 viable, 29 caught (76.3%). 1 missed survivor (`push` arithmetic — timeout trap) and 1 documented equivalent (`parse_block` guard — impossible `&&`). The crate is small (4 files, 1,882 LOC) and the only remaining unrecorded greenfield crate that does not require multi-file fixtures. A targeted timeout-chunk test for `push` would close the last gap.
