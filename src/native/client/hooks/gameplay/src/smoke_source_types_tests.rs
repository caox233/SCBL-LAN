
use super::*;
#[test]
fn source_types_distinguish_other_sources_without_addresses_or_unbounded_growth() {
    let mut entries = [None; SOURCE_TYPE_LIMIT];
    let key = SourceTypes { entry: 0, source: native_type_rva(Some(SMOKE)), component: 0, network: 0, reciprocal: false };
    assert!(claim_source_type(&mut entries, key));
    assert!(!claim_source_type(&mut entries, key));
    assert!(claim_source_type(&mut entries, SourceTypes { source: native_type_rva(Some(ADV_SMOKE)), ..key }));
    for i in 2..SOURCE_TYPE_LIMIT { assert!(claim_source_type(&mut entries, SourceTypes { component: i as u32, ..key })); }
    assert!(!claim_source_type(&mut entries, SourceTypes { component: 999, ..key }));
    assert_eq!(entries.iter().flatten().count(), 32);
}
#[test]
fn source_types_only_encode_pinned_readonly_native_tables() {
    for v in [None, Some(0), Some(0x00401000), Some(0x02972fff), Some(0x03104ad6), Some(0xf0100000)] { assert_eq!(native_type_rva(v), 0); }
    assert_eq!(native_type_rva(Some(SMOKE)), SMOKE - 0x00400000);
    assert_eq!(native_type_rva(Some(ADV_SMOKE)), ADV_SMOKE - 0x00400000);
    assert_eq!(native_type_rva(Some(SMOKE_SPHERE)), SMOKE_SPHERE - 0x00400000);
}

#[test]
fn source_types_repeated_sources_exhaust_total_read_budget_without_wrapping() {
    let attempts = std::sync::atomic::AtomicU32::new(0);
    for expected in 0..SOURCE_TYPE_ATTEMPT_LIMIT { assert_eq!(source_type_attempt(&attempts), Some(expected)); }
    for _ in 0..4096 { assert_eq!(source_type_attempt(&attempts), None); }
    assert_eq!(attempts.load(Ordering::Relaxed), SOURCE_TYPE_ATTEMPT_LIMIT);
}
