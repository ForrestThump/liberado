//! Risk-waiver setter kept off the crate root so `lib.rs` stays at its function baseline.

use liberado_common::RiskWaiverSet;

use super::DispatchPack;

impl DispatchPack {
    /// Set the risk-waiver set propagated to every dispatch the pack runs. Mirrors the
    /// orchestrator's own `with_risk_waivers` so the two enforcement points see one set.
    pub fn with_risk_waivers(mut self, waivers: RiskWaiverSet) -> Self {
        self.risk_waivers = waivers;
        self
    }
}
