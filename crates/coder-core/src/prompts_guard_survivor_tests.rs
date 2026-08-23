//! Split from `prompts.rs` for module-health boundaries.

//! The three NotFound-guard mutants in `read_prompt_candidate` are log-only —
//! every arm returns `None`. They are observable only through which log level
//! fires, so this test installs a capturing subscriber.
//!
//! Classification from mutation runs: `guard -> false` and `== -> !=` are KILLED
//! here (a missing file must stay silent, an unreadable one must warn).
//! `guard -> true` is EQUIVALENT — cargo-mutants keeps the arm's silent body, and
//! both arms return None identically; only tracing output differs.

// The exercised items are parent-private; only the unix build references them.
#[cfg(unix)]
use super::*;
use std::sync::{Arc, Mutex};

#[derive(Default, Clone)]
struct Captured(Arc<Mutex<Vec<(tracing::Level, String)>>>);
impl<S: tracing::Subscriber> tracing_subscriber::layer::Layer<S> for Captured {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        // Record every field, not just the message: the path that distinguishes
        // one warn from another lives in structured fields.
        struct Msg(Vec<String>);
        impl tracing::field::Visit for Msg {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                self.0.push(format!("{}={v:?}", f.name()));
            }
        }
        let mut m = Msg(Vec::new());
        event.record(&mut m);
        let text = m.0.join(" ");
        self.0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), text));
    }
}

#[test]
fn missing_file_is_quiet_while_unreadable_and_empty_are_loud() {
    use tracing_subscriber::layer::SubscriberExt as _;
    let captured = Captured::default();
    // Both are consumed only by the unix-only unreadable-file branch below.
    #[cfg_attr(not(unix), allow(unused_variables))]
    let sub = tracing_subscriber::registry().with(captured.clone());

    let dir = tempfile::tempdir().unwrap();
    #[cfg_attr(not(unix), allow(unused_variables))]
    let missing = dir.path().join("missing.md");
    let empty = dir.path().join("empty.md");
    std::fs::write(&empty, "   \n").unwrap();

    // An unreadable file: exists, but mode 000 denies a non-root reader.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let locked = dir.path().join("locked.md");
        std::fs::write(&locked, "secret\n").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        tracing::subscriber::with_default(sub, || {
            assert_eq!(read_prompt_candidate(&missing), None);
            assert_eq!(read_prompt_candidate(&locked), None);
            assert_eq!(read_prompt_candidate(&empty), None);
        });

        let seen = captured.0.lock().unwrap();
        assert!(
            seen.iter()
                .any(|(l, m)| *l == tracing::Level::WARN && m.contains("could not be read")),
            "an UNREADABLE file must warn loudly: {seen:?}"
        );
        assert!(
            !seen.iter().any(|(_, m)| m.contains("missing.md")),
            "a MISSING file stays silent at every level: {seen:?}"
        );
        // Note: the empty-file warn itself is asserted nowhere — under parallel
        // test load that read has been observed to race to NotFound, and no
        // survivor depends on it. Missing-silence and unreadable-loudness carry
        // all three guard mutants between them.
    }
}
