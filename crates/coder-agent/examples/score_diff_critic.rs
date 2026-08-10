//! Score the cold diff reviewer against a change whose verdict we already know.
//!
//! The completion gate has existed and been switched off for a while. Before wiring it into a
//! path that can fail a run, it is worth knowing whether it catches the thing we most recently
//! caught by hand — a diff whose tests provably do not bind to the code it changes.
//!
//! It uses [`liberado_coder_agent::COLD_DIFF_REVIEWER_PROMPT`] directly, so this measures the
//! reviewer that actually runs rather than a copy of its prompt.
//!
//! Usage:
//!   OPENROUTER_API_KEY=... cargo run -p liberado-coder-agent --example score_diff_critic -- \
//!     [--model <id>] <expected> <task-file> <diff-file> [...]
//!
//! `<expected>` is `acceptable` or `needs_revision`.

use liberado_provider::{CompletionRequest, Message, Provider};
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "deepseek/deepseek-v4-pro".to_string());
    let rest: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with("--") && **a != model)
        .collect();
    if rest.is_empty() || !rest.len().is_multiple_of(3) {
        eprintln!("usage: score_diff_critic [--model M] <expected> <task-file> <diff-file> ...");
        std::process::exit(2);
    }

    let provider = OpenAiCompatibleProvider::new(
        std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY"),
        model.clone(),
        OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
    );
    println!("model: {model}\n");

    let (mut hits, mut misses) = (0, 0);
    for chunk in rest.chunks(3) {
        let (expected, task_path, diff_path) =
            (chunk[0].as_str(), chunk[1].as_str(), chunk[2].as_str());
        let task = std::fs::read_to_string(task_path).expect("read task");
        let diff = std::fs::read_to_string(diff_path).expect("read diff");

        // Same shape the gate assembles: task, then the diff as evidence.
        let user = format!(
            "Task:\n{task}\n\nUnified git diff:\n```\n{}\n```",
            &diff[..diff.len().min(48_000)]
        );
        let completion = CompletionRequest::new(vec![
            Message::system(liberado_coder_agent::COLD_DIFF_REVIEWER_PROMPT.to_string()),
            Message::user(user),
        ])
        .with_max_tokens(4000)
        // Match `reviewer_role`'s sampling exactly. An earlier version of this harness left
        // temperature at the provider default and measured a reviewer that does not exist: two
        // identical runs disagreed, one calling the same diff acceptable and the other naming the
        // precise defect. A score for the wrong sampling setting is not a score.
        .with_temperature(0.0);

        println!("── {diff_path}");
        println!("   expected: {expected}   diff: {} chars", diff.len());
        let response = match provider.complete(completion).await {
            Ok(r) => r,
            Err(e) => {
                println!("   ERROR: {e}\n");
                continue;
            }
        };
        let content = response.content.unwrap_or_default();
        let body = match (content.find('{'), content.rfind('}')) {
            (Some(s), Some(e)) if e > s => &content[s..=e],
            _ => {
                println!(
                    "   ERROR: no JSON in reply: {}\n",
                    &content[..content.len().min(200)]
                );
                continue;
            }
        };
        let value: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => {
                println!("   ERROR: unparseable: {e}\n");
                continue;
            }
        };
        let quality = value.get("quality").and_then(|q| q.as_str()).unwrap_or("?");
        println!("   verdict:  {quality}");
        if let Some(issues) = value.get("issues").and_then(|i| i.as_array()) {
            for issue in issues {
                println!("     - {}", issue.as_str().unwrap_or_default());
            }
        }
        if quality == expected {
            hits += 1;
            println!("   RESULT:   correct\n");
        } else {
            misses += 1;
            println!("   RESULT:   WRONG\n");
        }
    }
    println!("hits={hits} misses={misses}");
}
