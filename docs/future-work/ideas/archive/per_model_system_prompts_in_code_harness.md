> **[archive, 2026-09-18]** Undeveloped one-paragraph stub. Never listed in
> [`../README.md`](../README.md) — the living ideas table did not index it.
> Related direction lives in
> [`../../model-knob-profiles.md`](../../model-knob-profiles.md) (draft), and
> [`../../harness-study-2026-08.md`](../../harness-study-2026-08.md) treats
> each harness's native system prompt as part of what is being measured
> (see §8 of that study: "Keep each harness's native system prompt and tool
> schemas: those are part of the harness being measured"). Moved here so the
> idea is not lost, but it is **not** current product direction.

---

Some harnesses are tuned to specific models. E.g. Grok Build system prompts are obviously tuned to Grok models. Kimicode's prompts are tuned to Kimi models. DeepSeek will be releasing a DeepSeek harness soon, and it will be tuned to DeepSeek V4. Therefore, the Liberado coding harness should use system prompts tuned to the specific model used in the harness in a given moment. So if the harness is set to use Grok, it should use Grok Build system prompts. etc. Having it do this automatically and dynamically would be a great feature.
