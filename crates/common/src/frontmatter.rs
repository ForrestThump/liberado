//! The YAML-frontmatter-fence note convention shared by every vault artifact that's structured
//! metadata + a human-readable body: [`crate::proposal::Proposal`], `liberado_memory_store`'s
//! notes, and `liberado-deliberate-mcp`'s deliberation transcripts all render/parse the same shape
//! (`---\n<yaml>\n---\n\n<body>`) — this was three independent, byte-identical implementations
//! before being pulled out here.

use serde::Serialize;

/// The fence that separates YAML frontmatter from a note's body.
pub const FRONTMATTER_FENCE: &str = "---";

/// Render `frontmatter` as a YAML-fenced block followed by `body`.
///
/// # Panics
/// Panics if `frontmatter` fails to serialize to YAML. Every current caller passes a plain
/// `#[derive(Serialize)]` struct of primitive/serde-standard fields, which cannot fail — this
/// matches the `.expect(...)` every caller wrote before this was consolidated.
pub fn render_note(frontmatter: &impl Serialize, body: &str) -> String {
    let yaml = serde_yaml::to_string(frontmatter).expect("frontmatter serializes to YAML");
    format!("{FRONTMATTER_FENCE}\n{yaml}{FRONTMATTER_FENCE}\n\n{body}")
}

/// Split out the YAML between the leading `---` fences. `None` if `content` doesn't start with a
/// well-formed frontmatter block.
pub fn extract_frontmatter(content: &str) -> Option<&str> {
    let rest = content.strip_prefix(FRONTMATTER_FENCE)?;
    let after_open = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let close = after_open.find(&format!("\n{FRONTMATTER_FENCE}"))?;
    Some(&after_open[..close])
}

/// The note body: everything after the closing frontmatter fence, including the blank-line
/// padding `render_note` inserts before the body — callers that want a clean body call
/// `.trim()` on the result (matching every current caller). Returns `content` unchanged if it has
/// no well-formed frontmatter block (rather than panicking/erroring — callers that only care about
/// the frontmatter, like `Proposal::from_note`, never call this at all).
pub fn body_after_frontmatter(content: &str) -> &str {
    let Some(rest) = content.strip_prefix(FRONTMATTER_FENCE) else {
        return content;
    };
    let Some(after_open) = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
    else {
        return content;
    };
    let fence_with_nl = format!("\n{FRONTMATTER_FENCE}");
    let Some(close) = after_open.find(&fence_with_nl) else {
        return content;
    };
    &after_open[close + fence_with_nl.len()..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Meta {
        id: String,
        n: u32,
    }

    #[test]
    fn round_trips_frontmatter_and_body() {
        let meta = Meta { id: "x1".into(), n: 7 };
        let note = render_note(&meta, "Hello, body.\n");

        let fm = extract_frontmatter(&note).unwrap();
        let parsed: Meta = serde_yaml::from_str(fm).unwrap();
        assert_eq!(parsed, meta);

        // body_after_frontmatter includes render_note's blank-line padding; callers trim it.
        assert_eq!(body_after_frontmatter(&note).trim(), "Hello, body.");
    }

    #[test]
    fn missing_frontmatter_returns_none() {
        assert_eq!(extract_frontmatter("just a body, no frontmatter"), None);
    }

    #[test]
    fn body_after_frontmatter_falls_back_to_whole_content_when_malformed() {
        let content = "not a fenced note at all";
        assert_eq!(body_after_frontmatter(content), content);
    }
}
