# Cold Review PR Flow

## When to use
- Reviewing an open PR before merge
- A cold-start (no context) subagent reviews the diff, then you verify findings against the actual code and fix real issues

## Flow

### 1. Checkout and diff
```bash
gh pr view <N> --json title,headRefName,additions,deletions,files
git fetch origin <branch>
git checkout <branch>
git diff main..HEAD
```

### 2. Cold-start review
Launch a subagent with the full diff. Tell it:
- It is a cold reviewer with NO project context
- Identify real issues: bugs, security holes, missing edge cases, design flaws
- Ignore style pedantry
- Return issues with severity (high/medium/low) and clear explanation
- Do NOT fix anything — just report

### 3. Verify findings against code
For each issue the reviewer found:
1. Read the actual code at the cited locations
2. Determine if the claim is:
   - **Real** — the code actually has this bug/gap
   - **Exaggerated** — technically possible but vanishingly unlikely
   - **Hallucinated** — the reviewer misunderstood the code or read it wrong

### 4. Fix real issues
Make followup commits on the PR branch for issues that are REAL and WORTH FIXING:
- High-severity issues: always fix
- Medium issues: fix if the fix is clean and low-risk
- Low issues: fix only if trivial (doc comments, test additions)

Skip issues that are:
- Theoretical but un-exploitable (TOCTOU with local-only attacker)
- Already correct code that the reviewer misread
- Design choices the author clearly made intentionally

### 5. Commit and push
```bash
git add <files>
git commit -m "fix: address cold review findings in <PR area>"
git push origin <branch>
```

### Key principle
The cold reviewer has zero context — it will find both real bugs and false positives. Your job is to be the filter that separates them by reading the actual code.
