//! Preserve the per-loadout ItemName/Upgrades table, not the global item catalog.
//! DX11 EF96C0 serializes entry+44 (24-byte rows); F08700 copies it via F043D0.
//! Generated shadow loadouts are excluded from retail saves, so their +44 table
//! must travel with the existing sidecar. Never grant ownership or equip all.
use super::{push_u16, take_u16, take_bytes, NativeValueVector};
use crate::equipment_native::{readable, writable};
use sha2::{Digest, Sha256};

const OFFSET: usize = 0x44;
const ROW_BYTES: usize = 24;
// A loadout stores remembered selections for the complete item catalog, not
// merely its equipped weapons. Retail live rows already number 95 (including
// armor camos). Keep a finite bound without rejecting a normal native table.
const MAX_ROWS: usize = 256;
const MAX_SELECTIONS: usize = 32;
const MAX_ID: usize = 256;
const LEGACY_ABSENT: u16 = u16::MAX;
const NATIVE_ASSIGN: usize = 0x00F0_43D0;
const ASSIGN_BYTES: usize = 241;
const ASSIGN_SHA256: &str = "65a66007ecb82ac2146be371b584d0a3d266cdfd93fb32b3873f80fe926ac4aa";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Selection {
    pub item_id: Vec<u8>,
    pub upgrade_ids: Vec<Vec<u8>>,
}

fn valid_id(id: &[u8]) -> bool {
    // Native F03AC0/F0FE20 retain empty ItemName and selected-attachment slots.
    // Do not filter them: one empty selection is not an empty selection vector.
    id.len() <= MAX_ID && !id.contains(&0)
}
fn valid(rows: &[Selection]) -> bool {
    rows.len() <= MAX_ROWS && rows.iter().all(|r| valid_id(&r.item_id)
        && r.upgrade_ids.len() <= MAX_SELECTIONS
        && r.upgrade_ids.iter().all(|x| valid_id(x)))
}
fn put_id(out: &mut Vec<u8>, id: &[u8]) {
    push_u16(out, id.len() as u16);
    out.extend_from_slice(id);
}
fn get_id(input: &[u8], at: &mut usize, allow_empty: bool) -> Result<Vec<u8>, &'static str> {
    let n = take_u16(input, at)? as usize;
    if (!allow_empty && n == 0) || n > MAX_ID { return Err("invalid attachment identifier"); }
    let id = take_bytes(input, at, n)?;
    if !valid_id(id) { return Err("invalid attachment identifier"); }
    Ok(id.to_vec())
}
pub(super) fn encode(out: &mut Vec<u8>, rows: Option<&[Selection]>) -> Result<(), &'static str> {
    let Some(rows) = rows else { push_u16(out, LEGACY_ABSENT); return Ok(()); };
    if !valid(rows) { return Err("invalid per-loadout attachment table"); }
    push_u16(out, rows.len() as u16);
    for row in rows {
        put_id(out, &row.item_id);
        push_u16(out, row.upgrade_ids.len() as u16);
        for id in &row.upgrade_ids { put_id(out, id); }
    }
    Ok(())
}
pub(super) fn decode_versioned(input: &[u8], at: &mut usize, version: u16) -> Result<Option<Vec<Selection>>, &'static str> {
    let allow_empty = version >= 3;
    let n = take_u16(input, at)?;
    if n == LEGACY_ABSENT { return Ok(None); }
    if n as usize > MAX_ROWS { return Err("too many attachment rows"); }
    let mut rows = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let item_id = get_id(input, at, allow_empty)?;
        let count = take_u16(input, at)? as usize;
        if count > MAX_SELECTIONS { return Err("too many selected attachments"); }
        let upgrade_ids = (0..count).map(|_| get_id(input, at, allow_empty)).collect::<Result<Vec<_>, _>>()?;
        rows.push(Selection { item_id, upgrade_ids });
    }
    Ok(Some(rows))
}

#[cfg(test)]
pub(super) fn decode(input: &[u8], at: &mut usize) -> Result<Option<Vec<Selection>>, &'static str> {
    decode_versioned(input, at, 3)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CaptureFailure {
    pub stage: &'static str,
    pub detail: &'static str,
    pub row: usize,
    pub child: usize,
    pub length: usize,
    pub capacity: usize,
}
// No pointer, item ID or player text enters this diagnostic value.
fn failure(stage: &'static str, detail: &'static str, row: usize, child: usize,
    length: usize, capacity: usize) -> CaptureFailure {
    CaptureFailure { stage, detail, row, child, length, capacity }
}
// A native heap vector can cross adjacent committed VirtualQuery regions.
// The common render-side predicate intentionally accepts only one region; do
// not relax it globally. Here, validate every page of the bounded save field.
// Windows x86 VM region boundaries are 4 KiB aligned. No memory is dereferenced
// until the complete span passes, and guard/no-access/uncommitted tails fail.
unsafe fn accessible_span(p: usize, n: usize, probe: unsafe fn(usize, usize) -> bool) -> bool {
    if p < 0x10000 || n == 0 { return false; }
    let Some(end) = p.checked_add(n) else { return false; };
    if probe(p, n) { return true; }
    let mut cursor = p;
    while cursor < end {
        let bytes = (0x1000 - (cursor & 0xfff)).min(end - cursor);
        if !probe(cursor, bytes) { return false; }
        cursor += bytes;
    }
    true
}
unsafe fn checked_vector(address: usize, stride: usize, max: usize, stage: &'static str,
    row: usize, child: usize) -> Result<(usize, usize, usize), CaptureFailure> {
    let header = || failure(stage, "header", row, child, 0, 0);
    if !accessible_span(address, 12, readable) { return Err(header()); }
    let fields = address as *const u32;
    let data = fields.read_unaligned() as usize;
    let n = fields.add(1).read_unaligned() as usize;
    let cap = fields.add(2).read_unaligned() as usize;
    if n > max || n > cap || cap > 8192 {
        return Err(failure(stage, "bounds", row, child, n, cap));
    }
    if n > 0 && !accessible_span(data, n.checked_mul(stride).ok_or_else(header)?, readable) {
        return Err(failure(stage, "storage", row, child, n, cap));
    }
    Ok((data, n, cap))
}
unsafe fn vector(address: usize, stride: usize, max: usize) -> Option<(usize, usize)> {
    checked_vector(address, stride, max, "table", 0, 0).ok().map(|(data, n, _)| (data, n))
}
unsafe fn read_id(address: usize, stage: &'static str, row: usize, child: usize) -> Result<Vec<u8>, CaptureFailure> {
    let (data, n, cap) = checked_vector(address, 1, MAX_ID + 1, stage, row, child)?;
    // The native FString constructor represents empty input as {0,0,0}.
    // Do not form a slice from its null pointer, even with length zero.
    if n == 0 { return Ok(Vec::new()); }
    let text = std::slice::from_raw_parts(data as *const u8, n);
    if text[n - 1] != 0 { return Err(failure(stage, "terminator", row, child, n, cap)); }
    if !valid_id(&text[..n - 1]) { return Err(failure(stage, "identifier", row, child, n, cap)); }
    Ok(text[..n - 1].to_vec())
}
pub(super) unsafe fn capture_checked(entry: *const u8) -> Result<Vec<Selection>, CaptureFailure> {
    if entry.is_null() { return Err(failure("entry", "null", 0, 0, 0, 0)); }
    let address = (entry as usize).checked_add(OFFSET)
        .ok_or_else(|| failure("entry", "overflow", 0, 0, 0, 0))?;
    let (rows, count, _) = checked_vector(address, ROW_BYTES, MAX_ROWS, "table", 0, 0)?;
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let row = rows + i * ROW_BYTES;
        let item_id = read_id(row, "item", i, 0)?;
        let (children, n, _) = checked_vector(row + 12, 12, MAX_SELECTIONS, "children", i, 0)?;
        let upgrade_ids = (0..n).map(|j| read_id(children + j * 12, "child", i, j))
            .collect::<Result<Vec<_>, _>>()?;
        result.push(Selection { item_id, upgrade_ids });
    }
    Ok(result)
}
#[cfg(test)]
pub(super) unsafe fn capture(entry: *const u8) -> Option<Vec<Selection>> {
    capture_checked(entry).ok()
}

type SetString = unsafe extern "thiscall" fn(*mut u8, *const u8) -> *mut u8;
type ResetString = unsafe extern "thiscall" fn(*mut u8);
type Assign = unsafe extern "thiscall" fn(*mut u8, *const u8) -> *mut u8;
struct Prepared {
    rows: Vec<[usize; 6]>,
    children: Vec<Vec<[usize; 3]>>,
    reset: ResetString,
}
impl Drop for Prepared {
    fn drop(&mut self) { unsafe {
        for row in &mut self.rows { (self.reset)(row.as_mut_ptr().cast()); }
        for group in &mut self.children {
            for text in group { (self.reset)(text.as_mut_ptr().cast()); }
        }
    }}
}
unsafe fn set_id(dst: *mut u8, id: &[u8], set: SetString) -> bool {
    let mut terminated = id.to_vec(); terminated.push(0);
    !set(dst, terminated.as_ptr()).is_null()
}
unsafe fn prepare(rows: &[Selection], set: SetString, reset: ResetString) -> Option<Prepared> {
    if !valid(rows) { return None; }
    let mut staged = Prepared { rows: vec![[0; 6]; rows.len()],
        children: rows.iter().map(|r| vec![[0; 3]; r.upgrade_ids.len()]).collect(), reset };
    for (i, row) in rows.iter().enumerate() {
        if !set_id(staged.rows[i].as_mut_ptr().cast(), &row.item_id, set) { return None; }
        for (j, id) in row.upgrade_ids.iter().enumerate() {
            if !set_id(staged.children[i][j].as_mut_ptr().cast(), id, set) { return None; }
        }
        staged.rows[i][3] = staged.children[i].as_ptr() as usize;
        staged.rows[i][4] = staged.children[i].len();
        staged.rows[i][5] = staged.children[i].len();
    }
    Some(staged)
}
unsafe fn apply_with(entry: *mut u8, rows: Option<&[Selection]>, set: SetString,
    reset: ResetString, assign: Assign) -> bool {
    let Some(rows) = rows else { return true; };
    let Some(address) = (entry as usize).checked_add(OFFSET) else { return false; };
    if entry.is_null() || !accessible_span(address, 12, writable) || vector(address, ROW_BYTES, MAX_ROWS).is_none() { return false; }
    let Some(mut staged) = prepare(rows, set, reset) else { return false; };
    let source = NativeValueVector { data: staged.rows.as_mut_ptr().cast(),
        len: staged.rows.len(), capacity: staged.rows.len() };
    // Native deep assignment frees/replaces the destination's existing rows.
    // Rust owns only these temporary headers; all FString storage is released
    // after the native copy. No pointers into this staging buffer survive.
    assign(address as *mut u8, (&source as *const NativeValueVector).cast());
    true
}
pub(super) unsafe fn apply(entry: *mut u8, rows: Option<&[Selection]>) -> bool {
    if rows.is_none() { return true; }
    if !readable(NATIVE_ASSIGN, ASSIGN_BYTES) { return false; }
    let bytes = std::slice::from_raw_parts(NATIVE_ASSIGN as *const u8, ASSIGN_BYTES);
    if format!("{:x}", Sha256::digest(bytes)) != ASSIGN_SHA256 {
        tracing::error!("SCBL_EXTENDED_LOADOUTS event=attachment-restore result=retail-assign-mismatch");
        return false;
    }
    apply_with(entry, rows, std::mem::transmute(super::NATIVE_STRING_FROM_ANSI),
        std::mem::transmute(super::NATIVE_STRING_RESET), std::mem::transmute(NATIVE_ASSIGN))
}

#[cfg(test)]
#[path = "extended_loadout_upgrades_tests.rs"]
mod tests;
