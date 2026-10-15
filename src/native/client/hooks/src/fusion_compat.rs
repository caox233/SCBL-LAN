use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tracing::info;
use tracing::warn;
use windows::core::s;
use windows::core::PCSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::GetModuleHandleExA;

const FOV_FACTOR_KEY: &[u8] = b"FOVFactor\0";
const DEFAULT_FOV_FACTOR: f32 = 1.2;
const COMPATIBILITY_WINDOW_TICKS: usize = 300;
const COMPATIBILITY_TICK: Duration = Duration::from_millis(50);

static STOP_COMPATIBILITY_THREAD: AtomicBool = AtomicBool::new(false);

// Keep the loaded image alive for this short compatibility monitor. Do not
// load a missing ASI or permanently pin it. One acquired reference is balanced
// on normal completion and on the stop path when the worker locals are dropped.
struct ModuleReference(HMODULE);

impl ModuleReference {
    unsafe fn loaded(name: PCSTR) -> Option<Self> {
        let mut module = HMODULE::default();
        GetModuleHandleExA(0, name, &mut module).ok()?;
        Some(Self(module))
    }
}

impl Drop for ModuleReference {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.0);
        }
    }
}

struct FusionModule {
    name: &'static str,
    module_name: PCSTR,
    module: Option<ModuleReference>,
    base: usize,
    fov_factor: Option<usize>,
    announced: bool,
    changed: bool,
}

impl FusionModule {
    const fn new(name: &'static str, module_name: PCSTR) -> Self {
        Self {
            name,
            module_name,
            module: None,
            base: 0,
            fov_factor: None,
            announced: false,
            changed: false,
        }
    }

    unsafe fn refresh(&mut self, target_factor: f32) {
        if self.module.is_none() {
            self.module = ModuleReference::loaded(self.module_name);
        }
        let Some(module) = self.module.as_ref() else {
            return;
        };

        let base = module.0.0 as usize;
        if base == 0 {
            return;
        }

        if self.base != base {
            self.base = base;
            self.fov_factor = module_image(module).and_then(|image| find_fov_factor_storage(image, base)).map(|offset| base + offset);
        }

        let Some(fov_factor) = self.fov_factor else {
            if !self.announced {
                warn!(
                    "{}",
                    format!(
                        "SCBL_FUSION_COMPAT module={} base={base:08X} action=apply-fov applied=false reason=fov-factor-signature-not-found",
                        self.name
                    ),
                );
                self.announced = true;
            }
            return;
        };

        if !self.announced {
            info!(
                "{}",
                format!(
                    "SCBL_FUSION_COMPAT module={} base={base:08X} fov_factor={fov_factor:08X} action=monitor-fov value={target_factor:.2}",
                    self.name,
                ),
            );
            self.announced = true;
        }

        let current = std::ptr::read_volatile(fov_factor as *const u32);
        let target_bits = target_factor.to_bits();
        if current != target_bits {
            std::ptr::write_volatile(fov_factor as *mut u32, target_bits);
            let applied = std::ptr::read_volatile(fov_factor as *const u32) == target_bits;
            info!(
                "{}",
                format!(
                    "SCBL_FUSION_COMPAT module={} action=apply-fov previous={} target={target_factor:.2} applied={applied}",
                    self.name,
                    f32::from_bits(current)
                ),
            );
            self.changed |= applied;
        }
    }
}

unsafe fn module_image(module: &ModuleReference) -> Option<&[u8]> {
    let base = module.0.0 as usize;
    if base == 0 || std::ptr::read_unaligned(base as *const u16) != 0x5A4D {
        return None;
    }

    let pe_offset = usize::try_from(std::ptr::read_unaligned((base + 0x3C) as *const u32)).ok()?;
    if pe_offset > 0x1000 || std::ptr::read_unaligned((base + pe_offset) as *const u32) != 0x0000_4550 {
        return None;
    }

    // SizeOfImage is 0x38 bytes into IMAGE_OPTIONAL_HEADER32, which begins
    // 0x18 bytes after the PE signature.
    let image_size = usize::try_from(std::ptr::read_unaligned((base + pe_offset + 0x50) as *const u32)).ok()?;
    if !(0x1000..=0x1000_0000).contains(&image_size) {
        return None;
    }

    Some(std::slice::from_raw_parts(base as *const u8, image_size))
}

fn find_bytes(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() || start > haystack.len().saturating_sub(needle.len()) {
        return None;
    }

    haystack[start..].windows(needle.len()).position(|window| window == needle).map(|offset| start + offset)
}

fn find_fov_factor_storage(image: &[u8], base: usize) -> Option<usize> {
    let key_offset = find_bytes(image, FOV_FACTOR_KEY, 0)?;
    let key_address = u32::try_from(base.checked_add(key_offset)?).ok()?.to_le_bytes();

    // FusionFix's configuration loader materializes a string_view as
    // `mov dword ptr [eax], <FOVFactor address>`. Starting at that unique
    // reference, the parsed and clamped float is stored to its module-global
    // field with `movss dword ptr [absolute], xmm0`. Deriving both addresses
    // from the key keeps this compatible with relocated and rebuilt ASIs.
    let mut reference_pattern = [0_u8; 6];
    reference_pattern[..2].copy_from_slice(&[0xC7, 0x00]);
    reference_pattern[2..].copy_from_slice(&key_address);
    let reference = find_bytes(image, &reference_pattern, 0)?;

    let search_start = reference + reference_pattern.len();
    let search_end = image.len().min(search_start + 0x100);
    let store_prefix = [0xF3, 0x0F, 0x11, 0x05];
    let store = find_bytes(&image[..search_end], &store_prefix, search_start)?;
    let target_bytes: [u8; 4] = image.get(store + store_prefix.len()..store + store_prefix.len() + 4)?.try_into().ok()?;
    let target = usize::try_from(u32::from_le_bytes(target_bytes)).ok()?;
    let offset = target.checked_sub(base)?;
    (offset.checked_add(4)? <= image.len()).then_some(offset)
}

pub fn init(configured_factor: f32) {
    let target_factor = normalized_fov_factor(configured_factor);
    STOP_COMPATIBILITY_THREAD.store(false, Ordering::Release);
    let spawn_result = std::thread::Builder::new().name(String::from("fusion-compat")).spawn(move || unsafe {
        let mut fusion_fix = FusionModule::new("FusionFix", s!("SplinterCellBlacklist.FusionFix.asi"));
        let mut fusion_mod = FusionModule::new("FusionMod", s!("SplinterCellBlacklist.FusionMod.asi"));
        let mut duplicate_announced = false;

        for _tick in 0..COMPATIBILITY_WINDOW_TICKS {
            if STOP_COMPATIBILITY_THREAD.load(Ordering::Acquire) {
                return;
            }

            fusion_fix.refresh(target_factor);
            fusion_mod.refresh(target_factor);
            if !duplicate_announced && fusion_fix.base != 0 && fusion_mod.base != 0 {
                warn!(
                    "{}",
                    format!(
                        "SCBL_FUSION_COMPAT duplicate_modules=true fusion_fix={:08X} fusion_mod={:08X} action=preserve-both-and-apply-fov",
                        fusion_fix.base, fusion_mod.base
                    ),
                );
                duplicate_announced = true;
            }
            std::thread::sleep(COMPATIBILITY_TICK);
        }

        info!(
            "{}",
            format!(
                "SCBL_FUSION_COMPAT monitoring-complete target={target_factor:.2} fusion_fix_detected={} fusion_fix_changed={} fusion_mod_detected={} fusion_mod_changed={}",
                fusion_fix.base != 0,
                fusion_fix.changed,
                fusion_mod.base != 0,
                fusion_mod.changed
            ),
        );
    });
    if let Err(error) = spawn_result {
        warn!("SCBL_FUSION_COMPAT monitoring-start-failed error={error}");
    }
}

fn normalized_fov_factor(configured_factor: f32) -> f32 {
    if configured_factor.is_finite() && (1.0..=1.3).contains(&configured_factor) {
        configured_factor
    } else {
        DEFAULT_FOV_FACTOR
    }
}

pub fn deinit() {
    STOP_COMPATIBILITY_THREAD.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_module_is_not_loaded_or_given_an_owned_reference() {
        let module = unsafe { ModuleReference::loaded(s!("scbl_missing_fov_monitor_8c27a631.asi")) };
        assert!(module.is_none());
    }

    #[test]
    fn derives_factor_storage_from_the_configuration_key() {
        let base = 0x1000_0000;
        let mut image = vec![0_u8; 0x400];
        let key_offset = 0x200;
        let factor_offset = 0x300;
        image[key_offset..key_offset + FOV_FACTOR_KEY.len()].copy_from_slice(FOV_FACTOR_KEY);

        image[0x40..0x42].copy_from_slice(&[0xC7, 0x00]);
        image[0x42..0x46].copy_from_slice(&u32::try_from(base + key_offset).unwrap().to_le_bytes());
        image[0x80..0x84].copy_from_slice(&[0xF3, 0x0F, 0x11, 0x05]);
        image[0x84..0x88].copy_from_slice(&u32::try_from(base + factor_offset).unwrap().to_le_bytes());

        assert_eq!(find_fov_factor_storage(&image, base), Some(factor_offset));
    }

    #[test]
    fn rejects_a_factor_storage_outside_the_module() {
        let base = 0x1000_0000;
        let mut image = vec![0_u8; 0x400];
        let key_offset = 0x200;
        image[key_offset..key_offset + FOV_FACTOR_KEY.len()].copy_from_slice(FOV_FACTOR_KEY);
        image[0x40..0x42].copy_from_slice(&[0xC7, 0x00]);
        image[0x42..0x46].copy_from_slice(&u32::try_from(base + key_offset).unwrap().to_le_bytes());
        image[0x80..0x84].copy_from_slice(&[0xF3, 0x0F, 0x11, 0x05]);
        image[0x84..0x88].copy_from_slice(&0x2000_0000_u32.to_le_bytes());

        assert_eq!(find_fov_factor_storage(&image, base), None);
    }

    #[test]
    fn normalizes_fov_to_the_supported_range() {
        assert_eq!(normalized_fov_factor(1.0), 1.0);
        assert_eq!(normalized_fov_factor(1.2), 1.2);
        assert_eq!(normalized_fov_factor(1.3), 1.3);
        assert_eq!(normalized_fov_factor(0.9), 1.2);
        assert_eq!(normalized_fov_factor(f32::INFINITY), 1.2);
    }
}
