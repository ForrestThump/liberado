//! Matching scraped leaderboard names onto OpenRouter model slugs — without an LLM.
//!
//! Leaderboards name models their own way (`DeepSeek R1`, `claude-sonnet-4.5`); OpenRouter wants
//! slugs (`deepseek/deepseek-r1:free`). The bridge is token overlap after normalization, scored
//! with the Dice coefficient and accepted only when a **unique** best candidate clears the
//! threshold. Ambiguity must abstain: ranking on a wrong match is worse than ranking on none,
//! and "no opinion" here just leaves the model to the unranked tail of the order.

use std::collections::HashSet;

use crate::free::FreeModel;

/// Tokens below this length carry too little identity to vote ("v3", "it", "ai").
const MIN_TOKEN_LEN: usize = 2;
/// Minimum Dice coefficient for a candidate to be considered at all.
const MATCH_THRESHOLD: f64 = 0.5;

/// Words that appear in both leaderboard names and slugs but distinguish nothing.
const STOPWORDS: &[&str] = &[
    "chat",
    "instruct",
    "it",
    "free",
    "preview",
    "experimental",
    "latest",
    "api",
    "model",
    "online",
    "thinking",
];

/// Map a Benchmarks API permaslug onto a currently-free OpenRouter id.
///
/// The API names the canonical slug (`z-ai/glm-5.2`); `/models` lists the
/// zero-priced variant as `{slug}:free`. Ranking looks up the free id, so a
/// miss here drops the score and the origin still claims the API decided.
pub fn free_id_for_api_slug(slug: &str, free: &[FreeModel]) -> Option<String> {
    try_free_id(slug, free).or_else(|| {
        slug.rsplit_once(':')
            .map(|(stem, _)| stem)
            .filter(|stem| stem.contains('/'))
            .and_then(|stem| try_free_id(stem, free))
    })
}

fn try_free_id(slug: &str, free: &[FreeModel]) -> Option<String> {
    let hits: Vec<&FreeModel> = free
        .iter()
        .filter(|m| model_matches_slug(m, slug))
        .collect();
    match hits.as_slice() {
        [] => None,
        [m] => Some(m.id.clone()),
        many => {
            // Benchmarks API slugs are OpenRouter's. Prefer that vendor when several
            // providers share a native id; abstain if even that is ambiguous.
            let or: Vec<&FreeModel> = many
                .iter()
                .copied()
                .filter(|m| m.provider == "openrouter")
                .collect();
            match or.as_slice() {
                [m] => Some(m.id.clone()),
                _ => None,
            }
        }
    }
}

fn model_matches_slug(m: &FreeModel, slug: &str) -> bool {
    let as_free = format!("{slug}:free");
    if m.id == slug || m.id == as_free || m.upstream_id == slug || m.upstream_id == as_free {
        return true;
    }
    let prefix = format!("{}/", m.provider);
    m.id.strip_prefix(&prefix)
        .is_some_and(|rest| rest == slug || rest == as_free)
}

/// The best-matching free-model slug for a leaderboard name, or `None`.
///
/// Uniqueness rule: if the top two candidates tie (same score), the name is ambiguous and no
/// slug is returned. Ties are exactly how "GPT-4o" ends up pinned to the wrong vendor variant.
pub fn best_slug_for(leader_name: &str, free: &[FreeModel]) -> Option<String> {
    let leader = tokens(leader_name);
    if leader.is_empty() {
        return None;
    }

    let mut scored: Vec<(f64, &str)> = free
        .iter()
        .filter_map(|m| {
            let dice = dice(&leader, &tokens_for_model(m));
            (dice >= MATCH_THRESHOLD).then_some((dice, m.id.as_str()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    match scored.as_slice() {
        [(_best, id)] => Some((*id).to_string()),
        // Exact ties are ambiguous: two candidates with identical overlap give no principled
        // winner, and picking either pins leaderboard names to the wrong vendor variant.
        // Exact comparison rather than an epsilon window — dice scores are small rationals,
        // so "nearly equal" is not a real state and a tolerance boundary would be untestable.
        [(top, _), (second, _), ..] if top == second => None,
        [(_best, id), ..] => Some((*id).to_string()),
        [] => None,
    }
}

fn tokens_for_model(m: &FreeModel) -> HashSet<String> {
    // Native slug ("gemma-4-31b-it" / "google/gemma-4-31b-it:free") carries the identity
    // most leaderboards name. Public ids are `{provider}/{native}`; scoring the native
    // half keeps "Gemma 4 31B" matching after the provider prefix is added.
    let slug = if m.upstream_id.is_empty() {
        m.id.as_str()
    } else {
        m.upstream_id.as_str()
    };
    let mut set = tokens(slug.rsplit('/').next().unwrap_or(slug));
    if let Some(author) = slug.split('/').next()
        && slug.contains('/')
    {
        set.extend(tokens(author));
    }
    set
}

/// Lowercase alphanumeric tokens, minus stopwords and single characters.
fn tokens(s: &str) -> HashSet<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= MIN_TOKEN_LEN)
        .filter(|t| !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

fn dice(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    // The empty guards look redundant (an empty set intersects to 0 regardless), but with both
    // sets empty the division below is 0/0 → NaN. The ≥threshold filter discards NaN anyway,
    // so mutating this `||` changes no observable behavior — it stays because NaN must never
    // enter the scores that feed ordering.
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    2.0 * inter / (a.len() + b.len()) as f64
}

#[cfg(test)]
#[path = "match_slug_tests.rs"]
mod tests;
