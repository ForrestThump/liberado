//! One canonical rendering of a file for the model, and the inverse that writes it back.
//!
//! ## Why this exists
//!
//! An edit tool matches the model's `old` text against the file's bytes. That only works if both
//! sides agree on what the file *looks like*. Two things routinely break that agreement, and both
//! are invisible in a diff:
//!
//! - **Line endings.** A model emits `\n`. A file checked out on Windows with `core.autocrlf` on
//!   holds `\r\n`. Every exact match then fails with "old text was not found", and the model has
//!   no way to see why. `CLAUDE.md` already names autocrlf as a Windows CI hazard; this is the
//!   same hazard reaching the coding tools.
//! - **A byte-order mark.** A leading `\u{feff}` shifts the first line, so an anchor on line one
//!   never matches, and a naive rewrite drops the BOM from a file that needs it.
//!
//! `kimi-code` and `opencode` both solve this the same way: define one model-facing view, take
//! the model's anchors from that view, and restore the original shape on write. This is that,
//! ported.
//!
//! ## The rule for mixed files
//!
//! A file whose endings are all `\n`, or all `\r\n`, is shown as `\n` and written back in its own
//! style. A file that mixes them is shown and written **literally, unchanged**.
//!
//! That asymmetry is deliberate. Normalizing a mixed file would make an edit tool silently
//! rewrite every line it did not touch, turning a one-line change into a whole-file diff — the
//! kind of change that passes review because nobody reads 3,000 unrelated lines. Refusing to
//! guess is cheaper than being wrong at scale.

/// How a file separates its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// Every ending is `\n`, or the file has none at all.
    Lf,
    /// Every ending is `\r\n`.
    Crlf,
    /// A mix. The view is the raw text and nothing is converted on the way back.
    Mixed,
}

/// A file as the model sees it, plus what is needed to restore the original shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelText {
    /// What the model reads and writes anchors against.
    pub text: String,
    pub ending: LineEnding,
    /// The file began with a UTF-8 BOM, which must be put back.
    pub bom: bool,
}

const BOM: char = '\u{feff}';

/// Render raw file text as the model should see it.
pub fn to_model_view(raw: &str) -> ModelText {
    let (bom, body) = match raw.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    let crlf = body.matches("\r\n").count();
    let all_lf = body.matches('\n').count();
    let ending = if crlf == 0 {
        LineEnding::Lf
    } else if crlf == all_lf {
        LineEnding::Crlf
    } else {
        LineEnding::Mixed
    };
    let text = match ending {
        LineEnding::Crlf => body.replace("\r\n", "\n"),
        // Lf needs no work, and Mixed must not be touched.
        _ => body.to_string(),
    };
    ModelText { text, ending, bom }
}

/// Turn edited model-view text back into the file's own shape.
pub fn materialize(text: &str, ending: LineEnding, bom: bool) -> String {
    let body = match ending {
        // `\n` -> `\r\n`, but only after collapsing any `\r\n` the model supplied itself, or a
        // model that pasted CRLF back would produce `\r\r\n`.
        LineEnding::Crlf => text.replace("\r\n", "\n").replace('\n', "\r\n"),
        _ => text.to_string(),
    };
    if bom { format!("{BOM}{body}") } else { body }
}

/// Put a model-supplied anchor into the same shape as [`to_model_view`]'s output.
///
/// A model can emit `\r\n` — because it copied from a CRLF source, or simply because it did.
/// Comparing that against an LF view fails for a reason no error message can usefully explain,
/// so both sides are normalized before matching. `Mixed` is left alone for the same reason the
/// view is: in a file we do not understand, the literal bytes are the only safe contract.
pub fn normalize_anchor(anchor: &str, ending: LineEnding) -> String {
    match ending {
        LineEnding::Mixed => anchor.to_string(),
        _ => anchor.replace("\r\n", "\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pure_lf_file_is_unchanged_in_both_directions() {
        let view = to_model_view("a\nb\n");
        assert_eq!(view.ending, LineEnding::Lf);
        assert_eq!(view.text, "a\nb\n");
        assert_eq!(materialize(&view.text, view.ending, view.bom), "a\nb\n");
    }

    /// The Windows case. A model emitting `\n` must be able to match a CRLF file, and the file
    /// must still be CRLF afterwards — an edit tool that quietly converts a repo's line endings
    /// produces a diff nobody can review.
    #[test]
    fn a_crlf_file_reads_as_lf_and_writes_back_as_crlf() {
        let view = to_model_view("a\r\nb\r\n");
        assert_eq!(view.ending, LineEnding::Crlf);
        assert_eq!(view.text, "a\nb\n", "the model must not have to type \\r");
        assert_eq!(materialize("a\nB\n", view.ending, view.bom), "a\r\nB\r\n");
    }

    /// A model that pastes CRLF back into `new` must not produce `\r\r\n`.
    #[test]
    fn crlf_supplied_by_the_model_is_not_doubled() {
        assert_eq!(
            materialize("a\r\nb\r\n", LineEnding::Crlf, false),
            "a\r\nb\r\n"
        );
    }

    /// Mixed endings are left exactly as found. Normalizing would rewrite every untouched line
    /// in the file, which turns a one-line edit into an unreviewable diff.
    #[test]
    fn a_mixed_file_is_passed_through_untouched() {
        let raw = "a\r\nb\nc\r\n";
        let view = to_model_view(raw);
        assert_eq!(view.ending, LineEnding::Mixed);
        assert_eq!(
            view.text, raw,
            "a file we do not understand must not be rewritten"
        );
        assert_eq!(materialize(&view.text, view.ending, view.bom), raw);
    }

    #[test]
    fn a_bom_is_hidden_from_the_model_and_restored_on_write() {
        let view = to_model_view("\u{feff}fn main() {}\n");
        assert!(view.bom);
        assert_eq!(
            view.text, "fn main() {}\n",
            "a leading BOM shifts every line-one anchor"
        );
        assert_eq!(
            materialize(&view.text, view.ending, view.bom),
            "\u{feff}fn main() {}\n",
            "dropping the BOM changes a file the edit never meant to touch"
        );
    }

    #[test]
    fn a_bom_on_a_crlf_file_survives_both_conversions() {
        let raw = "\u{feff}a\r\nb\r\n";
        let view = to_model_view(raw);
        assert!(view.bom);
        assert_eq!(view.ending, LineEnding::Crlf);
        assert_eq!(view.text, "a\nb\n");
        assert_eq!(materialize(&view.text, view.ending, view.bom), raw);
    }

    /// The point of normalizing the anchor as well as the file: otherwise a model that emits
    /// CRLF cannot match an LF view, and the error says only "not found".
    #[test]
    fn an_anchor_is_matched_in_the_same_shape_as_the_view() {
        let view = to_model_view("let x = 1;\r\nlet y = 2;\r\n");
        let anchor = normalize_anchor("let x = 1;\r\nlet y = 2;\r\n", view.ending);
        assert!(
            view.text.contains(&anchor),
            "a CRLF anchor must match a CRLF file read as LF"
        );
    }

    #[test]
    fn a_mixed_file_matches_anchors_literally() {
        let view = to_model_view("a\r\nb\nc\r\n");
        assert_eq!(
            normalize_anchor("b\nc\r\n", view.ending),
            "b\nc\r\n",
            "in a mixed file the literal bytes are the only safe contract"
        );
    }

    #[test]
    fn a_file_with_no_newline_at_all_is_lf() {
        let view = to_model_view("no trailing newline");
        assert_eq!(view.ending, LineEnding::Lf);
        assert_eq!(
            materialize(&view.text, view.ending, view.bom),
            "no trailing newline"
        );
    }
}
