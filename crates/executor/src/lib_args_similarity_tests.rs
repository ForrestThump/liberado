//! Split from `lib.rs`: cosine / args-similarity unit tests (module-health).
//!
//! Kept beside the clamp on `cosine` so the [0, 1] regression stays next to the
//! production fix without growing the crate-root file past its ploc waiver.

use super::*;

#[test]
fn cosine_of_identical_vectors_is_one() {
    let mut v = std::collections::HashMap::new();
    v.insert("hello".into(), 1.0);
    v.insert("world".into(), 2.0);
    assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
}

#[test]
fn cosine_of_orthogonal_vectors_is_zero() {
    let mut a = std::collections::HashMap::new();
    a.insert("hello".into(), 1.0);
    let mut b = std::collections::HashMap::new();
    b.insert("world".into(), 1.0);
    assert!((cosine(&a, &b) - 0.0).abs() < 1e-6);
}

#[test]
fn cosine_of_zero_vector_is_zero() {
    let a = std::collections::HashMap::new();
    let mut b = std::collections::HashMap::new();
    b.insert("hello".into(), 1.0);
    assert!((cosine(&a, &b) - 0.0).abs() < 1e-6);
    assert!((cosine(&b, &a) - 0.0).abs() < 1e-6);
}

#[test]
fn args_similarity_permutation_arrays_stay_in_unit_range() {
    // Minimal failing input from main CI after #208 (ubuntu test job).
    let a = serde_json::json!([null, false]);
    let b = serde_json::json!([false, null]);
    let s = args_similarity(&a, &b);
    assert!(
        (0.0..=1.0).contains(&s),
        "permuted near-duplicate arrays must stay in [0,1], got {s}"
    );
}

#[test]
fn args_similarity_default_near_duplicates() {
    let a = serde_json::json!({"q": "hello world"});
    let b = serde_json::json!({"q": "hello world"});
    let sim = args_similarity(&a, &b);
    assert!(
        sim > 0.9,
        "identical plain text should be near-duplicate: {sim}"
    );
}

#[test]
fn args_similarity_neutral_args_still_use_cosine() {
    // Two non-empty, unequal argument sets with no overlap in tokens. The `&&` on line 1247
    // correctly skips the early return to compute TF-IDF; with `||` it would return 0.0.
    // Cosine is also 0.0 for orthogonal vectors, so we verify the function doesn't panic
    // and returns a sub-1 value.
    let a = serde_json::json!({"x": "hello"});
    let b = serde_json::json!({"y": "world"});
    let sim = args_similarity(&a, &b);
    assert!(
        sim < 1.0,
        "different args must not be perfectly similar: {sim}"
    );
}

#[test]
fn args_similarity_empty_one_is_not_one() {
    let a = serde_json::json!({});
    let b = serde_json::json!({"q": "hello"});
    // When one is empty but the other has tokens, similarity must still be computed.
    let sim = args_similarity(&a, &b);
    assert!(
        sim < 1.0,
        "one empty should not produce perfect similarity: {sim}"
    );
}

#[test]
fn tf_idf_smoke() {
    let docs = vec![
        vec!["a".to_string(), "b".to_string()],
        vec!["b".to_string(), "c".to_string()],
    ];
    let vectors = tf_idf_vectors(&docs);
    assert_eq!(vectors.len(), 2);
    // Each vector has 2 entries.
    assert_eq!(vectors[0].len(), 2);
    assert_eq!(vectors[1].len(), 2);
}
