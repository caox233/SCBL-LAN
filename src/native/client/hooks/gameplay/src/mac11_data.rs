//! Exact retail identity, not a synthetic weapon or attachment catalog.
pub(super) const RETAIL_ID: u32 = 1716735579;
pub(super) const RETAIL_NAME: &[u8] = b"ADV_Spy_SMG_MAC11";
// R33/R34 wrote this name into a real MOD save. Resolve old references to
// existing equivalent retail records, without adding selectable duplicates.
// No alias for the SP-only extended magazine or hollow-point upgrade.
pub(super) fn legacy_id(id: u32) -> Option<u32> {
    Some(match id {
        1424978178 => RETAIL_ID,
        929156680 => 683876750,
        884690441 => 844490966,
        2701629778 => 1525804001,
        1942604074 => 1036918527,
        2010385489 => 818377621,
        705715625 => 2407719902,
        1555799823 => 1521203290,
        2534561880 => 527248164,
        _ => return None,
    })
}
