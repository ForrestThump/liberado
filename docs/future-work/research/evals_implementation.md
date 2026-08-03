
Anthropic Engineer:

"The biggest mistake in AI right now - people are building graphs and loops without self-improving eval agent

We made that mistake at Anthropic - It cost us 2 years"

here's how he build it, step by step:

step 1 → take 50 real user prompts - run your agent - if it passes 80%+, your eval is too easy - sweet spot is 50%

step 2 → every failed run is a transcript - feed it to Haiku: "what went wrong?" - your eval set builds itself for free

step 3 → score two things: right answer AND right path - right answer, wrong path = breaks next week

step 4 → new model drops, run the same eval - his +9% was fake - 6% was the model dodging a bug - the transcript caught it, the dashboard didn't

step 5 → plug evals into CI - no green evals = no deploy -  this is the loop that fixes itself
