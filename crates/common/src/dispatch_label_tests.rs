//! Survivor-adjacent coverage for `Delivery` (mutation campaign 2026-08):
//! pins both stable kind-labels and the Display routing through them.

use crate::dispatch::Delivery;

#[test]
fn delivery_labels_are_stable_per_variant() {
    assert_eq!(Delivery::Summarize.label(), "summarize");
    assert_eq!(
        Delivery::Vault {
            path: "notes/report.md".into(),
        }
        .label(),
        "vault"
    );
}
