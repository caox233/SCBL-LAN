use std::ffi::c_void;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use retour::static_detour;
use sha2::Digest;
use sha2::Sha256;
use tracing::error;
use tracing::info;
use tracing::warn;

const SUPPORTED_WORLD_RAYCAST: usize = 0x0058_9860;
const LOADOUT_MANAGER_SERIALIZE: usize = 0x00F0_3CF0;
const LOADOUT_MANAGER_QUERY_KIND: usize = 0x00EE_56F0;
const ARCHIVE_IS_SAVING: usize = 0x0067_5850;
const GAME_ALLOCATE: usize = 0x004A_C7A0;
const NATIVE_VECTOR_RESIZE: usize = 0x0042_DAB0;
const NATIVE_STRING_RESET: usize = 0x0042_D7B0;
const NATIVE_STRING_FROM_ANSI: usize = 0x0042_E2A0;
const LOADOUT_CONSTRUCT: usize = 0x00F0_FB20;
const LOADOUT_ASSIGN: usize = 0x00F0_8700;
const LOADOUT_GEAR_ASSIGN: usize = 0x0045_3D60;

const LOADOUT_ARRAY_OFFSET: usize = 0x20;
const LOADOUT_COUNT_OFFSET: usize = 0x24;
const LOADOUT_CAPACITY_OFFSET: usize = 0x28;
// DX11 retail Serialize (00EF9F50): +0x10/+0x14/+0x18 is
// m_TEquippedLoadoutsMap; +0x64/+0x68/+0x6c is LoadoutItems (ItemName/Upgrades).
// Neither is a disposable lookup cache. Preserve both when appending shadows
// and when temporarily filtering the native Loadouts vector for serialization.
#[cfg(test)]
const LOADOUT_ITEMS_OFFSET: usize = 0x64;
#[cfg(test)]
const LOADOUT_ITEMS_COUNT_OFFSET: usize = 0x68;
#[cfg(test)]
const LOADOUT_ITEMS_CAPACITY_OFFSET: usize = 0x6C;
const LOADOUT_NAME_POINTER_OFFSET: usize = 0;
const LOADOUT_NAME_LENGTH_OFFSET: usize = 4;
const LOADOUT_DISPLAY_NAME_LENGTH_OFFSET: usize = 0x10;
const LOADOUT_DISPLAY_NAME_POINTER_OFFSET: usize = 0x0C;
const LOADOUT_LOCALIZATION_KEY_OFFSET: usize = 0x18;
const LOADOUT_GEAR_OFFSET: usize = 0x50;
const LOADOUT_UNLOCK_COST_A_OFFSET: usize = 0x34;
const MAX_REASONABLE_LOADOUTS: usize = 64;
const NATIVE_STRING_SIZE: usize = 12;
const SIDECAR_MAGIC: &[u8; 8] = b"SCBLLDO1";
const SIDECAR_VERSION: u16 = 3; // v3 preserves native empty attachment strings; v1/v2 remain readable.
#[path = "extended_loadout_upgrades.rs"]
mod upgrades;
const SIDECAR_RECORD_COUNT: usize = 6;
const MAX_DISPLAY_NAME_UNITS: usize = 128;
const MAX_GEAR_ITEMS: usize = 32;
const MAX_GEAR_ID_BYTES: usize = 256;
// Six complete per-loadout catalogs, with a bounded 256-row attachment table.
const MAX_SIDECAR_BYTES: usize = 512 * 1024;
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

static LAST_MANAGER: AtomicUsize = AtomicUsize::new(0);
static SIDECAR_LOADED_MANAGER: AtomicUsize = AtomicUsize::new(0);
static SIDECAR_REJECTED_MANAGER: AtomicUsize = AtomicUsize::new(0);
static SIDECAR_IO_ACTIVE: AtomicBool = AtomicBool::new(false);
// One detailed capture/apply failure per shadow slot per process: at most 12.
static FAILURE_DETAILS: AtomicUsize = AtomicUsize::new(0);
fn first_failure_detail(kind: u32, slot: u8, applying: bool) -> bool {
    if !(1..=2).contains(&kind) || slot >= 3 { return false; }
    let bit = 1usize << ((kind as usize - 1) * 3 + slot as usize + if applying { 6 } else { 0 });
    FAILURE_DETAILS.fetch_or(bit, Ordering::Relaxed) & bit == 0
}

#[link(name = "kernel32")]
extern "system" {
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

const EXTENDED: [&[u8]; 6] = [
    b"SCBL_EXTENDED_KIND1_1\0",
    b"SCBL_EXTENDED_KIND1_2\0",
    b"SCBL_EXTENDED_KIND1_3\0",
    b"SCBL_EXTENDED_KIND2_1\0",
    b"SCBL_EXTENDED_KIND2_2\0",
    b"SCBL_EXTENDED_KIND2_3\0",
];

const EDITABLE_LOADOUT_KINDS: [usize; 2] = [1, 2];

#[repr(C)]
struct NativePointerVector {
    data: *mut *mut u8,
    len: i32,
    capacity: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SidecarRecord {
    kind: u32,
    slot: u8,
    display_name: Vec<u16>,
    gear_ids: Vec<Vec<u8>>,
    // None is a legacy v1 record: retain native defaults, never invent selections.
    upgrades: Option<Vec<upgrades::Selection>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Sidecar {
    records: Vec<SidecarRecord>,
}

#[repr(C)]
struct NativeValueVector {
    data: *mut u8,
    len: usize,
    capacity: usize,
}

struct SidecarIoGuard;

impl SidecarIoGuard {
    fn acquire() -> Option<Self> {
        SIDECAR_IO_ACTIVE.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| Self)
    }
}

impl Drop for SidecarIoGuard {
    fn drop(&mut self) {
        SIDECAR_IO_ACTIVE.store(false, Ordering::Release);
    }
}

static_detour! {
    static LoadoutManagerSerializeHook: unsafe extern "thiscall" fn(*mut u8, usize, *mut c_void);
    static LoadoutManagerQueryKindHook: unsafe extern "thiscall" fn(*mut u8, usize, *mut NativePointerVector);
}

unsafe fn read_usize(base: *const u8, offset: usize) -> usize {
    base.add(offset).cast::<usize>().read_unaligned()
}

unsafe fn write_usize(base: *mut u8, offset: usize, value: usize) {
    base.add(offset).cast::<usize>().write_unaligned(value);
}

fn name_without_nul(name: &[u8]) -> &[u8] {
    name.strip_suffix(&[0]).unwrap_or(name)
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn take_bytes<'a>(input: &'a [u8], cursor: &mut usize, count: usize) -> Result<&'a [u8], &'static str> {
    let end = cursor.checked_add(count).ok_or("sidecar length overflow")?;
    let value = input.get(*cursor..end).ok_or("sidecar truncated")?;
    *cursor = end;
    Ok(value)
}

fn take_u8(input: &[u8], cursor: &mut usize) -> Result<u8, &'static str> {
    Ok(take_bytes(input, cursor, 1)?[0])
}

fn take_u16(input: &[u8], cursor: &mut usize) -> Result<u16, &'static str> {
    let bytes: [u8; 2] = take_bytes(input, cursor, 2)?.try_into().map_err(|_| "sidecar truncated")?;
    Ok(u16::from_le_bytes(bytes))
}

fn take_u32(input: &[u8], cursor: &mut usize) -> Result<u32, &'static str> {
    let bytes: [u8; 4] = take_bytes(input, cursor, 4)?.try_into().map_err(|_| "sidecar truncated")?;
    Ok(u32::from_le_bytes(bytes))
}

fn encode_sidecar(sidecar: &Sidecar) -> Result<Vec<u8>, &'static str> {
    if sidecar.records.len() != SIDECAR_RECORD_COUNT {
        return Err("sidecar requires six records");
    }
    let mut output = Vec::with_capacity(2048);
    output.extend_from_slice(SIDECAR_MAGIC);
    push_u16(&mut output, SIDECAR_VERSION);
    push_u16(&mut output, sidecar.records.len() as u16);
    for (index, record) in sidecar.records.iter().enumerate() {
        let expected_kind = EDITABLE_LOADOUT_KINDS[index / 3] as u32;
        let expected_slot = (index % 3) as u8;
        if record.kind != expected_kind || record.slot != expected_slot {
            return Err("sidecar record identity mismatch");
        }
        if record.display_name.len() > MAX_DISPLAY_NAME_UNITS || record.gear_ids.len() > MAX_GEAR_ITEMS {
            return Err("sidecar record exceeds bounds");
        }
        push_u32(&mut output, record.kind);
        output.push(record.slot);
        output.push(0);
        push_u16(&mut output, record.display_name.len() as u16);
        push_u16(&mut output, record.gear_ids.len() as u16);
        for unit in &record.display_name {
            push_u16(&mut output, *unit);
        }
        for gear_id in &record.gear_ids {
            if gear_id.is_empty() || gear_id.len() > MAX_GEAR_ID_BYTES || gear_id.contains(&0) {
                return Err("invalid gear identifier");
            }
            push_u16(&mut output, gear_id.len() as u16);
            output.extend_from_slice(gear_id);
        }
        upgrades::encode(&mut output, record.upgrades.as_deref())?;
    }
    if output.len() + 32 > MAX_SIDECAR_BYTES {
        return Err("sidecar exceeds maximum size");
    }
    let digest = Sha256::digest(&output);
    output.extend_from_slice(&digest);
    Ok(output)
}

fn decode_sidecar(input: &[u8]) -> Result<Sidecar, &'static str> {
    if input.len() < SIDECAR_MAGIC.len() + 4 + 32 || input.len() > MAX_SIDECAR_BYTES {
        return Err("invalid sidecar size");
    }
    let payload_len = input.len() - 32;
    let (payload, expected_digest) = input.split_at(payload_len);
    if &Sha256::digest(payload)[..] != expected_digest {
        return Err("sidecar checksum mismatch");
    }
    let mut cursor = 0;
    if take_bytes(payload, &mut cursor, SIDECAR_MAGIC.len())? != SIDECAR_MAGIC {
        return Err("sidecar magic mismatch");
    }
    let version = take_u16(payload, &mut cursor)?;
    if version != 1 && version != 2 && version != SIDECAR_VERSION {
        return Err("unsupported sidecar version");
    }
    if take_u16(payload, &mut cursor)? as usize != SIDECAR_RECORD_COUNT {
        return Err("sidecar record count mismatch");
    }
    let mut records = Vec::with_capacity(SIDECAR_RECORD_COUNT);
    for index in 0..SIDECAR_RECORD_COUNT {
        let kind = take_u32(payload, &mut cursor)?;
        let slot = take_u8(payload, &mut cursor)?;
        let reserved = take_u8(payload, &mut cursor)?;
        let display_len = take_u16(payload, &mut cursor)? as usize;
        let gear_count = take_u16(payload, &mut cursor)? as usize;
        if reserved != 0 || kind != EDITABLE_LOADOUT_KINDS[index / 3] as u32 || slot != (index % 3) as u8 || display_len > MAX_DISPLAY_NAME_UNITS || gear_count > MAX_GEAR_ITEMS {
            return Err("invalid sidecar record header");
        }
        let mut display_name = Vec::with_capacity(display_len);
        for _ in 0..display_len {
            display_name.push(take_u16(payload, &mut cursor)?);
        }
        let mut gear_ids = Vec::with_capacity(gear_count);
        for _ in 0..gear_count {
            let length = take_u16(payload, &mut cursor)? as usize;
            if length == 0 || length > MAX_GEAR_ID_BYTES {
                return Err("invalid gear identifier length");
            }
            let value = take_bytes(payload, &mut cursor, length)?.to_vec();
            if value.contains(&0) {
                return Err("gear identifier contains NUL");
            }
            gear_ids.push(value);
        }
        records.push(SidecarRecord {
            kind,
            slot,
            display_name,
            gear_ids,
            upgrades: if version >= 2 { upgrades::decode_versioned(payload, &mut cursor, version)? } else { None },
        });
    }
    if cursor != payload.len() {
        return Err("sidecar trailing data");
    }
    Ok(Sidecar { records })
}

fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source_wide = source.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let destination_wide = destination.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    let success = unsafe { MoveFileExW(source_wide.as_ptr(), destination_wide.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
    if success == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// Before the first v3 write, preserve an exact older-format save for downgrade.
// This runs only on the game's existing save path; no migration is done at load.
fn preserve_pre_v3_sidecar(path: &Path) -> std::io::Result<()> {
    use std::io::Read;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut previous = Vec::new();
    file.take(MAX_SIDECAR_BYTES as u64 + 1).read_to_end(&mut previous)?;
    if previous.len() < 10 || previous.len() > MAX_SIDECAR_BYTES || &previous[..8] != SIDECAR_MAGIC {
        return Ok(()); // The load/rejection guard, not this backup helper, validates saves.
    }
    let version = u16::from_le_bytes([previous[8], previous[9]]);
    if !matches!(version, 1 | 2) || decode_sidecar(&previous).is_err() { return Ok(()); }
    let backup = path.with_extension(format!("scbl-loadouts.pre-v3-{:x}.bak", Sha256::digest(&previous)));
    match fs::OpenOptions::new().write(true).create_new(true).open(&backup) {
        Ok(mut file) => { file.write_all(&previous)?; file.sync_all()?; }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut existing = Vec::new();
            File::open(&backup)?.take(MAX_SIDECAR_BYTES as u64 + 1).read_to_end(&mut existing)?;
            if existing != previous {
                return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "pre-v3 backup mismatch; original save retained"));
            }
        }
        Err(error) => return Err(error),
    }
    Ok(())
}


fn write_sidecar_atomic(path: &Path, sidecar: &Sidecar) -> Result<(), String> {
    let encoded = encode_sidecar(sidecar).map_err(str::to_owned)?;
    preserve_pre_v3_sidecar(path).map_err(|err| err.to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut temporary = path.to_path_buf();
    temporary.set_extension("scbl-loadouts.tmp");
    let result = (|| -> std::io::Result<()> {
        let mut file = File::create(&temporary)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|err| err.to_string())
}

unsafe fn entry_has_name(entry: *const u8, expected: &[u8]) -> bool {
    if entry.is_null() {
        return false;
    }
    let pointer = read_usize(entry, LOADOUT_NAME_POINTER_OFFSET) as *const u8;
    let length = read_usize(entry, LOADOUT_NAME_LENGTH_OFFSET);
    // The retail native string stores its length including the trailing NUL.
    // LOADOUT_CONSTRUCT does strlen(name) + 1 before assigning this field.
    if pointer.is_null() || length != expected.len() || length > 64 {
        return false;
    }
    std::slice::from_raw_parts(pointer, length) == expected
}

unsafe fn find_entry(manager: *const u8, name: &[u8]) -> *mut u8 {
    if manager.is_null() {
        return std::ptr::null_mut();
    }
    let entries = read_usize(manager, LOADOUT_ARRAY_OFFSET) as *const *mut u8;
    let count = read_usize(manager, LOADOUT_COUNT_OFFSET);
    if entries.is_null() || count > MAX_REASONABLE_LOADOUTS {
        return std::ptr::null_mut();
    }
    for index in 0..count {
        let entry = entries.add(index).read_unaligned();
        if entry_has_name(entry, name) {
            return entry;
        }
    }
    std::ptr::null_mut()
}

unsafe fn read_native_string(value: *const u8) -> Option<Vec<u8>> {
    let pointer = read_usize(value, 0) as *const u8;
    let length_with_nul = read_usize(value, 4);
    if pointer.is_null() || !(2..=MAX_GEAR_ID_BYTES + 1).contains(&length_with_nul) {
        return None;
    }
    let bytes = std::slice::from_raw_parts(pointer, length_with_nul);
    if bytes.last() != Some(&0) || bytes[..length_with_nul - 1].contains(&0) {
        return None;
    }
    Some(bytes[..length_with_nul - 1].to_vec())
}

unsafe fn capture_record(entry: *const u8, kind: u32, slot: u8) -> Option<SidecarRecord> {
    if entry.is_null() {
        return None;
    }
    let display_pointer = read_usize(entry, LOADOUT_DISPLAY_NAME_POINTER_OFFSET) as *const u16;
    let display_len = read_usize(entry, LOADOUT_DISPLAY_NAME_LENGTH_OFFSET);
    if display_len > MAX_DISPLAY_NAME_UNITS || (display_len > 0 && display_pointer.is_null()) {
        return None;
    }
    let display_name = if display_len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(display_pointer, display_len).to_vec()
    };

    let gear_vector = entry.add(LOADOUT_GEAR_OFFSET);
    let gear_data = read_usize(gear_vector, 0) as *const u8;
    let gear_count = read_usize(gear_vector, 4);
    if gear_count > MAX_GEAR_ITEMS || (gear_count > 0 && gear_data.is_null()) {
        return None;
    }
    let mut gear_ids = Vec::with_capacity(gear_count);
    for index in 0..gear_count {
        gear_ids.push(read_native_string(gear_data.add(index * NATIVE_STRING_SIZE))?);
    }
    let upgrades = match upgrades::capture_checked(entry) {
        Ok(value) => value,
        Err(failure) => {
            if first_failure_detail(kind, slot, false) {
                warn!("SCBL_EXTENDED_LOADOUTS event=sidecar-capture result=invalid-upgrades reason=native-contract kind={kind} slot={slot} stage={} detail={} row={} child={} length={} capacity={}",
                    failure.stage, failure.detail, failure.row, failure.child, failure.length, failure.capacity);
            }
            return None;
        }
    };
    Some(SidecarRecord {
        kind,
        slot,
        display_name,
        gear_ids,
        upgrades: Some(upgrades),
    })
}

unsafe fn capture_sidecar(manager: *const u8) -> Option<Sidecar> {
    if manager.is_null() {
        return None;
    }
    let mut records = Vec::with_capacity(SIDECAR_RECORD_COUNT);
    for (index, name) in EXTENDED.iter().enumerate() {
        let entry = find_entry(manager, name);
        records.push(capture_record(entry, EDITABLE_LOADOUT_KINDS[index / 3] as u32, (index % 3) as u8)?);
    }
    Some(Sidecar { records })
}

unsafe fn replace_display_name(entry: *mut u8, display_name: &[u16]) -> bool {
    if entry.is_null() || display_name.len() > MAX_DISPLAY_NAME_UNITS {
        return false;
    }
    let vector = entry.add(LOADOUT_DISPLAY_NAME_POINTER_OFFSET);
    let capacity = read_usize(vector, 8);
    if display_name.len() > capacity {
        write_usize(vector, 4, display_name.len());
        write_usize(vector, 8, next_capacity_for_mode(display_name.len(), false));
        let resize: unsafe extern "thiscall" fn(*mut u8, usize) = std::mem::transmute(NATIVE_VECTOR_RESIZE);
        resize(vector, std::mem::size_of::<u16>());
    } else {
        write_usize(vector, 4, display_name.len());
    }
    if display_name.is_empty() {
        return true;
    }
    let destination = read_usize(vector, 0) as *mut u16;
    if destination.is_null() {
        return false;
    }
    std::ptr::copy_nonoverlapping(display_name.as_ptr(), destination, display_name.len());
    true
}

unsafe fn replace_gear(entry: *mut u8, gear_ids: &[Vec<u8>]) -> bool {
    if entry.is_null() || gear_ids.len() > MAX_GEAR_ITEMS {
        return false;
    }
    let set_string: unsafe extern "thiscall" fn(*mut u8, *const u8) -> *mut u8 = std::mem::transmute(NATIVE_STRING_FROM_ANSI);
    let reset_string: unsafe extern "thiscall" fn(*mut u8) = std::mem::transmute(NATIVE_STRING_RESET);
    let assign_gear: unsafe extern "thiscall" fn(*mut u8, *const u8) = std::mem::transmute(LOADOUT_GEAR_ASSIGN);

    let mut strings = vec![[0usize; 3]; gear_ids.len()];
    let mut initialized = 0;
    for (index, gear_id) in gear_ids.iter().enumerate() {
        if gear_id.is_empty() || gear_id.len() > MAX_GEAR_ID_BYTES || gear_id.contains(&0) {
            break;
        }
        let mut terminated = Vec::with_capacity(gear_id.len() + 1);
        terminated.extend_from_slice(gear_id);
        terminated.push(0);
        if set_string(strings[index].as_mut_ptr().cast(), terminated.as_ptr()).is_null() {
            break;
        }
        initialized += 1;
    }
    if initialized != gear_ids.len() {
        for value in strings.iter_mut().take(initialized) {
            reset_string(value.as_mut_ptr().cast());
        }
        return false;
    }
    let source = NativeValueVector {
        data: strings.as_mut_ptr().cast(),
        len: strings.len(),
        capacity: strings.len(),
    };
    assign_gear(entry.add(LOADOUT_GEAR_OFFSET), (&source as *const NativeValueVector).cast());
    for value in &mut strings {
        reset_string(value.as_mut_ptr().cast());
    }
    true
}

unsafe fn apply_sidecar(manager: *mut u8, sidecar: &Sidecar) -> bool {
    if sidecar.records.len() != SIDECAR_RECORD_COUNT {
        return false;
    }
    for (index, record) in sidecar.records.iter().enumerate() {
        if record.kind != EDITABLE_LOADOUT_KINDS[index / 3] as u32 || record.slot != (index % 3) as u8 {
            return false;
        }
        let entry = find_entry(manager, EXTENDED[index]);
        let failed = if entry.is_null() { Some("entry") }
            else if !replace_display_name(entry, &record.display_name) { Some("display-name") }
            else if !replace_gear(entry, &record.gear_ids) { Some("gear") }
            else if !upgrades::apply(entry, record.upgrades.as_deref()) { Some("attachments") }
            else { None };
        if let Some(stage) = failed {
            if first_failure_detail(record.kind, record.slot, true) {
                warn!("SCBL_EXTENDED_LOADOUTS event=attachment-restore result=failed reason=native-contract kind={} slot={} stage={stage}", record.kind, record.slot);
            }
            return false;
        }
    }
    true
}

unsafe fn load_sidecar_once(manager: *mut u8) {
    if manager.is_null() || SIDECAR_LOADED_MANAGER.load(Ordering::Acquire) == manager as usize {
        return;
    }
    // A manager may be queried before both retail factions are available.
    // Missing shadows are transient readiness, not an invalid sidecar. Do not
    // mark this manager loaded/rejected until ensure can supply all six names.
    if EXTENDED.iter().any(|name| find_entry(manager, name).is_null()) {
        return;
    }
    let Some(_guard) = SidecarIoGuard::acquire() else {
        return;
    };
    let Some(path) = crate::extended_loadout_sidecar_path() else {
        warn!("SCBL_EXTENDED_LOADOUTS event=sidecar-load result=no-save-path");
        return;
    };
    let result = match fs::read(&path) {
        Ok(bytes) => decode_sidecar(&bytes)
            .map_err(str::to_owned)
            .and_then(|sidecar| apply_sidecar(manager, &sidecar).then_some(()).ok_or_else(|| "could not apply sidecar".to_owned())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            info!("SCBL_EXTENDED_LOADOUTS event=sidecar-load result=missing action=official-defaults");
            Ok(())
        }
        Err(err) => Err(err.to_string()),
    };
    match result {
        Ok(()) => {
            SIDECAR_REJECTED_MANAGER.store(0, Ordering::Release);
            SIDECAR_LOADED_MANAGER.store(manager as usize, Ordering::Release);
            info!("SCBL_EXTENDED_LOADOUTS event=sidecar-load result=ok");
        }
        Err(err) => {
            SIDECAR_REJECTED_MANAGER.store(manager as usize, Ordering::Release);
            SIDECAR_LOADED_MANAGER.store(manager as usize, Ordering::Release);
            warn!("SCBL_EXTENDED_LOADOUTS event=sidecar-load result=rejected action=official-defaults error={err}");
        }
    }
}

pub(crate) fn request_sidecar_reload() {
    SIDECAR_LOADED_MANAGER.store(0, Ordering::Release);
}

pub(crate) fn persist_sidecar(path: &Path) {
    let manager = LAST_MANAGER.load(Ordering::Acquire) as *const u8;
    if manager.is_null() {
        info!("SCBL_EXTENDED_LOADOUTS event=sidecar-save result=skipped reason=no-manager");
        return;
    }
    let Some(_guard) = SidecarIoGuard::acquire() else {
        warn!("SCBL_EXTENDED_LOADOUTS event=sidecar-save result=skipped reason=io-busy");
        return;
    };
    if SIDECAR_LOADED_MANAGER.load(Ordering::Acquire) != manager as usize {
        warn!("SCBL_EXTENDED_LOADOUTS event=sidecar-save result=skipped reason=sidecar-not-loaded");
        return;
    }
    if SIDECAR_REJECTED_MANAGER.load(Ordering::Acquire) == manager as usize {
        warn!("SCBL_EXTENDED_LOADOUTS event=sidecar-save result=skipped reason=rejected-sidecar");
        return;
    }
    let Some(sidecar) = (unsafe { capture_sidecar(manager) }) else {
        warn!("SCBL_EXTENDED_LOADOUTS event=sidecar-save result=skipped reason=incomplete-shadow-set");
        return;
    };
    match write_sidecar_atomic(path, &sidecar) {
        Ok(()) => info!("SCBL_EXTENDED_LOADOUTS event=sidecar-save result=ok records={SIDECAR_RECORD_COUNT}"),
        Err(err) => error!("SCBL_EXTENDED_LOADOUTS event=sidecar-save result=failed error={err}"),
    }
}

fn next_capacity_for_mode(count: usize, large_growth_mode: bool) -> usize {
    let proportional = count.saturating_add(count / 8).saturating_add(3);
    if large_growth_mode {
        count.saturating_add(8).min(proportional)
    } else {
        proportional
    }
}

unsafe fn append_entry(manager: *mut u8, entry: *mut u8) -> bool {
    let count = read_usize(manager, LOADOUT_COUNT_OFFSET);
    let new_count = count.saturating_add(1);
    if new_count > MAX_REASONABLE_LOADOUTS {
        return false;
    }
    write_usize(manager, LOADOUT_COUNT_OFFSET, new_count);
    let capacity = read_usize(manager, LOADOUT_CAPACITY_OFFSET);
    if new_count > capacity {
        let large_growth_mode = (0x0330_C290 as *const u32).read_unaligned() != 0;
        let new_capacity = next_capacity_for_mode(new_count, large_growth_mode);
        write_usize(manager, LOADOUT_CAPACITY_OFFSET, new_capacity);
        let resize: unsafe extern "thiscall" fn(*mut u8, usize) = std::mem::transmute(NATIVE_VECTOR_RESIZE);
        resize(manager.add(LOADOUT_ARRAY_OFFSET), std::mem::size_of::<usize>());
    }
    let entries = read_usize(manager, LOADOUT_ARRAY_OFFSET) as *mut *mut u8;
    if entries.is_null() {
        write_usize(manager, LOADOUT_COUNT_OFFSET, count);
        return false;
    }
    entries.add(count).write_unaligned(entry);
    // +0x14 is the equipped-map count, NOT a cache. Preserve it so retail
    // writes m_TEquippedLoadoutsMap(i) fields rather than omitting the group.
    true
}

unsafe fn apply_official_defaults(entry: *mut u8, official: *const u8) {
    let assign_gear: unsafe extern "thiscall" fn(*mut u8, *const u8) = std::mem::transmute(LOADOUT_GEAR_ASSIGN);
    assign_gear(entry.add(LOADOUT_GEAR_OFFSET), official.add(LOADOUT_GEAR_OFFSET));

    // A user rename lives in the UTF-16 display-name vector at +0x0c. A new
    // shadow starts with no rename, so retail falls through to the official
    // localization key. Keep the existing allocation for the editor to reuse.
    write_usize(entry, LOADOUT_DISPLAY_NAME_LENGTH_OFFSET, 0);
    let official_key = read_usize(official, LOADOUT_LOCALIZATION_KEY_OFFSET) as *const u8;
    let official_key_len = read_usize(official, LOADOUT_LOCALIZATION_KEY_OFFSET + 4);
    if !official_key.is_null() && official_key_len > 0 && official_key_len <= 256 {
        let reset: unsafe extern "thiscall" fn(*mut u8) = std::mem::transmute(NATIVE_STRING_RESET);
        let set: unsafe extern "thiscall" fn(*mut u8, *const u8) -> *mut u8 = std::mem::transmute(NATIVE_STRING_FROM_ANSI);
        reset(entry.add(LOADOUT_LOCALIZATION_KEY_OFFSET));
        set(entry.add(LOADOUT_LOCALIZATION_KEY_OFFSET), official_key);
    }
}

unsafe fn clone_entry(editable_template: *mut u8, official_defaults: *mut u8, internal_name: &[u8]) -> *mut u8 {
    let allocate: unsafe extern "C" fn(usize, usize, usize, usize, usize) -> *mut u8 = std::mem::transmute(GAME_ALLOCATE);
    let entry = allocate(0x60, 0x0298_C3A0, 0x21, 0, 0);
    if entry.is_null() {
        return std::ptr::null_mut();
    }
    let construct: unsafe extern "thiscall" fn(*mut u8, *const u8) -> *mut u8 = std::mem::transmute(LOADOUT_CONSTRUCT);
    let assign: unsafe extern "thiscall" fn(*mut u8, *const u8) -> *mut u8 = std::mem::transmute(LOADOUT_ASSIGN);
    let reset_name: unsafe extern "thiscall" fn(*mut u8) = std::mem::transmute(NATIVE_STRING_RESET);
    let set_name: unsafe extern "thiscall" fn(*mut u8, *const u8) -> *mut u8 = std::mem::transmute(NATIVE_STRING_FROM_ANSI);

    construct(entry, internal_name.as_ptr());
    // Clone the retail custom entry so editability, candidate pools and rename
    // behavior stay on the game's proven custom path. Only the initial gear and
    // display identity come from the corresponding official preset.
    assign(entry, editable_template);
    reset_name(entry.add(LOADOUT_NAME_POINTER_OFFSET));
    set_name(entry.add(LOADOUT_NAME_POINTER_OFFSET), internal_name.as_ptr());
    apply_official_defaults(entry, official_defaults);
    entry
}

struct SourceEntries {
    official: [*mut u8; 3],
    editable: [*mut u8; 3],
}

unsafe fn source_entries_for_kind(manager: *const u8, kind: usize) -> SourceEntries {
    let mut retail = Vec::with_capacity(6);
    if manager.is_null() {
        return SourceEntries {
            official: [std::ptr::null_mut(); 3],
            editable: [std::ptr::null_mut(); 3],
        };
    }
    let entries = read_usize(manager, LOADOUT_ARRAY_OFFSET) as *const *mut u8;
    let count = read_usize(manager, LOADOUT_COUNT_OFFSET);
    if entries.is_null() || count > MAX_REASONABLE_LOADOUTS {
        return SourceEntries {
            official: [std::ptr::null_mut(); 3],
            editable: [std::ptr::null_mut(); 3],
        };
    }

    // Retail presents each faction as three fixed presets followed by three
    // customizable entries. Generated shadows are excluded from both groups.
    for index in 0..count {
        let entry = entries.add(index).read_unaligned();
        if entry.is_null() || read_usize(entry, 0x28) != kind || EXTENDED.iter().any(|name| entry_has_name(entry, name)) {
            continue;
        }
        retail.push(entry);
    }
    if retail.len() < 6 {
        return SourceEntries {
            official: [std::ptr::null_mut(); 3],
            editable: [std::ptr::null_mut(); 3],
        };
    }
    let last = retail.len();
    SourceEntries {
        official: [retail[0], retail[1], retail[2]],
        editable: [retail[last - 3], retail[last - 2], retail[last - 1]],
    }
}

unsafe fn ensure_extended_loadouts(manager: *mut u8, reason: &str) -> usize {
    if manager.is_null() {
        return 0;
    }
    LAST_MANAGER.store(manager as usize, Ordering::Release);
    let count = read_usize(manager, LOADOUT_COUNT_OFFSET);
    let entries = read_usize(manager, LOADOUT_ARRAY_OFFSET);
    if entries == 0 || count == 0 || count > MAX_REASONABLE_LOADOUTS {
        warn!("SCBL_EXTENDED_LOADOUTS event=ensure reason={reason} manager={manager:?} count={count} result=invalid-manager");
        return 0;
    }

    let mut added = 0;
    for (kind_index, kind) in EDITABLE_LOADOUT_KINDS.into_iter().enumerate() {
        let sources = source_entries_for_kind(manager, kind);
        for slot in 0..3 {
            let official = sources.official[slot];
            let editable = sources.editable[slot];
            let extended_name = EXTENDED[kind_index * 3 + slot];
            if !find_entry(manager, extended_name).is_null() {
                continue;
            }
            if official.is_null() || editable.is_null() {
                warn!(
                    "SCBL_EXTENDED_LOADOUTS event=clone reason={reason} kind={kind} slot={slot} target={} result=missing-source",
                    String::from_utf8_lossy(name_without_nul(extended_name))
                );
                continue;
            }
            let clone = clone_entry(editable, official, extended_name);
            if clone.is_null() || !append_entry(manager, clone) {
                error!(
                    "SCBL_EXTENDED_LOADOUTS event=clone reason={reason} kind={kind} slot={slot} target={} result=failed",
                    String::from_utf8_lossy(name_without_nul(extended_name))
                );
                continue;
            }
            added += 1;
            info!(
                "SCBL_EXTENDED_LOADOUTS event=clone reason={reason} kind={kind} slot={slot} target={} official_entry={official:?} editable_template={editable:?} entry={clone:?} result=added",
                String::from_utf8_lossy(name_without_nul(extended_name))
            );
        }
    }
    added
}


unsafe fn unlock_multiplayer_loadouts(manager: *mut u8, reason: &str) -> usize {
    use crate::equipment_native::{readable, word, writable};
    let manager = manager as usize;
    let Some(entries) = word(manager, LOADOUT_ARRAY_OFFSET).map(|v| v as usize) else { return 0; };
    let Some(count) = word(manager, LOADOUT_COUNT_OFFSET).map(|v| v as usize) else { return 0; };
    if count > MAX_REASONABLE_LOADOUTS || !readable(entries, count * 4) { return 0; }
    let mut changed = 0;
    for index in 0..count {
        let entry = ((entries + index * 4) as *const u32).read_unaligned() as usize;
        if entry % 4 != 0 || !matches!(word(entry, 0x28), Some(1 | 2)) || !writable(entry + LOADOUT_UNLOCK_COST_A_OFFSET, 12) { continue; }
        let fields = std::slice::from_raw_parts_mut((entry + LOADOUT_UNLOCK_COST_A_OFFSET) as *mut u32, 3);
        if fields != [0, 0, 0] { fields.fill(0); changed += 1; }
    }
    if changed != 0 {
        info!("SCBL_EXTENDED_LOADOUTS event=unlock reason={reason} scope=all-spy-merc entries={changed}");
    }
    changed
}

fn visible_projection<T: Copy>(retail: [T; 6], shadows: [T; 3]) -> [T; 6] {
    [shadows[0], shadows[1], shadows[2], retail[3], retail[4], retail[5]]
}

unsafe fn project_visible_entries(output: *mut NativePointerVector, kind: usize) -> (usize, bool) {
    if output.is_null() {
        return (0, false);
    }
    let vector = &mut *output;
    if vector.data.is_null() || vector.len <= 0 || vector.len as usize > MAX_REASONABLE_LOADOUTS {
        return (0, false);
    }
    let original_len = vector.len as usize;
    let mut retail = Vec::with_capacity(6);
    let mut shadows = [std::ptr::null_mut(); 3];
    let group = EDITABLE_LOADOUT_KINDS.iter().position(|candidate| *candidate == kind);
    for read_index in 0..original_len {
        let entry = vector.data.add(read_index).read_unaligned();
        if let Some(group) = group {
            for (slot, name) in EXTENDED[group * 3..group * 3 + 3].iter().enumerate() {
                if entry_has_name(entry, name) {
                    shadows[slot] = entry;
                }
            }
        }
        if !EXTENDED.iter().any(|name| entry_has_name(entry, name)) {
            retail.push(entry);
        }
    }
    if group.is_some() && retail.len() >= 6 && shadows.iter().all(|entry| !entry.is_null()) {
        let retail = [retail[0], retail[1], retail[2], retail[3], retail[4], retail[5]];
        let projected = visible_projection(retail, shadows);
        for (index, entry) in projected.into_iter().enumerate() {
            vector.data.add(index).write_unaligned(entry);
        }
        vector.len = projected.len() as i32;
        return (original_len.saturating_sub(projected.len()), true);
    }

    // Fail safe: a partial shadow set must never create seven/eight entries.
    for (index, entry) in retail.iter().copied().enumerate() {
        vector.data.add(index).write_unaligned(entry);
    }
    vector.len = retail.len() as i32;
    (original_len.saturating_sub(retail.len()), false)
}

fn loadout_manager_serialize(this: *mut u8, argument: usize, archive: *mut c_void) {
    let manager = if this.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { read_usize(this, 8) as *mut u8 }
    };
    let writing_native_save = if archive.is_null() {
        false
    } else {
        let is_saving: unsafe extern "thiscall" fn(*mut c_void) -> bool = unsafe { std::mem::transmute(ARCHIVE_IS_SAVING) };
        unsafe { is_saving(archive) }
    };
    let (added, hidden, unlocked) = if writing_native_save {
        let added = unsafe { ensure_extended_loadouts(manager, "serialize-write") };
        unsafe { load_sidecar_once(manager) };
        if let Some(path) = crate::extended_loadout_sidecar_path() {
            persist_sidecar(&path);
        }
        let hidden = unsafe { serialize_without_extended_entries(this, manager, argument, archive) };
        let unlocked = unsafe { unlock_multiplayer_loadouts(manager, "serialize-write-after-retail") };
        (added, hidden, unlocked)
    } else {
        // During load the manager still contains only retail entries. Let the
        // game deserialize those first, then create and hydrate shadow slots.
        unsafe { LoadoutManagerSerializeHook.call(this, argument, archive) };
        request_sidecar_reload();
        let added = unsafe { ensure_extended_loadouts(manager, "serialize-read") };
        unsafe { load_sidecar_once(manager) };
        let unlocked = unsafe { unlock_multiplayer_loadouts(manager, "serialize-read-after-retail") };
        (added, 0, unlocked)
    };
    info!("SCBL_EXTENDED_LOADOUTS event=serialize manager={manager:?} native_write={writing_native_save} hidden={hidden} added={added} unlocked={unlocked}");
}

unsafe fn serialize_without_extended_entries(this: *mut u8, manager: *mut u8, argument: usize, archive: *mut c_void) -> usize {
    if manager.is_null() {
        LoadoutManagerSerializeHook.call(this, argument, archive);
        return 0;
    }
    let original_data = read_usize(manager, LOADOUT_ARRAY_OFFSET) as *mut *mut u8;
    let original_count = read_usize(manager, LOADOUT_COUNT_OFFSET);
    if original_data.is_null() || original_count > MAX_REASONABLE_LOADOUTS {
        LoadoutManagerSerializeHook.call(this, argument, archive);
        return 0;
    }

    let mut compatible = Vec::with_capacity(original_count);
    for index in 0..original_count {
        let entry = original_data.add(index).read_unaligned();
        if !EXTENDED.iter().any(|name| entry_has_name(entry, name)) {
            compatible.push(entry);
        }
    }
    let hidden = original_count.saturating_sub(compatible.len());

    // Filter only the generated Loadouts rows. Keep the native item table and
    // equipped map intact. MOD saves are isolated; shadow rows are recreated in
    // the same append order immediately after native deserialization, before
    // this wrapper returns. Their equipment remains in the validated sidecar.
    with_compatible_serialization_view(manager, compatible.as_mut_ptr(), compatible.len(), || {
        LoadoutManagerSerializeHook.call(this, argument, archive)
    });
    hidden
}

fn loadout_manager_query_kind(manager: *mut u8, kind: usize, output: *mut NativePointerVector) {
    let added = unsafe { ensure_extended_loadouts(manager, "query") };
    unsafe { load_sidecar_once(manager) };
    let unlocked = unsafe { unlock_multiplayer_loadouts(manager, "query-before-retail") };
    unsafe { LoadoutManagerQueryKindHook.call(manager, kind, output) };
    let (removed, projected) = unsafe { project_visible_entries(output, kind) };
    let result_count = if output.is_null() { -1 } else { unsafe { (*output).len } };
    info!(
        "SCBL_EXTENDED_LOADOUTS event=query manager={manager:?} kind={kind} layout=retail-six projected={projected} added={added} unlocked={unlocked} hidden_internal={removed} result_count={result_count}"
    );
}

unsafe fn with_compatible_serialization_view<R>(manager: *mut u8, compatible_data: *mut *mut u8, compatible_count: usize, serialize: impl FnOnce() -> R) -> R {


    let original_data = read_usize(manager, LOADOUT_ARRAY_OFFSET);
    let original_count = read_usize(manager, LOADOUT_COUNT_OFFSET);
    let original_capacity = read_usize(manager, LOADOUT_CAPACITY_OFFSET);



    write_usize(manager, LOADOUT_ARRAY_OFFSET, compatible_data as usize);
    write_usize(manager, LOADOUT_COUNT_OFFSET, compatible_count);
    write_usize(manager, LOADOUT_CAPACITY_OFFSET, compatible_count);

    let result = serialize();



    write_usize(manager, LOADOUT_ARRAY_OFFSET, original_data);
    write_usize(manager, LOADOUT_COUNT_OFFSET, original_count);
    write_usize(manager, LOADOUT_CAPACITY_OFFSET, original_capacity);
    result
}

pub unsafe fn init(world_raycast: usize) {
    if world_raycast != SUPPORTED_WORLD_RAYCAST {
        error!("SCBL_EXTENDED_LOADOUTS unsupported executable world_raycast={world_raycast:08X}");
        return;
    }
    let serialize_target = std::mem::transmute(LOADOUT_MANAGER_SERIALIZE);
    match LoadoutManagerSerializeHook
        .initialize(serialize_target, loadout_manager_serialize)
        .and_then(|hook| hook.enable())
    {
        Ok(_) => info!("SCBL_EXTENDED_LOADOUTS serialize_hook_enabled=true address={LOADOUT_MANAGER_SERIALIZE:08X}"),
        Err(err) => error!("SCBL_EXTENDED_LOADOUTS serialize_hook_enabled=false address={LOADOUT_MANAGER_SERIALIZE:08X} error={err:?}"),
    }
    let query_target = std::mem::transmute(LOADOUT_MANAGER_QUERY_KIND);
    match LoadoutManagerQueryKindHook
        .initialize(query_target, loadout_manager_query_kind)
        .and_then(|hook| hook.enable())
    {
        Ok(_) => info!("SCBL_EXTENDED_LOADOUTS query_hook_enabled=true address={LOADOUT_MANAGER_QUERY_KIND:08X}"),
        Err(err) => error!("SCBL_EXTENDED_LOADOUTS query_hook_enabled=false address={LOADOUT_MANAGER_QUERY_KIND:08X} error={err:?}"),
    }
    info!(
        "SCBL_EXTENDED_LOADOUTS revision=59 save_contract=native-map-and-items-preserved sidecar_version=3 per_loadout_upgrades=true empty_attachment_slots=preserved enabled=true storage=validated-sidecar native_save=single-source visible_layout=official-three-plus-custom-three visible_count=6 retail_entries_mutated=false"
    );
}

pub unsafe fn deinit() {
    let _ = LoadoutManagerQueryKindHook.disable();
    let _ = LoadoutManagerSerializeHook.disable();
    LAST_MANAGER.store(0, Ordering::Release);
    SIDECAR_LOADED_MANAGER.store(0, Ordering::Release);
    SIDECAR_REJECTED_MANAGER.store(0, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn all_multiplayer_slots_unlock_without_changing_gear_or_campaign() {
        let mut entries: Vec<Vec<u32>> = (0..4).map(|kind| {
            let mut e=vec![7;0x80/4];e[0x28/4]=kind;e
        }).collect();
        let before=entries.clone();
        let mut ptrs:Vec<u32>=entries.iter_mut().map(|e|e.as_mut_ptr()as u32).collect();
        let mut manager=vec![0u32;0x30/4];manager[0x20/4]=ptrs.as_mut_ptr()as u32;manager[0x24/4]=4;
        assert_eq!(unsafe{unlock_multiplayer_loadouts(manager.as_mut_ptr().cast(),"unit")},2);
        for kind in 0..4 {
            let mut expected=before[kind].clone();
            if kind==1||kind==2 { expected[0x34/4..0x40/4].fill(0); }
            assert_eq!(entries[kind],expected);
        }
        assert_eq!(unsafe{unlock_multiplayer_loadouts(manager.as_mut_ptr().cast(),"unit-repeat")},0);
        assert_eq!(unsafe{unlock_multiplayer_loadouts(std::ptr::null_mut(),"unit-null")},0);
    }

    #[test]
    fn extended_names_do_not_overlap_retail_custom_names() {
        for name in EXTENDED {
            assert!(name_without_nul(name).starts_with(b"SCBL_EXTENDED_"));
        }
    }

    #[test]
    fn growth_always_covers_requested_count() {
        for count in 1..MAX_REASONABLE_LOADOUTS {
            assert!(next_capacity_for_mode(count, false) >= count);
            assert!(next_capacity_for_mode(count, true) >= count);
        }
    }

    #[test]
    fn visible_layout_replaces_official_rows_without_adding_rows() {
        let retail = [0, 1, 2, 3, 4, 5];
        let shadows = [10, 11, 12];
        assert_eq!(visible_projection(retail, shadows), [10, 11, 12, 3, 4, 5]);
    }

    #[test]
    fn compatible_native_save_preserves_items_and_equipped_map_and_restores_view() {
        let mut manager = [0u8; 0x80];
        let manager = manager.as_mut_ptr();
        let mut live_entries = [0x10usize as *mut u8, 0x20usize as *mut u8, 0x30usize as *mut u8];
        let mut compatible_entries = [0x10usize as *mut u8, 0x30usize as *mut u8];
        let mut live_cache = [2usize, 0, 1];
        let mut equipped_map = [0usize, 7, 20, 21, 22, 23, 25];

        unsafe {
            write_usize(manager, 0x10, equipped_map.as_mut_ptr() as usize);
            write_usize(manager, 0x14, equipped_map.len());
            write_usize(manager, 0x18, equipped_map.len());
            write_usize(manager, LOADOUT_ITEMS_OFFSET, live_cache.as_mut_ptr() as usize);
            write_usize(manager, LOADOUT_ITEMS_COUNT_OFFSET, live_cache.len());
            write_usize(manager, LOADOUT_ITEMS_CAPACITY_OFFSET, live_cache.len() + 4);
            write_usize(manager, LOADOUT_ARRAY_OFFSET, live_entries.as_mut_ptr() as usize);
            write_usize(manager, LOADOUT_COUNT_OFFSET, live_entries.len());
            write_usize(manager, LOADOUT_CAPACITY_OFFSET, live_entries.len() + 5);

            with_compatible_serialization_view(manager, compatible_entries.as_mut_ptr(), compatible_entries.len(), || {
                assert_eq!(read_usize(manager, LOADOUT_ITEMS_OFFSET), live_cache.as_mut_ptr() as usize);
                assert_eq!(read_usize(manager, LOADOUT_ITEMS_COUNT_OFFSET), live_cache.len());
                assert_eq!(read_usize(manager, LOADOUT_ITEMS_CAPACITY_OFFSET), live_cache.len() + 4);
                assert_eq!(read_usize(manager, 0x10), equipped_map.as_mut_ptr() as usize);
                assert_eq!(read_usize(manager, 0x14), 7);
                assert_eq!(read_usize(manager, 0x18), 7);
                assert_eq!(equipped_map, [0, 7, 20, 21, 22, 23, 25]);
                assert_eq!(read_usize(manager, LOADOUT_ARRAY_OFFSET), compatible_entries.as_mut_ptr() as usize);
                assert_eq!(read_usize(manager, LOADOUT_COUNT_OFFSET), compatible_entries.len());
                assert_eq!(read_usize(manager, LOADOUT_CAPACITY_OFFSET), compatible_entries.len());
            });

            assert_eq!(read_usize(manager, LOADOUT_ITEMS_OFFSET), live_cache.as_mut_ptr() as usize);
            assert_eq!(read_usize(manager, LOADOUT_ITEMS_COUNT_OFFSET), live_cache.len());
            assert_eq!(read_usize(manager, LOADOUT_ITEMS_CAPACITY_OFFSET), live_cache.len() + 4);
            assert_eq!(read_usize(manager, LOADOUT_ARRAY_OFFSET), live_entries.as_mut_ptr() as usize);
            assert_eq!(read_usize(manager, LOADOUT_COUNT_OFFSET), live_entries.len());
            assert_eq!(read_usize(manager, LOADOUT_CAPACITY_OFFSET), live_entries.len() + 5);
        }
    }

    #[test]
    fn native_save_item_table_offsets_match_retail_serializer_layout() {
        assert_eq!(LOADOUT_ITEMS_OFFSET, 0x64);
        assert_eq!(LOADOUT_ITEMS_COUNT_OFFSET, 0x68);
        assert_eq!(LOADOUT_ITEMS_CAPACITY_OFFSET, 0x6C);
        assert!(LOADOUT_ITEMS_OFFSET > LOADOUT_CAPACITY_OFFSET);
    }


    #[test]
    fn appending_six_shadows_preserves_every_native_manager_field_except_loadouts() {
        let mut manager = [0usize; 0x80/4];
        let m = manager.as_mut_ptr().cast::<u8>();
        let mut entries = [std::ptr::null_mut::<u8>(); 32];
        let mut selected = [0usize, 7, 20, 21, 22, 23, 25];
        unsafe {
            write_usize(m, 0x10, selected.as_mut_ptr() as usize);
            write_usize(m, 0x14, 7); write_usize(m, 0x18, 7);
            write_usize(m, 0x20, entries.as_mut_ptr() as usize);
            write_usize(m, 0x24, 20); write_usize(m, 0x28, 32);
            write_usize(m, 0x64, 0x1234); write_usize(m, 0x68, 10); write_usize(m, 0x6c, 16);
            let before = manager;
            for i in 0..6 { assert!(append_entry(m, (0x1000+i*0x60) as *mut u8)); }
            let mut expected = before; expected[0x24/4] = 26;
            assert_eq!(manager, expected);
            assert_eq!(selected, [0, 7, 20, 21, 22, 23, 25]);
            assert!(selected.iter().all(|&i| i < read_usize(m, 0x24)));
        }
    }
    fn sample_sidecar() -> Sidecar {
        Sidecar {
            records: (0..SIDECAR_RECORD_COUNT)
                .map(|index| SidecarRecord {
                    kind: EDITABLE_LOADOUT_KINDS[index / 3] as u32,
                    slot: (index % 3) as u8,
                    display_name: vec![b'A' as u16 + index as u16],
                    gear_ids: vec![format!("gear_{index}_a").into_bytes(), format!("gear_{index}_b").into_bytes()],
                    upgrades: Some(Vec::new()),
                })
                .collect(),
        }
    }

    #[test]
    fn sidecar_round_trip_preserves_six_bounded_records() {
        let expected = sample_sidecar();
        let encoded = encode_sidecar(&expected).unwrap();
        assert_eq!(decode_sidecar(&encoded).unwrap(), expected);
    }

    #[test]
    fn sidecar_rejects_modified_payload() {
        let mut encoded = encode_sidecar(&sample_sidecar()).unwrap();
        encoded[16] ^= 0x40;
        assert_eq!(decode_sidecar(&encoded), Err("sidecar checksum mismatch"));
    }

    #[test]
    fn sidecar_rejects_trailing_data_even_with_valid_checksum() {
        let encoded = encode_sidecar(&sample_sidecar()).unwrap();
        let payload_len = encoded.len() - 32;
        let mut payload = encoded[..payload_len].to_vec();
        payload.push(0xAA);
        let digest = Sha256::digest(&payload);
        payload.extend_from_slice(&digest);
        assert_eq!(decode_sidecar(&payload), Err("sidecar trailing data"));
    }

    #[test]
    fn sidecar_atomic_write_replaces_existing_bytes() {
        let directory = std::env::temp_dir().join(format!("scbl-loadout-sidecar-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("00000001.scbl-loadouts");
        fs::write(&path, b"old-invalid-data").unwrap();

        let expected = sample_sidecar();
        write_sidecar_atomic(&path, &expected).unwrap();
        let actual = decode_sidecar(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(actual, expected);
        assert!(!directory.join("00000001.scbl-loadouts.tmp").exists());

        fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod empty_slot_migration_tests {
    use super::*;
    fn sample() -> Sidecar {
        Sidecar { records: (0..6).map(|i| SidecarRecord { kind: (i/3+1) as u32, slot: (i%3) as u8,
            display_name: vec![65+i as u16,0],gear_ids:vec![b"OwnedGear".to_vec()],upgrades:None }).collect() }
    }
    #[test]
    fn v3_first_write_preserves_exact_v2_backup_and_rejects_backup_collision() {
        let dir=std::env::temp_dir().join(format!("scbl-empty-migration-{}",std::process::id()));
        fs::create_dir_all(&dir).unwrap();let path=dir.join("owned.scbl-loadouts");
        let desired=sample();let mut old=encode_sidecar(&desired).unwrap();
        old[8..10].copy_from_slice(&2u16.to_le_bytes());
        let n=old.len()-32;let sum=Sha256::digest(&old[..n]);old[n..].copy_from_slice(&sum);
        assert!(decode_sidecar(&old).is_ok());fs::write(&path,&old).unwrap();
        let backup=path.with_extension(format!("scbl-loadouts.pre-v3-{:x}.bak",Sha256::digest(&old)));
        write_sidecar_atomic(&path,&desired).unwrap();
        assert_eq!(fs::read(&backup).expect("v3 migration must retain original bytes"),old);
        assert_eq!(decode_sidecar(&fs::read(&path).unwrap()).unwrap(),desired);
        write_sidecar_atomic(&path,&desired).unwrap();
        assert_eq!(fs::read(&backup).unwrap(),old);
        // If an existing backup is wrong, do not overwrite either it or the old save.
        fs::write(&path,&old).unwrap();fs::write(&backup,b"owned-conflict").unwrap();
        assert!(write_sidecar_atomic(&path,&desired).is_err());
        assert_eq!(fs::read(&path).unwrap(),old);
        assert_eq!(fs::read(&backup).unwrap(),b"owned-conflict");
        fs::remove_file(&path).unwrap();fs::remove_file(&backup).unwrap();fs::remove_dir(&dir).unwrap();
    }
    #[test]
    fn detailed_failure_logging_is_bounded_by_six_slots_and_two_phases() {
        FAILURE_DETAILS.store(0,Ordering::Relaxed);
        assert!(!first_failure_detail(0,0,false));assert!(!first_failure_detail(3,0,false));
        assert!(!first_failure_detail(1,3,false));
        for applying in [false,true] {for kind in 1..=2 {for slot in 0..3 {
            assert!(first_failure_detail(kind,slot,applying));
            assert!(!first_failure_detail(kind,slot,applying));
        }}}
        assert_eq!(FAILURE_DETAILS.load(Ordering::Relaxed),0xfff);
        FAILURE_DETAILS.store(0,Ordering::Relaxed);
    }
}
