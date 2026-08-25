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
    // Slug's model segment ("gemma-4-31b-it") carries the identity most leaderboards name;
    // the author segment ("google") adds evidence for models whose identity lives there
    // ("google/gemma-4" vs a bare "gemma 4" entry still matches via the model half).
    let mut set = tokens(m.id.rsplit('/').next().unwrap_or(&m.id));
    if let Some(author) = m.id.split('/').next()
        && m.id.contains('/')
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
mod tests {
    use super::*;

    fn free(ids: &[&str]) -> Vec<FreeModel> {
        ids.iter()
            .map(|id| FreeModel {
                id: (*id).into(),
                context_length: 0,
                supports_tools: true,
            })
            .collect()
    }

    #[test]
    fn leaderboard_name_matches_its_slug_across_vendor_prefixes() {
        let pool = free(&["deepseek/deepseek-r1:free", "z-ai/glm-5.2:free"]);
        assert_eq!(
            best_slug_for("DeepSeek R1", &pool).as_deref(),
            Some("deepseek/deepseek-r1:free")
        );
    }

    #[test]
    fn hyphens_and_versions_survive_normalization() {
        let pool = free(&["google/gemma-4-31b-it:free"]);
        assert_eq!(
            best_slug_for("Gemma 4 31B", &pool).as_deref(),
            Some("google/gemma-4-31b-it:free")
        );
    }

    #[test]
    fn ambiguous_names_abstain() {
        let pool = free(&["a/ultra-pro", "b/ultra-pro"]);
        assert_eq!(best_slug_for("Ultra Pro", &pool), None);
    }

    #[test]
    fn unrelated_names_return_none() {
        let pool = free(&["z-ai/glm-5.2:free"]);
        assert_eq!(best_slug_for("Completely Different Thing", &pool), None);
        assert_eq!(best_slug_for("", &pool), None);
    }

    #[test]
    fn stopword_heavy_names_still_match_on_content_tokens() {
        let pool = free(&["nvidia/nemotron-3-ultra-550b-a55b:free"]);
        assert_eq!(
            best_slug_for("Nemotron 3 Ultra Instruct Chat Latest", &pool).as_deref(),
            Some("nvidia/nemotron-3-ultra-550b-a55b:free")
        );
    }

    #[test]
    fn the_clear_winner_beats_a_weaker_partial_overlap() {
        let pool = free(&["x/nemotron-nano", "y/nemotron-3-ultra-550b-a55b"]);
        assert_eq!(
            best_slug_for("Nemotron 3 Ultra", &pool).as_deref(),
            Some("y/nemotron-3-ultra-550b-a55b")
        );
    }

    /// Kills the `/` → `*` mutation in `dice`: a size-boosting score hands the win to the
    /// candidate with the larger token set instead of the higher overlap ratio. The numbers are
    /// chosen so the two ratios are NOT equal (the ambiguity guard would otherwise mask the
    /// difference): 6/9 beats 8/13 by ratio, yet loses badly by product.
    #[test]
    fn overlap_ratio_not_token_set_size_decides() {
        let pool = free(&[
            "org/alpha-beta-gamma-extra",
            "org/alpha-beta-gamma-delta-epsilon-zeta-eta-theta",
        ]);
        assert_eq!(
            best_slug_for("Alpha Beta Gamma Delta", &pool).as_deref(),
            Some("org/alpha-beta-gamma-extra"),
            "3/5 overlap on a small set must beat 4/9 spread over a big one"
        );
    }

    /// Kills the `<` → `<=` family on the length filters in `parse_openrouter_benchmarks`'s
    /// sibling parser and here via the empty-name path: a one-character name is noise.
    #[test]
    fn single_character_names_do_not_match_anything() {
        let pool = free(&["x/gamma"]);
        assert_eq!(best_slug_for("A", &pool), None);
    }
}
