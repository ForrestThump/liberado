use std::slice;

use proptest::prelude::*;
use serde_json::Value;

use super::*;
use crate::catalog::McpDescriptor;
use crate::{Consequence, WriteClass};

// ── Generation primitives ─────────────────────────────────────────────

/// A tool name as it appears in `write_tools` / `tool_zones` keys.
const TOOL_NAME: &str = "[a-zA-Z0-9_/-]{1,20}";

/// A vault zone name: a plain 1–20 char alphanumeric string.
const ZONE_NAME: &str = "[a-zA-Z0-9]{1,20}";

/// A zone-name wrapper so generation carries intent. `McpDescriptor` stores zones as plain
/// `String`; this newtype only exists to give the strategy a name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Zone(String);

impl Arbitrary for Zone {
    type Parameters = ();
    type Strategy = BoxedStrategy<Zone>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        ZONE_NAME.prop_map(Zone).boxed()
    }
}

impl Arbitrary for WriteClass {
    type Parameters = ();
    type Strategy = BoxedStrategy<WriteClass>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        prop_oneof![
            Just(WriteClass::HumanOnly),
            Just(WriteClass::AgentWritable),
            Just(WriteClass::ProposalOnly),
            Just(WriteClass::Shared),
        ]
        .boxed()
    }
}

impl Arbitrary for McpDescriptor {
    type Parameters = ();
    type Strategy = BoxedStrategy<McpDescriptor>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        // `write_tools` is only consulted by `write_target` inside the `zone_from_arg`
        // (path-addressed) branch. A fixed-zone descriptor (`zone_from_arg: None`) ignores
        // `write_tools` and resolves through `resolve_zone`, which reports `NotAWrite` for a
        // tool with no default/tool zone. Correlating the two keeps "a declared write tool
        // is never `NotAWrite`" a genuine invariant rather than a vacuous one.
        let zone_from_arg_strategy = prop_oneof![
            2 => Just(None::<String>),
            3 => "[a-zA-Z0-9_-]{1,20}".prop_map(Some),
        ];

        zone_from_arg_strategy
            .prop_flat_map(|zone_from_arg| {
                let write_tools: BoxedStrategy<Vec<String>> = match &zone_from_arg {
                    Some(_) => proptest::collection::vec(TOOL_NAME, 0..5).boxed(),
                    None => Just(Vec::<String>::new()).boxed(),
                };
                let read_tools: BoxedStrategy<Vec<String>> = match &zone_from_arg {
                    Some(_) => proptest::collection::vec(TOOL_NAME, 0..5).boxed(),
                    None => Just(Vec::<String>::new()).boxed(),
                };
                let vault_wide_writes: BoxedStrategy<Vec<String>> = match &zone_from_arg {
                    Some(_) => proptest::collection::vec(TOOL_NAME, 0..3).boxed(),
                    None => Just(Vec::<String>::new()).boxed(),
                };
                (
                    "[a-zA-Z0-9_-]{1,20}",  // name
                    "[a-zA-Z0-9_ -]{0,40}", // description
                    prop_oneof![
                        1 => Just(None::<String>),
                        2 => any::<Zone>().prop_map(|z| Some(z.0)),
                    ],
                    proptest::collection::vec(
                        (
                            TOOL_NAME,
                            prop_oneof![
                                1 => Just(None::<String>),
                                2 => any::<Zone>().prop_map(|z| Some(z.0)),
                            ],
                        ),
                        0..3,
                    ),
                    write_tools,
                    read_tools,
                    vault_wide_writes,
                )
                    .prop_map(
                        move |(
                            name,
                            description,
                            default_zone,
                            tool_zones,
                            write_tools,
                            read_tools,
                            vault_wide_writes,
                        )| {
                            McpDescriptor {
                                name,
                                description,
                                consequence: Consequence::Reversible,
                                provenance: None,
                                default_zone,
                                tool_zones,
                                zone_from_arg: zone_from_arg.clone(),
                                write_tools,
                                read_tools,
                                vault_wide_writes,
                                write_path_args: std::collections::BTreeMap::new(),
                                admin_tools: Vec::new(),
                            }
                        },
                    )
                    .boxed()
            })
            .boxed()
    }
}

/// A `serde_json::Value` strategy. `any::<serde_json::Value>()` requires proptest's optional
/// `serde_json` feature (not enabled here), so build one by hand: scalar leaves plus
/// recursive arrays/objects. String leaves deliberately allow `/` and `.` so `write_target`'s
/// path-segment logic is exercised.
fn arb_json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<f64>()
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(Value::from),
        "[a-zA-Z0-9_/.-]{0,30}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 16, 8, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            proptest::collection::hash_map("[a-zA-Z0-9_-]{0,12}", inner, 0..5).prop_map(|m| {
                let mut map = serde_json::Map::new();
                for (k, v) in m {
                    map.insert(k, v);
                }
                Value::Object(map)
            },),
        ]
    })
}

/// A write class that does **not** allow direct agent writes. Test 2's guard-equivalence
/// property asserts `(Some(zone), Some(zone))` for every resolved zone — the fail-closed
/// behavior this guard is specified to have. The "agent may write, so no restriction"
/// outcome is a separate behavior already pinned by the unit tests in `tests`.
fn restricted_write_class() -> impl Strategy<Value = WriteClass> {
    prop_oneof![Just(WriteClass::HumanOnly), Just(WriteClass::ProposalOnly)]
}

// ── Test 1: write_target never collapses a declared write tool to NotAWrite ──

fn write_target_never_notawrite(desc: McpDescriptor, tool: String, args: Value) -> bool {
    if desc.write_tools.contains(&tool) {
        !matches!(write_target(&desc, &tool, &args), WriteTarget::NotAWrite)
    } else {
        true
    }
}

proptest! {
    #[test]
    fn proptest_write_target_never_notawrite(
        desc in any::<McpDescriptor>(),
        tool in TOOL_NAME,
        args in arb_json_value(),
    ) {
        prop_assert!(write_target_never_notawrite(desc, tool, args));
    }
}

// ── Test 2: zone_write_restriction agrees with resolve_zone ──────────

/// `zone_write_restriction` looks its descriptor up by name in a *slice* catalog; the spec
/// check passes the single generated descriptor as that one-entry catalog
/// (`std::slice::from_ref`, adapting the real `&[McpDescriptor]` signature).
fn zone_write_restriction_agrees(
    mcp: String,
    tool: String,
    desc: McpDescriptor,
    zone_classes: Vec<(String, WriteClass)>,
) -> bool {
    let zone = resolve_zone(&desc, &tool);
    let r = zone_write_restriction(&mcp, &tool, slice::from_ref(&desc), &zone_classes);
    match (zone, r) {
        (None, None) => true,
        (Some(z), Some(rz)) => z == rz,
        _ => false,
    }
}

proptest! {
    #[test]
    fn proptest_zone_write_restriction_agrees_with_resolve_zone(
        // `mcp` must name the descriptor actually in the catalog, or the lookup is a
        // no-op and the equivalence degenerates to "always `None`".
        (mcp, desc) in any::<McpDescriptor>()
            .prop_map(|desc| (desc.name.clone(), desc)),
        tool in TOOL_NAME,
        // Restricted classes only, so a resolved zone always yields `Some(zone)` — exactly
        // the `(Some(z), Some(rz))` arm.
        zone_classes in proptest::collection::vec(
            (TOOL_NAME, restricted_write_class()),
            0..4,
        ),
    ) {
        prop_assert!(zone_write_restriction_agrees(mcp, tool, desc, zone_classes));
    }
}

// ── Test 3: classification is exhaustive for path-addressed MCPs ───────────────
//
// The 2026-10-11 live bug, pinned at the type level: a tool not in write_tools was NotAWrite
// (fail OPEN). The new rule is "fail CLOSED on undeclared tools" — every tool resolves to
// one of NotAWrite / Zone / VaultWide / Undeterminable, and Undeterminable is the answer for
// any tool not in any of the three classification lists.
fn classification_is_exhaustive(desc: McpDescriptor, tool: String, args: Value) -> bool {
    if desc.zone_from_arg.is_none() {
        // Fixed-zone MCPs keep the older "NotAWrite for the unlisted" semantics.
        return matches!(
            write_target(&desc, &tool, &args),
            WriteTarget::NotAWrite | WriteTarget::Zone(_)
        );
    }
    // Path-addressed: any of the four outcomes is acceptable; the structural property is
    // that NotAWrite is reachable only via the read_tools list — never by accident through
    // omission from write_tools.
    match write_target(&desc, &tool, &args) {
        WriteTarget::NotAWrite => {
            // Read classification must come from the explicit list.
            desc.read_tools.contains(&tool)
        }
        WriteTarget::VaultWide => desc.vault_wide_writes.contains(&tool),
        // Multi-zone writes are writes regardless of whether the call resolves one zone or
        // many — only Undeterminable is the fail-closed shape that's neither read, write,
        // nor vault-wide.
        WriteTarget::Zone(_) | WriteTarget::Zones(_) | WriteTarget::Undeterminable(_) => true,
    }
}

proptest! {
    #[test]
    fn proptest_path_addressed_classification_is_exhaustive(
        desc in any::<McpDescriptor>(),
        tool in TOOL_NAME,
        args in arb_json_value(),
    ) {
        prop_assert!(classification_is_exhaustive(desc, tool, args));
    }
}
