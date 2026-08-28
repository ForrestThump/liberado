# chat-client-contract — Mutation Testing Report

**Status:** historical
**Authority:** evidence
**Date:** 2026-08-28 · **Campaign commit:** `3104fae6a2da43bd08dea33caca60a7933984bdc` · **Tool:** cargo-mutants 27.1.0

The original branch row named base commit `3f78ccf9797573df0fd8d7c6285964e479b569a9`,
which did not contain its dirty-tree test. That row remains append-only history. Three reviewed
runs record the merged artifact, removal of an equivalent branch, and the final bounded drain.

| Metric | Original row | Merged rerun | Equivalent removed | Final |
|--------|:------------:|:------------:|:------------------:|:-----:|
| Total | 38 | 38 | 37 | 35 |
| Viable | 31 | 31 | 30 | 28 |
| Caught | 29 | 29 | 29 | **28** |
| Survived | 1 | 1 | **0** | **0** |
| Timeout | 1 | 1 | 1 | **0** |
| Unviable | 7 | 7 | 7 | 7 |

The original `parse_block` guard used `line.is_empty() || line.starts_with(':')`. Changing
`||` to `&&` did not change any result because neither empty lines nor comment lines matched an
SSE field prefix. The added test passed with and without that mutation and duplicated existing
comment and separator coverage. The final code removes both the ineffective test and the
unobservable `line.is_empty()` branch.

The original `SseDecoder::push` drained through `idx + 2`. Replacing `+` with `-` caused a
timeout, while replacing it with `*` was caught. The final code drains the event body through
`idx`, then drains the fixed two-byte separator. This keeps each loop iteration bounded and
removes both arithmetic mutants.

The final command used the default `--timeout 3.0 --minimum-test-timeout 30` and `--in-place`.
It tested 35 mutants in 42s after a 1s baseline build.

## Conclusion

The final `chat-client-contract` suite catches **100% of viable mutants**: 28 caught, 0
survivors, 0 timeouts, and 7 unviable mutants. The fixes remove an unobservable parser branch
and an arithmetic operation that could prevent the SSE decoder from making progress.
