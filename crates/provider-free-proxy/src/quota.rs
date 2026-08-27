//! Conservative remaining-quota checks for providers that might start billing after a cap.
//!
//! Rate-limited free endpoints (429, not a bill) do not need this: failover already walks the
//! ranking. Quota-then-pay vendors are omitted from the catalog unless a remaining figure is
//! known *and* would cover another request. Unknown remaining is treated as "do not send" —
//! guessing would be a charge.

/// How a vendor bills. This is the gate that keeps paid leftovers off the ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingKind {
    /// 429 / rate-limit is failover, never a bill (Groq, NVIDIA playground, Cerebras free,
    /// OpenRouter `:free`).
    RateLimitedFree,
    /// Only rows whose pricing both parse to zero (OpenRouter, Kilo, Mistral). Missing or
    /// unparseable pricing is paid.
    ZeroPricedOnly,
    /// Free up to a cap, then a charge (Cloudflare Workers AI neurons). Omitted unless a
    /// remaining-quota figure is known; this crate does not invent one, so the adapter is
    /// skipped.
    QuotaThenPay,
}

/// Observed remaining quota for one provider, if any.
///
/// `None` means we do not know. For [`BillingKind::QuotaThenPay`] that is a skip, not a green
/// light: a request we cannot bound is a request that could bill.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaBudget {
    pub remaining: Option<u64>,
}

impl QuotaBudget {
    pub fn unknown() -> Self {
        Self { remaining: None }
    }

    pub fn remaining(units: u64) -> Self {
        Self {
            remaining: Some(units),
        }
    }

    /// Whether sending one more request is known not to bill.
    ///
    /// Rate-limited free and zero-priced-only catalogs may always send (a 429 is failover).
    /// Quota-then-pay may send only when remaining is a positive count.
    pub fn allows_request(&self, billing: BillingKind) -> bool {
        match billing {
            BillingKind::RateLimitedFree | BillingKind::ZeroPricedOnly => true,
            BillingKind::QuotaThenPay => self.remaining.is_some_and(|n| n > 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_free_always_allows_a_request() {
        let unknown = QuotaBudget::unknown();
        assert!(unknown.allows_request(BillingKind::RateLimitedFree));
        assert!(unknown.allows_request(BillingKind::ZeroPricedOnly));
    }

    #[test]
    fn quota_then_pay_skips_when_remaining_is_unknown() {
        assert!(!QuotaBudget::unknown().allows_request(BillingKind::QuotaThenPay));
    }

    #[test]
    fn quota_then_pay_skips_at_zero_and_allows_a_positive_count() {
        assert!(!QuotaBudget::remaining(0).allows_request(BillingKind::QuotaThenPay));
        assert!(QuotaBudget::remaining(1).allows_request(BillingKind::QuotaThenPay));
    }
}
