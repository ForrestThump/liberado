//! Read-time pricing: rates × tokens → optional USD.

use std::collections::HashMap;

use liberado_common::{ModelProfile, ModelTokenPrices};

use crate::journal::JournalEvent;

/// Model name → per-million rates. Absent key or empty rates ⇒ unpriced.
pub type PriceTable = HashMap<String, ModelTokenPrices>;

/// Build a price table from declared `[[models]]` profiles (only entries with at least one rate).
pub fn price_table_from_models(models: &[ModelProfile]) -> PriceTable {
    let mut table = PriceTable::new();
    for m in models {
        if m.prices.is_priced() {
            table.insert(m.name.clone(), m.prices.clone());
        }
    }
    table
}

/// One event after applying rates at read time.
#[derive(Debug, Clone, PartialEq)]
pub struct PricedEvent {
    pub event: JournalEvent,
    /// `None` when the model is unpriced, rates are incomplete for the usage present, or token
    /// usage itself is absent (cannot price what was not reported).
    pub cost_usd: Option<f64>,
    /// True when the **rates** cannot price this call: no entry for the model, empty rates, or an
    /// entry missing a rate the usage actually needs (`output` set but not `input`, say). Such a
    /// call's tokens belong on the unpriced line rather than in a money total.
    ///
    /// False when usage itself was absent — that is the provider reporting nothing, not a pricing
    /// gap, and it is deliberately a different condition. Both leave `cost_usd` as `None`.
    pub cost_unknown: bool,
}

/// Price a single journal event.
///
/// Rules:
/// - No rates for `event.model` → `cost_usd = None`, `cost_unknown = true`.
/// - `prompt_tokens` and `completion_tokens` both absent → `cost_usd = None`, but
///   `cost_unknown = false`: the provider reported no usage, which is not a pricing gap.
/// - A rate the usage needs but the entry lacks → `cost_usd = None`, `cost_unknown = true`. The
///   missing side is never quietly priced at zero.
/// - Cached portion uses `cached_input` when set; when unset, falls back to `input`, so a table
///   that omits only the cache rate still yields money rather than inventing a free cache.
/// - Uncached prompt = `prompt - cached` (cached clamped to prompt).
/// - Rates are USD per 1_000_000 tokens.
pub fn price_event(event: &JournalEvent, prices: &PriceTable) -> PricedEvent {
    let unknown = |cost_unknown| PricedEvent {
        event: event.clone(),
        cost_usd: None,
        cost_unknown,
    };

    let Some(rates) = prices.get(&event.model).filter(|r| r.is_priced()) else {
        return unknown(true);
    };

    // Nothing to price when the provider reported no usage at all.
    if event.prompt_tokens.is_none() && event.completion_tokens.is_none() {
        return unknown(false);
    }

    let prompt_tokens = event.prompt_tokens.unwrap_or(0);
    let completion_tokens = event.completion_tokens.unwrap_or(0);
    let cached = event.cached_prompt_tokens.unwrap_or(0).min(prompt_tokens);
    let uncached = prompt_tokens.saturating_sub(cached);
    // Cached tokens are input tokens; a table that prices input but not the cache still prices them.
    let cached_rate = rates.cached_input.or(rates.input);

    // Every rate we are about to apply to a non-zero count must exist. Otherwise the call is
    // unpriceable — reporting the priced fraction alone would understate it.
    if (uncached > 0 && rates.input.is_none())
        || (cached > 0 && cached_rate.is_none())
        || (completion_tokens > 0 && rates.output.is_none())
    {
        return unknown(true);
    }

    // Each `unwrap_or(0.0)` below is reachable only where the token count is zero.
    let usd = (f64::from(uncached) * rates.input.unwrap_or(0.0)
        + f64::from(cached) * cached_rate.unwrap_or(0.0)
        + f64::from(completion_tokens) * rates.output.unwrap_or(0.0))
        / 1_000_000.0;

    PricedEvent {
        event: event.clone(),
        cost_usd: Some(usd),
        cost_unknown: false,
    }
}
