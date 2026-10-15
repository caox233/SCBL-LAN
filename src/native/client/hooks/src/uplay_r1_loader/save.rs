use std::fs;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::os::windows::prelude::FromRawHandle;
use std::os::windows::prelude::IntoRawHandle;
use std::os::windows::prelude::RawHandle;
use std::path::Path;
use std::path::PathBuf;
use std::slice;

use hooks_proc::forwardable_export;
use tracing::error;
use tracing::info;

use super::List;
use super::Result;
use super::UplayList;
use super::UplayOverlapped;
use super::UplaySave;
use crate::config::get;
use crate::config::GameplayMode;
use crate::config::SaveGameExt as _;

const EXTENDED_SLOT_BASE: usize = 10_000_000;

#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[repr(usize)]
enum OpenMode {
    Read = 0,
    Write = 1,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum SaveRoute {
    Official,
    Mod,
}

#[derive(Debug, PartialEq, Eq)]
struct SaveHandle {
    slot_id: usize,
    path: PathBuf,
    raw_handle: Option<RawHandle>,
    failed: bool,
}

impl SaveHandle {
    fn into_mut_ptr(self) -> *mut SaveHandle {
        Box::into_raw(Box::new(self))
    }

    #[allow(clippy::unnecessary_box_returns)]
    fn from_mut_ptr(ptr: *mut SaveHandle) -> Box<SaveHandle> {
        if ptr.is_null() || !ptr.is_aligned() {
            error!("{ptr:?} is invalid");
            panic!("{ptr:?} is invalid");
        }
        unsafe { Box::from_raw(ptr) }
    }
}

fn extended_loadouts_enabled() -> bool {
    get().is_some_and(|config| config.gameplay_mode == GameplayMode::Mod)
}

fn route_for_gameplay_mode(gameplay_mode: GameplayMode) -> SaveRoute {
    if gameplay_mode == GameplayMode::Mod {
        SaveRoute::Mod
    } else {
        SaveRoute::Official
    }
}

fn active_route() -> Option<SaveRoute> {
    get().map(|config| route_for_gameplay_mode(config.gameplay_mode))
}

fn official_path(slot_id: usize) -> Option<PathBuf> {
    get().map(|config| config.save.get_savegame_path(slot_id))
}

fn mod_path_from_official(official: &Path) -> Option<PathBuf> {
    let parent = official.parent()?;
    let file_name = official.file_name()?;
    Some(parent.join("MOD").join(file_name))
}

fn save_path_for_route(slot_id: usize, route: SaveRoute) -> Option<PathBuf> {
    let official = official_path(slot_id)?;
    match route {
        SaveRoute::Official => Some(official),
        SaveRoute::Mod => mod_path_from_official(&official),
    }
}

pub(crate) fn extended_loadout_sidecar_path() -> Option<PathBuf> {
    let mut path = save_path_for_route(1, active_route()?)?;
    path.set_extension("scbl-loadouts");
    Some(path)
}

fn route_for(slot_id: usize, mode: OpenMode) -> Option<SaveRoute> {
    let _ = (slot_id, mode);
    active_route()
}

fn path_for(slot_id: usize, mode: OpenMode, route: SaveRoute) -> Option<PathBuf> {
    let _ = mode;
    save_path_for_route(slot_id, route)
}

fn listed_read_source(slot_id: usize, path: PathBuf, route: SaveRoute) -> (PathBuf, SaveRoute) {
    let _ = slot_id;
    (path, route)
}

fn path_with_meta_extension(path: &Path) -> PathBuf {
    let mut result = path.to_path_buf();
    result.set_extension("meta");
    result
}

fn path_for_name(handle: &SaveHandle) -> Option<PathBuf> {
    Some(path_with_meta_extension(&handle.path))
}

// Null/alignment guards do not make arbitrary foreign addresses safe: callers
// still own valid buffers and handles. Complete rejected asynchronous requests.
fn save_pointer_present<T>(pointer: *const T) -> bool {
    !pointer.is_null() && pointer.is_aligned()
}

unsafe fn reject_save_arguments(overlapped: *mut UplayOverlapped) -> bool {
    if save_pointer_present(overlapped) {
        (*overlapped).set_result(1);
    }
    false
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_SAVE_Close(save_handle: *mut SaveHandle) -> bool {
    if !save_pointer_present(save_handle) { return false; }
    let mut handle = SaveHandle::from_mut_ptr(save_handle);
    if let Some(raw_handle) = handle.raw_handle.take() {
        let file = File::from_raw_handle(raw_handle);
        drop(file);
    }
    !handle.failed
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_SAVE_GetSavegames(out_games_list: *mut *mut List, overlapped: *mut UplayOverlapped) -> bool {
    if out_games_list.is_null() {
        error!("Invalid out_games_list");
        if !overlapped.is_null() {
            (*overlapped).set_result(1);
        }
        return false;
    }
    let saves = match get_savegames() {
        Ok(saves) => saves,
        Err(err) => {
            error!("Couldn't get savegames: {err}");
            if !overlapped.is_null() {
                (*overlapped).set_result(6);
            }
            return false;
        }
    };
    *out_games_list = Box::into_raw(Box::new(UplayList::Saves(saves).into()));
    if !overlapped.is_null() {
        (*overlapped).set_success();
    }
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_SAVE_Open(slot_id: usize, mode: OpenMode, out_save_handle: *mut *mut SaveHandle, overlapped: *mut UplayOverlapped) -> bool {
    if !save_pointer_present(out_save_handle) {
        error!("Invalid save handle output");
        return reject_save_arguments(overlapped);
    }
    if extended_loadouts_enabled() && (1..=2).contains(&slot_id) {
        if mode == OpenMode::Read {
            crate::plugin_loader::notify(hooks_plugin_api::EVENT_NATIVE_SAVE_OPENED);
        }
    }
    let Some(route) = route_for(slot_id, mode) else {
        error!("Save route is unavailable because gameplay mode is not configured");
        if !overlapped.is_null() {
            (*overlapped).set_result(6);
        }
        return false;
    };
    let Some(path) = path_for(slot_id, mode, route) else {
        error!("Save config is unavailable");
        return false;
    };
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            error!("Couldn't create save game dir {parent:?}: {err}");
            if !overlapped.is_null() {
                (*overlapped).set_result(6);
            }
            return false;
        }
    }
    let file = match mode {
        OpenMode::Read => File::options().read(true).open(&path),
        OpenMode::Write => File::options().write(true).create(true).truncate(true).open(&path),
    };
    let file = match file {
        Ok(file) => file,
        Err(err) => {
            error!("Couldn't open slot {slot_id} route={route:?} for {mode:?}: {err}");
            if !overlapped.is_null() {
                (*overlapped).set_result(6);
            }
            return false;
        }
    };
    info!("SCBL_SAVE_COMPAT phase=open slot={slot_id} mode={mode:?} route={route:?} path={path:?}");
    *out_save_handle = SaveHandle {
        slot_id,
        path,
        raw_handle: Some(file.into_raw_handle()),
        failed: false,
    }
    .into_mut_ptr();
    if !overlapped.is_null() {
        (*overlapped).set_success();
    }
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_SAVE_Read(
    save_handle: *mut SaveHandle,
    num_of_bytes_to_read: usize,
    offset: usize,
    out_buffer: *mut *mut u8,
    out_num_of_bytes_read: *mut usize,
    overlapped: *mut UplayOverlapped,
) -> bool {
    if !save_pointer_present(save_handle) || !save_pointer_present(out_buffer)
        || !save_pointer_present(out_num_of_bytes_read) || num_of_bytes_to_read > isize::MAX as usize
        || !save_pointer_present(*out_buffer) {
        return reject_save_arguments(overlapped);
    }
    let save_handle = save_handle.as_mut().unwrap();
    let result = if let Some(raw_handle) = save_handle.raw_handle.take() {
        let mut file = File::from_raw_handle(raw_handle);
        let result = (|| -> std::io::Result<()> {
            file.seek(std::io::SeekFrom::Start(offset as u64))?;
            let mut buffer = vec![0; num_of_bytes_to_read];
            let read = file.read(&mut buffer)?;
            std::ptr::copy(buffer.as_ptr(), *out_buffer, read);
            *out_num_of_bytes_read = read;
            Ok(())
        })();
        save_handle.raw_handle.replace(file.into_raw_handle());
        result
    } else {
        Ok(())
    };
    let success = result.is_ok();
    if !overlapped.is_null() {
        if let Err(err) = result {
            error!("Couldn't read from file: {err}");
            (*overlapped).set_result(6);
        } else {
            (*overlapped).set_success();
        }
    }
    success
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_SAVE_Remove(slot_id: usize, overlapped: *mut UplayOverlapped) -> bool {
    let Some(route) = active_route() else {
        if !overlapped.is_null() {
            (*overlapped).set_result(6);
        }
        return false;
    };
    let mut paths = save_path_for_route(slot_id, route).into_iter().collect::<Vec<_>>();
    if slot_id == 1 {
        paths.extend(extended_loadout_sidecar_path());
    }
    for path in paths {
        if path.exists() {
            if let Err(err) = fs::remove_file(&path) {
                error!("Couldn't remove {path:?}: {err}");
                if !overlapped.is_null() {
                    (*overlapped).set_result(6);
                }
                return false;
            }
        }
    }
    if !overlapped.is_null() {
        (*overlapped).set_success();
    }
    true
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_SAVE_SetName(save_handle: *mut SaveHandle, name: *const i8) -> bool {
    if !save_pointer_present(save_handle) || name.is_null() { return false; }
    let handle = save_handle.as_mut().unwrap();
    let Some(path) = path_for_name(handle) else {
        return false;
    };
    let name = std::ffi::CStr::from_ptr(name);
    if let Err(err) = fs::write(&path, name.to_bytes()) {
        error!("Couldn't set save name for slot {} path={path:?}: {err}", handle.slot_id);
        false
    } else {
        true
    }
}

#[forwardable_export]
unsafe extern "cdecl" fn UPLAY_SAVE_Write(save_handle: *mut SaveHandle, num_of_bytes_to_write: usize, buffer: *const *const u8, overlapped: *mut UplayOverlapped) -> bool {
    if !save_pointer_present(save_handle) || !save_pointer_present(buffer)
        || num_of_bytes_to_write > isize::MAX as usize || !save_pointer_present(*buffer) {
        return reject_save_arguments(overlapped);
    }
    let save_handle = save_handle.as_mut().unwrap();
    let result = if let Some(raw_handle) = save_handle.raw_handle.take() {
        let mut file = File::from_raw_handle(raw_handle);
        let result = (|| -> std::io::Result<()> {
            file.write_all(slice::from_raw_parts(*buffer, num_of_bytes_to_write))?;
            Ok(())
        })();
        save_handle.raw_handle.replace(file.into_raw_handle());
        result
    } else {
        Ok(())
    };
    let success = result.is_ok();
    if !success {
        save_handle.failed = true;
    }
    if !overlapped.is_null() {
        if let Err(err) = result {
            error!("Couldn't write to file: {err}");
            (*overlapped).set_result(6);
        } else {
            (*overlapped).set_success();
        }
    }
    success
}

// The native ABI uses pointer-width sizes; never advertise a wrapped length.
fn native_save_size(file_size: u64) -> Option<usize> {
    usize::try_from(file_size).ok()
}

fn get_savegames() -> Result<Vec<UplaySave>> {
    let cfg = get().ok_or(anyhow::anyhow!("config not loaded"))?;
    let route = route_for_gameplay_mode(cfg.gameplay_mode);
    let official_directory = cfg.save.get_savegames_path();
    let path = match route {
        SaveRoute::Official => official_directory,
        SaveRoute::Mod => official_directory.join("MOD"),
    };
    if !path.exists() {
        return Ok(vec![]);
    }
    let saves = fs::read_dir(path)?
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == std::ffi::OsStr::new("sav")))
        .filter_map(|entry| {
            entry
                .path()
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .and_then(|stem| stem.parse::<usize>().ok())
                .filter(|slot_id| *slot_id < EXTENDED_SLOT_BASE)
                .map(|slot_id| (slot_id, entry.path()))
        })
        .filter_map(|(slot_id, path)| {
            let (path, route) = listed_read_source(slot_id, path, route);
            let file_size = path.metadata().ok()?.len();
            let Some(size) = native_save_size(file_size) else {
                error!("SCBL_SAVE_COMPAT phase=list slot={slot_id} route={route:?} bytes={file_size} reason=size-exceeds-native-width");
                return None;
            };
            info!("SCBL_SAVE_COMPAT phase=list slot={slot_id} route={route:?} size={size}");
            let meta = path_with_meta_extension(&path);
            let mut name = fs::read_to_string(meta).unwrap_or_default();
            if let Some(index) = name.find('\0') {
                name.truncate(index);
            }
            Some(UplaySave { slot_id, name, size })
        })
        .collect();
    Ok(saves)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::listed_read_source;
    use super::mod_path_from_official;
    use super::route_for_gameplay_mode;
    use super::SaveRoute;
    use crate::config::GameplayMode;

    #[test]
    fn native_save_size_preserves_representable_lengths() {
        for size in [0, 64, u64::from(u32::MAX)] {
            assert_eq!(super::native_save_size(size).map(|value| value as u64), Some(size));
        }
    }

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn native_save_size_rejects_wrapped_lengths() {
        for size in [u64::from(u32::MAX) + 1, u64::from(u32::MAX) + 13, u64::MAX] {
            assert_eq!(super::native_save_size(size), None);
        }
    }

    #[test]
    fn gameplay_mode_selects_a_single_isolated_save_namespace() {
        assert_eq!(route_for_gameplay_mode(GameplayMode::Official), SaveRoute::Official);
        assert_eq!(route_for_gameplay_mode(GameplayMode::Mod), SaveRoute::Mod);
    }

    #[test]
    fn mod_namespace_preserves_slot_file_names_under_a_sibling_directory() {
        let official = PathBuf::from("savegames/00000001.sav");
        assert_eq!(mod_path_from_official(&official), Some(PathBuf::from("savegames/MOD/00000001.sav")));
        let (listed, route) = listed_read_source(1, PathBuf::from("savegames/MOD/00000001.sav"), SaveRoute::Mod);
        assert_eq!(listed, PathBuf::from("savegames/MOD/00000001.sav"));
        assert_eq!(route, SaveRoute::Mod);
    }
}
