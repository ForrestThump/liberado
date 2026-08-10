//! Score [`session_critic`](liberado_coder_agent::session_critic) against traces whose verdict we
//! already know.
//!
//! A critic is only worth wiring in if it finds the things we found by hand and stays quiet
//! otherwise. Both halves matter: a reviewer that flags every run is as useless as one that flags
//! none, and the second failure is the one that gets a check switched off.
//!
//! Usage:
//!   OPENROUTER_API_KEY=... cargo run -p liberado-coder-agent --example score_session_critic -- \
//!     [--model <id>] [--text-only] <expected> <trace.json> [<expected> <trace.json> ...]
//!
//! `<expected>` is `clean`, or the `kind` the run is known to contain
//! (`abandoned_finding` / `unsupported_claim` / `silent_reversal`).
//!
//! `--text-only` drops tool-call names, which is the setting this measurement exists to argue
//! about: run the same set both ways and compare.

use std::sync::Arc;

use liberado_coder_agent::SingleProviderFactory;
use liberado_coder_agent::session_critic::{self, ToolVisibility};
use liberado_coder_core::{CoderRoleConfig, CoderTrace};
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let visibility = if args.iter().any(|a| a == "--text-only") {
        ToolVisibility::TextOnly
    } else {
        ToolVisibility::NamesOnly
    };
    let model = args
        .iter()
        .position(|a| a == "--model")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| "deepseek/deepseek-v4-pro".to_string());
    let pairs: Vec<&String> = args
        .iter()
        .filter(|a| !a.starts_with("--") && **a != model)
        .collect();
    if pairs.is_empty() || !pairs.len().is_multiple_of(2) {
        eprintln!("usage: score_session_critic [--model M] [--text-only] <expected> <trace> ...");
        std::process::exit(2);
    }

    let api_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY");
    let provider = OpenAiCompatibleProvider::new(
        api_key,
        model.clone(),
        OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
    );
    let providers = SingleProviderFactory::new(Arc::new(provider));
    let role = CoderRoleConfig {
        model: model.clone(),
        prompt_path: None,
        prompt: None,
        temperature: Some(0.0),
        max_tokens: Some(8000),
        max_turns: Some(1),
    };

    println!("model:      {model}");
    println!("visibility: {visibility:?}\n");

    let (mut hits, mut misses, mut false_alarms) = (0, 0, 0);
    for chunk in pairs.chunks(2) {
        let (expected, path) = (chunk[0].as_str(), chunk[1].as_str());
        let raw = std::fs::read_to_string(path).expect("read trace");
        let trace: CoderTrace = serde_json::from_str(&raw).expect("parse trace");
        let transcript = session_critic::build_transcript(&trace.events, visibility);

        println!(
            "── {}",
            std::path::Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
        );
        println!("   expected:   {expected}");
        println!("   transcript: {} chars", transcript.len());

        let filed_report = trace.result.as_ref().map(|r| r.summary.as_str());
        println!(
            "   report:     {} chars",
            filed_report.map(str::len).unwrap_or(0)
        );

        let review = match session_critic::review_session(
            &providers,
            &trace.request,
            &role,
            &trace.events,
            filed_report,
            visibility,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                // Not scored either way. A failed call is not a verdict, and counting it as a
                // clean run is how a measurement flatters the thing it measures.
                println!("   ERROR: {e}\n");
                continue;
            }
        };

        let found: Vec<&str> = review.findings.iter().map(|f| f.kind.as_str()).collect();
        println!("   found:      {:?}", found);
        for f in &review.findings {
            println!("     [{}] {}", f.kind, f.why);
            println!("       > {}", f.quote.replace('\n', " "));
        }
        match (expected, review.is_clean()) {
            ("clean", true) => {
                hits += 1;
                println!("   VERDICT:    correct (quiet on a clean run)");
            }
            ("clean", false) => {
                false_alarms += 1;
                println!("   VERDICT:    FALSE ALARM");
            }
            (want, _) if found.contains(&want) => {
                hits += 1;
                println!("   VERDICT:    correct");
            }
            (_, true) => {
                misses += 1;
                println!("   VERDICT:    MISS (said nothing)");
            }
            _ => {
                misses += 1;
                println!("   VERDICT:    MISS (found something else)");
            }
        }
        println!();
    }
    println!("hits={hits} misses={misses} false_alarms={false_alarms}");
}
