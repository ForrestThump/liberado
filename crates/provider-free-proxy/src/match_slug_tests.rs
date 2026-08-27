use super::*;

fn free(ids: &[&str]) -> Vec<FreeModel> {
    ids.iter()
        .map(|id| FreeModel::fixture(*id, 0, true))
        .collect()
}

#[test]
fn api_slug_maps_onto_a_prefixed_openrouter_id() {
    let pool = vec![FreeModel {
        id: "openrouter/z-ai/glm-5.2:free".into(),
        provider: "openrouter".into(),
        upstream_id: "z-ai/glm-5.2:free".into(),
        context_length: 0,
        supports_tools: true,
    }];
    assert_eq!(
        free_id_for_api_slug("z-ai/glm-5.2", &pool).as_deref(),
        Some("openrouter/z-ai/glm-5.2:free")
    );
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

#[test]
fn api_permaslug_joins_the_free_variant() {
    let pool = free(&["z-ai/glm-5.2:free", "deepseek/deepseek-r1:free"]);
    assert_eq!(
        free_id_for_api_slug("z-ai/glm-5.2", &pool).as_deref(),
        Some("z-ai/glm-5.2:free")
    );
    assert_eq!(
        free_id_for_api_slug("z-ai/glm-5.2:free", &pool).as_deref(),
        Some("z-ai/glm-5.2:free")
    );
    assert_eq!(
        free_id_for_api_slug("deepseek/deepseek-r1:extended", &pool).as_deref(),
        Some("deepseek/deepseek-r1:free")
    );
}

#[test]
fn api_permaslug_exact_match_wins_when_the_free_id_has_no_suffix() {
    let pool = free(&["vendor/small"]);
    assert_eq!(
        free_id_for_api_slug("vendor/small", &pool).as_deref(),
        Some("vendor/small")
    );
}

#[test]
fn api_permaslug_with_no_free_peer_is_dropped() {
    let pool = free(&["z-ai/glm-5.2:free"]);
    assert_eq!(free_id_for_api_slug("unrelated/model", &pool), None);
    assert_eq!(free_id_for_api_slug("z-ai/glm-5.2", &[]), None);
}
