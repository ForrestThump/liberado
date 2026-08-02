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
    /// True when the model has no price entry (or empty rates). Distinct from "usage absent".
    pub unpriced_model: bool,
}

/// Price a single journal event.
///
/// Rules:
/// - No rates for `event.model` → `cost_usd = None`, `unpriced_model = true`.
/// - `prompt_tokens` and `completion_tokens` both absent → `cost_usd = None` (unknown usage).
/// - Cached portion uses `cached_input` when set; when unset, falls back to `input` so a partial
///   price table still yields money rather than inventing a free cache.
/// - Uncached prompt = `prompt - cached` (cached clamped to prompt).
/// - Rates are USD per 1_000_000 tokens.
pub fn price_event(event: &JournalEvent, prices: &PriceTable) -> PricedEvent {
    let rates = prices.get(&event.model);
    let unpriced_model = rates.map(|r| !r.is_priced()).unwrap_or(true);
    if unpriced_model {
        return PricedEvent {
            event: event.clone(),
            cost_usd: None,
            unpriced_model: true,
        };
    }
    let rates = rates.expect("priced");

    let prompt = event.prompt_tokens;
    let completion = event.completion_tokens;
    // Nothing to price when the provider reported no usage at all.
    if prompt.is_none() && completion.is_none() {
        return PricedEvent {
            event: event.clone(),
            cost_usd: None,
            unpriced_model: false,
        };
    }

    // Need at least the rates we will apply. Missing input with prompt tokens ⇒ cannot price.
    let prompt_tokens = prompt.unwrap_or(0);
    let completion_tokens = completion.unwrap_or(0);
    let cached = event.cached_prompt_tokens.unwrap_or(0).min(prompt_tokens);
    let uncached = prompt_tokens.saturating_sub(cached);

    if (uncached > 0 || (prompt_tokens > 0 && event.cached_prompt_tokens.is_none()))
        && rates.input.is_none()
    {
        // Have prompt tokens but no input rate.
        if uncached > 0 || cached == 0 {
            return PricedEvent {
                event: event.clone(),
                cost_usd: None,
                unpriced_model: true,
            };
        }
    }
    if completion_tokens > 0 && rates.output.is_none() {
        return PricedEvent {
            event: event.clone(),
            cost_usd: None,
            unpriced_model: true,
        };
    }
    if cached > 0 && rates.cached_input.is_none() && rates.input.is_none() {
        return PricedEvent {
            event: event.clone(),
            cost_usd: None,
            unpriced_model: true,
        };
    }

    let input_rate = rates.input.unwrap_or(0.0);
    let output_rate = rates.output.unwrap_or(0.0);
    let cached_rate = rates.cached_input.unwrap_or(input_rate);

    let usd = (f64::from(uncached) * input_rate
        + f64::from(cached) * cached_rate
        + f64::from(completion_tokens) * output_rate)
        / 1_000_000.0;

    PricedEvent {
        event: event.clone(),
        cost_usd: Some(usd),
        unpriced_model: false,
    }
}
