//! Branch naming for delegated work (plan §7.4): deterministic, git-ref-safe, and
//! stable across kickback rounds so every round lands on the same PR.

use liberado_delegate_contract::TaskSpec;

/// Branch shape per plan §7.4, honoring the grant's namespace override.
pub fn branch_name(spec: &TaskSpec) -> String {
    let namespace = spec
        .grant
        .branch_namespace
        .clone()
        .unwrap_or_else(|| spec.id.short());
    format!("delegate/{}/{}", namespace, slugify(&spec.goal))
}

/// Git-ref-safe kebab of the goal's leading words, capped at 40 characters.
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if slug.ends_with('-') || slug.is_empty() {
            continue;
        } else {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    slug.trim_matches('-').to_string()
}
