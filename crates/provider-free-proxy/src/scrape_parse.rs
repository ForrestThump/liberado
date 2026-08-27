//! Deterministic parsers for scraped leaderboard markdown.
//!
//! The scrape fallback exists so ranking survives even when the Benchmarks API cannot answer.
//! Its hard constraint: **no model reads the page**. Each parser is a pure function from
//! markdown text to `(leaderboard name, percent)` rows — table-cell extraction and bounded
//! regexes only, fully unit-testable against captured fixtures. If a page's shape drifts past
//! what a parser recognises, the source returns nothing and the resolver moves on; it never
//! guesses.

use crate::rank::ModelScores;

/// One leaderboard row after extraction: the page's own name for a model and its headline score
/// (a percentage on both sources we parse today).
#[derive(Debug, Clone, PartialEq)]
pub struct LeaderRow {
    pub name: String,
    pub percent: f64,
}

/// Which known page a markdown blob came from — selects the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrapeSource {
    /// `https://openrouter.ai/benchmarks` (JS-rendered; needs spider-mcp's Chrome escalation).
    OpenRouterBenchmarks,
    /// `https://aider.chat/docs/leaderboards/` (static HTML table).
    AiderLeaderboard,
}

impl ScrapeSource {
    /// Every source the fallback knows, in try order.
    pub const ALL: &'static [ScrapeSource] = &[
        ScrapeSource::OpenRouterBenchmarks,
        ScrapeSource::AiderLeaderboard,
    ];

    pub fn url(self) -> &'static str {
        match self {
            ScrapeSource::OpenRouterBenchmarks => "https://openrouter.ai/benchmarks",
            ScrapeSource::AiderLeaderboard => "https://aider.chat/docs/leaderboards/",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScrapeSource::OpenRouterBenchmarks => "openrouter-benchmarks-page",
            ScrapeSource::AiderLeaderboard => "aider-leaderboard",
        }
    }
}

/// Parse `markdown` with the parser for `source`; empty output means "shape not recognised".
///
/// A page can repeat a model across sections or table variants; each name keeps its **best**
/// score, so ordering sees exactly one row per name.
pub fn parse_scraped_markdown(source: ScrapeSource, markdown: &str) -> Vec<LeaderRow> {
    let mut rows = match source {
        ScrapeSource::OpenRouterBenchmarks => parse_openrouter_benchmarks(markdown),
        ScrapeSource::AiderLeaderboard => parse_aider_leaderboard(markdown),
    };
    // Name ascending, then score descending: `dedup_by` keeps the first of each equal-name run,
    // which is now that name's highest score.
    rows.sort_by(|a, b| a.name.cmp(&b.name).then(b.percent.total_cmp(&a.percent)));
    rows.dedup_by(|a, b| a.name == b.name);
    rows
}

/// Rows converted into rank-table entries keyed by leaderboard display name (slug matching is
/// the resolver's job — it needs the free-model list).
pub fn rows_as_scores(rows: &[LeaderRow]) -> Vec<(String, ModelScores)> {
    rows.iter()
        .map(|r| {
            (
                r.name.clone(),
                ModelScores {
                    scraped_percent: Some(r.percent),
                    ..Default::default()
                },
            )
        })
        .collect()
}

/// Parse spider-mcp's rendering of openrouter.ai/benchmarks.
///
/// The rendered page lists, per benchmark card, leaders as
/// `<Metric><score>%<Model display name>$<cost>…` (e.g. `Quality81.5%Claude Fable 5$0.020`).
/// Only `Quality` lines are taken — the page's Value/Speed leaders price-rank models, which must
/// not leak into a coding-quality ordering. Names end at `$` or `,`; anything longer than 80
/// chars is treated as a mis-parse and dropped rather than matched against some poor token soup.
fn parse_openrouter_benchmarks(markdown: &str) -> Vec<LeaderRow> {
    let re =
        regex::Regex::new(r"Quality\s*(\d+(?:\.\d+)?)%\s*([^$,\n]{2,80})").expect("static regex");
    let mut out = Vec::new();
    for caps in re.captures_iter(markdown) {
        let Some(percent) = parse_percent(&caps[1]) else {
            continue;
        };
        let raw_name = caps[2].trim_end();
        let name = clean_name(raw_name);
        if name.len() < 2 || !name.chars().next().is_some_and(char::is_alphanumeric) {
            continue;
        }
        out.push(LeaderRow { name, percent });
    }
    out
}

/// Parse aider's leaderboard table (`| Model | Percent correct | … |`).
///
/// Column order has been stable for years but is not trusted blindly: the header row locates the
/// "Percent correct" column and data rows are read at that same index. Separator and malformed
/// rows are skipped silently.
fn parse_aider_leaderboard(markdown: &str) -> Vec<LeaderRow> {
    let mut percent_col: Option<usize> = None;
    let mut out = Vec::new();
    for line in markdown.lines() {
        let cells = split_row(line);
        if cells.is_empty() || cells.iter().any(|c| c.starts_with("---")) {
            continue;
        }
        if let Some(idx) = cells
            .iter()
            .position(|c| c.to_ascii_lowercase().contains("percent correct"))
        {
            percent_col = Some(idx);
            continue;
        }
        let (Some(col), Some(name)) = (percent_col, cells.first()) else {
            continue;
        };
        if name.is_empty() || name.eq_ignore_ascii_case("model") {
            continue;
        }
        let Some(percent) = leading_number(cells.get(col).copied().unwrap_or("")) else {
            continue;
        };
        out.push(LeaderRow {
            name: clean_name(name),
            percent,
        });
    }
    out
}

/// Split one markdown table row into trimmed cells, tolerating missing outer pipes.
fn split_row(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(str::trim).collect()
}

fn clean_name(raw: &str) -> String {
    // Strip parenthetical annotations ("claude-sonnet-4.5 (diff editor)") — they describe the
    // harness configuration, not the model, and would break slug matching otherwise.
    raw.split('(').next().unwrap_or(raw).trim().to_string()
}

fn parse_percent(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| (0.0..=100.0).contains(v))
}

/// Leading number of a cell ("84.2%", "84.2 pts"). Bounded to `[0, 100]` so stray counts can
/// never impersonate a percentage.
fn leading_number(s: &str) -> Option<f64> {
    let num: String = s
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    parse_percent(&num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_quality_lines_yield_rows_and_skip_value_speed() {
        let md = "Agents & tools\n\
                  Quality81.5%Claude Fable 5$0.020Step 3.7 Flash1.8m\n\
                  Value$0.020Step 3.7 FlashSpeed1.8m\n\
                  Reasoning\n\
                  Quality94.3%Gemini 3.1 Pro Preview$0.015";
        let rows = parse_scraped_markdown(ScrapeSource::OpenRouterBenchmarks, md);
        assert_eq!(
            rows,
            vec![
                LeaderRow {
                    name: "Claude Fable 5".into(),
                    percent: 81.5
                },
                LeaderRow {
                    name: "Gemini 3.1 Pro Preview".into(),
                    percent: 94.3
                },
            ]
        );
    }

    #[test]
    fn openrouter_parser_ignores_unrecognised_shapes() {
        assert!(
            parse_scraped_markdown(ScrapeSource::OpenRouterBenchmarks, "nothing here").is_empty()
        );
        assert!(
            parse_scraped_markdown(ScrapeSource::OpenRouterBenchmarks, "Quality%NoNumber")
                .is_empty()
        );
    }

    #[test]
    fn aider_table_is_parsed_at_the_header_declared_column() {
        // Deliberately puts "Percent correct" at index 2, past a decoy numeric column, so a
        // hard-coded column-1 reader reads edit counts instead.
        let md = "| Model                        | Runs | Percent correct |\n\
                  |:-----------------------------|-----:|:---------------:|\n\
                  | claude-sonnet-4.5 (diff editor) | 500 | 84.2%          |\n\
                  | o3-r192                      | 480  | 81.2%           |\n";
        let rows = parse_scraped_markdown(ScrapeSource::AiderLeaderboard, md);
        assert_eq!(
            rows,
            vec![
                LeaderRow {
                    name: "claude-sonnet-4.5".into(),
                    percent: 84.2
                },
                LeaderRow {
                    name: "o3-r192".into(),
                    percent: 81.2
                },
            ]
        );
    }

    #[test]
    fn repeated_names_keep_their_best_score() {
        let md = "Quality50.0%Model A$1\nQuality70.0%Model A$2";
        let rows = parse_scraped_markdown(ScrapeSource::OpenRouterBenchmarks, md);
        assert_eq!(
            rows,
            vec![LeaderRow {
                name: "Model A".into(),
                percent: 70.0
            }]
        );
    }

    #[test]
    fn scores_outside_zero_to_hundred_are_dropped() {
        assert_eq!(parse_percent("120"), None);
        assert_eq!(parse_percent("-3"), None);
        assert_eq!(parse_percent("abc"), None);
        assert_eq!(parse_percent("55.5"), Some(55.5));
    }

    #[test]
    fn parentheticals_are_stripped_from_names() {
        assert_eq!(clean_name("deepseek r1 (0528)"), "deepseek r1");
        assert_eq!(clean_name("plain"), "plain");
    }

    /// The URL constants are the contract with spider-mcp and with the wiremock mocks in the
    /// resolver tests; a typo'd or emptied URL would silently scrape nothing.
    #[test]
    fn source_urls_and_labels_are_the_documented_pages() {
        assert_eq!(
            ScrapeSource::OpenRouterBenchmarks.url(),
            "https://openrouter.ai/benchmarks"
        );
        assert_eq!(
            ScrapeSource::AiderLeaderboard.url(),
            "https://aider.chat/docs/leaderboards/"
        );
        assert_eq!(
            ScrapeSource::OpenRouterBenchmarks.label(),
            "openrouter-benchmarks-page"
        );
        assert_eq!(ScrapeSource::AiderLeaderboard.label(), "aider-leaderboard");
        assert_eq!(
            ScrapeSource::ALL,
            &[
                ScrapeSource::OpenRouterBenchmarks,
                ScrapeSource::AiderLeaderboard
            ]
        );
    }

    /// Kills the `<` → `==` / `<=` mutations on the name-length floor: exactly-one-char names
    /// never match the bounded regex, exactly-two-char names are real models and must be kept.
    #[test]
    fn openrouter_name_length_floor_drops_one_char_keeps_two() {
        let md = "Quality7.0%X$1\nQuality8.0%Ab$2";
        let rows = parse_scraped_markdown(ScrapeSource::OpenRouterBenchmarks, md);
        assert_eq!(
            rows,
            vec![LeaderRow {
                name: "Ab".into(),
                percent: 8.0
            }]
        );
    }

    /// Kills the `||` → `&&` on the alphanumeric-start guard: a name beginning with punctuation
    /// passes the length floor but must still be rejected outright.
    #[test]
    fn openrouter_names_starting_with_punctuation_are_rejected() {
        let md = "Quality5.0%-ok$1\nQuality6.0%Real Model$2";
        let rows = parse_scraped_markdown(ScrapeSource::OpenRouterBenchmarks, md);
        assert_eq!(
            rows,
            vec![LeaderRow {
                name: "Real Model".into(),
                percent: 6.0
            }]
        );
    }

    /// Kills the `||` → `&&` on the model-name guard: a literal-named or blank model cell must
    /// be skipped, not promoted to a row with a fabricated identity.
    #[test]
    fn aider_rows_named_model_or_blank_are_not_data() {
        let md = "| Model             | Percent correct |\n\
                  |-------------------|-----------------|\n\
                  | Model             | 12.0%           |\n\
                  |                   | 34.0%           |\n\
                  | claude-sonnet-4.5 | 84.2%           |\n";
        let rows = parse_scraped_markdown(ScrapeSource::AiderLeaderboard, md);
        assert_eq!(
            rows,
            vec![LeaderRow {
                name: "claude-sonnet-4.5".into(),
                percent: 84.2
            }]
        );
    }

    /// Kills the `||` → `&&` on the separator/debris guard: any debris cell marks the whole row
    /// as non-data even when a percentage sits elsewhere in it.
    #[test]
    fn aider_debris_rows_are_skipped_wholesale() {
        let md = "| Model            | Percent correct |\n\
                  |---               |-----------------|\n\
                  | ---section break | 42.0%           |\n\
                  | o3-r192          | 81.2%           |\n";
        let rows = parse_scraped_markdown(ScrapeSource::AiderLeaderboard, md);
        assert_eq!(
            rows,
            vec![LeaderRow {
                name: "o3-r192".into(),
                percent: 81.2
            }]
        );
    }

    #[test]
    fn rows_convert_into_scraped_score_entries() {
        let entries = rows_as_scores(&[LeaderRow {
            name: "m".into(),
            percent: 12.5,
        }]);
        assert_eq!(entries[0].0, "m");
        assert_eq!(entries[0].1.scraped_percent, Some(12.5));
    }
}
