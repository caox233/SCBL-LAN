use super::*;
use crate::mod_descriptions::{lookup, ENTRIES};
use std::cell::RefCell;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}
fn native_text(p: usize) -> String {
    unsafe {
        let mut n = 0;
        while (p as *const u16).add(n).read_unaligned() != 0 && n < 1024 {
            n += 1;
        }
        String::from_utf16(std::slice::from_raw_parts(p as *const u16, n)).unwrap()
    }
}
struct Fixture {
    section: Vec<u16>,
    key: Vec<u16>,
    file: Vec<u8>,
    lang: Vec<u8>,
    buffer: Vec<u16>,
}
impl Fixture {
    fn new(key: &str) -> Self {
        Self {
            section: wide(SECTION),
            key: wide(key),
            file: b"Localization\\Loc_Content\0".to_vec(),
            lang: b"int\0".to_vec(),
            buffer: vec![0xeeee; SLOTS * SLOT_BYTES / 2],
        }
    }
    fn args(&self) -> Args {
        Args {
            section: self.section.as_ptr() as usize,
            key: self.key.as_ptr() as usize,
            file: self.file.as_ptr() as usize,
            language: 0,
            optional: 1,
            resolver: 0,
        }
    }
    fn run(
        &mut self,
        enabled: bool,
        normal: bool,
        resolver: usize,
        language: usize,
    ) -> (usize, usize) {
        let mut a = self.args();
        a.resolver = resolver;
        a.language = language;
        let p = self.buffer.as_mut_ptr() as usize;
        let native = wide("NATIVE DESCRIPTION");
        let mut calls = 0;
        let result = unsafe {
            localize_with(a, enabled, self.lang.as_ptr() as usize, normal, p, |b| {
                calls += 1;
                assert_eq!(
                    (b.section, b.key, b.file, b.language, b.optional, b.resolver),
                    (a.section, a.key, a.file, a.language, a.optional, a.resolver)
                );
                std::ptr::copy_nonoverlapping(native.as_ptr(), p as *mut u16, native.len());
                p
            })
        };
        assert_eq!(calls, 1);
        assert_eq!(result, p);
        (p, calls)
    }
}
#[test]
fn catalog_is_exact_unique_bilingual_and_ring_bounded() {
    let mut keys = std::collections::BTreeSet::new();
    for e in ENTRIES {
        assert!(keys.insert(e.key));
        assert!(
            e.key.starts_with("LocID_CDESC_MPArmor") || e.key.starts_with("LocID_CDESC_GI_MP_")
        );
        for text in [e.en, e.cht] {
            assert!(!text.is_empty() && !text.contains('\0') && !text.contains('\u{fffd}'));
            assert!(text.encode_utf16().count() < 1023);
        }
        assert_eq!(lookup(true, SECTION, e.key, FILE, "int"), Some(e.en));
        assert_eq!(lookup(true, SECTION, e.key, FILE, "CHT"), Some(e.cht));
        assert_eq!(lookup(false, SECTION, e.key, FILE, "int"), None);
    }
    assert_eq!(keys.len(), 20);
}
#[test]
fn campaign_other_fields_files_sections_and_languages_are_untouched() {
    let k = ENTRIES[0].key;
    for section in [
        "",
        "Character_Customization_Items",
        "Multiplayer_HUD",
        "Character_Customization_Descriptions2",
    ] {
        assert!(lookup(true, section, k, FILE, "int").is_none());
    }
    for file in [
        "Loc_Content",
        "Localization\\Loc_ADV",
        "Localization\\Loc_Content.int",
        "Localization/Loc_Content",
    ] {
        assert!(lookup(true, SECTION, k, file, "int").is_none());
    }
    for lang in ["", "jpn", "chs", "zh-CN", "none", "english"] {
        assert!(lookup(true, SECTION, k, FILE, lang).is_none());
    }
    for key in [
        "LocID_CITEM_MPArmorGlovesGhost",
        "LocID_CDESC_SPGlovesSuitC",
        "LocID_CDESC_MPArmorPantsGhostElite",
        "LocID_CDESC_MPArmorGlovesGhost_PC",
        "LocID_CDESC_MPArmorGlovesGhost_Short_BAD",
    ] {
        assert!(lookup(true, SECTION, key, FILE, "int").is_none());
    }
}
#[test]
fn ability_tokens_survive_rebound_controls_and_no_hardcoded_ctrl() {
    for e in ENTRIES.iter().filter(|e| e.key.contains("Torso")) {
        for text in [e.en, e.cht] {
            assert_eq!(
                text.matches("{CtrlAct.ButtonsLarge.eActionType_ToggleArmorAbility}")
                    .count(),
                1
            );
            assert!(!text.contains("CTRL") && !text.contains("\\n"));
            assert_eq!(
                text.matches("<FONT ").count(),
                text.matches("</FONT>").count()
            );
        }
    }
}
#[test]
fn balanced_gloves_and_native_drone_destruction_are_not_misdescribed() {
    let text = lookup(true, SECTION, "LocID_CDESC_MPArmorGlovesGhost", FILE, "int").unwrap();
    assert!(text.contains("20%") && !text.contains("30%") && !text.contains("50%"));
    for e in ENTRIES.iter().filter(|e| e.key.contains("TorsoHunter")) {
        assert!(
            e.en.contains("3 SECONDS") && e.en.contains("10 METRES") && e.en.contains("15 SECONDS")
        );
        assert!(
            !e.en.contains("DETONATE") && !e.en.contains("EMP") && !e.en.contains("NO EXPLOSION")
        );
    }
}
#[test]
fn unfinished_exposure_warning_is_not_advertised() {
    for e in ENTRIES {
        for t in [e.en, e.cht] {
            assert!(!t.contains("YELLOW") && !t.contains("已暴露") && !t.contains("黃色"));
        }
    }
}
#[test]
fn original_once_same_ring_no_ownership_or_name_write() {
    let mut f = Fixture::new("LocID_CDESC_MPArmorGlovesGhost");
    let (p, _) = f.run(true, true, 0, 0);
    assert!(native_text(p).contains("20%"));
    assert_eq!(f.buffer[1024], 0xeeee);
    assert_eq!(
        String::from_utf16(&f.key[..f.key.len() - 1]).unwrap(),
        "LocID_CDESC_MPArmorGlovesGhost"
    );
}
#[test]
fn explicit_language_wins_without_language_cache() {
    let mut f = Fixture::new(ENTRIES[0].key);
    let cht = b"cht\0";
    let p = f.run(true, true, 0, cht.as_ptr() as usize).0;
    assert_eq!(native_text(p), ENTRIES[0].cht);
    let p = f.run(true, true, 0, 0).0;
    assert_eq!(native_text(p), ENTRIES[0].en);
    f.lang = b"cht\0".to_vec();
    let p = f.run(true, true, 0, 0).0;
    assert_eq!(native_text(p), ENTRIES[0].cht);
}
#[test]
fn disabled_custom_resolver_and_debug_localization_use_native_once() {
    let mut f = Fixture::new(ENTRIES[0].key);
    for (enabled, normal, resolver) in [(false, true, 0), (true, false, 0), (true, true, 7)] {
        assert_eq!(
            native_text(f.run(enabled, normal, resolver, 0).0),
            "NATIVE DESCRIPTION"
        );
    }
}
#[test]
fn unknown_key_locale_section_and_file_use_native_once() {
    let mut f = Fixture::new("LocID_CDESC_SPGlovesSuitC");
    assert_eq!(native_text(f.run(true, true, 0, 0).0), "NATIVE DESCRIPTION");
    f.key = wide(ENTRIES[0].key);
    f.lang = b"jpn\0".to_vec();
    assert_eq!(native_text(f.run(true, true, 0, 0).0), "NATIVE DESCRIPTION");
    f.lang = b"int\0".to_vec();
    f.section = wide("Character_Customization_Items");
    assert_eq!(native_text(f.run(true, true, 0, 0).0), "NATIVE DESCRIPTION");
    f.section = wide(SECTION);
    f.file = b"Localization\\Loc_ADV\0".to_vec();
    assert_eq!(native_text(f.run(true, true, 0, 0).0), "NATIVE DESCRIPTION");
}
#[test]
fn native_null_empty_and_non_ring_results_remain_native() {
    let mut f = Fixture::new(ENTRIES[0].key);
    let p = f.buffer.as_mut_ptr() as usize;
    for result in [0, 1, p + 4, p + SLOTS * SLOT_BYTES] {
        let r = unsafe {
            localize_with(f.args(), true, f.lang.as_ptr() as usize, true, p, |_| {
                result
            })
        };
        assert_eq!(r, result);
    }
    f.buffer[0] = 0;
    let r = unsafe { localize_with(f.args(), true, f.lang.as_ptr() as usize, true, p, |_| p) };
    assert_eq!(r, p);
    assert_eq!(f.buffer[0], 0);
}
#[test]
fn ring_start_offset_two_and_last_slot_boundaries() {
    let mut b = vec![0xeeeeu16; SLOTS * SLOT_BYTES / 2];
    let p = b.as_mut_ptr() as usize;
    assert_eq!(capacity(p, p), Some(1024));
    assert_eq!(capacity(p + 2, p), Some(1023));
    assert_eq!(capacity(p + SLOT_BYTES * (SLOTS - 1) + 2, p), Some(1023));
    for r in [p - 1, p + 1, p + 4, p + SLOTS * SLOT_BYTES, usize::MAX] {
        assert_eq!(capacity(r, p), None);
    }
    unsafe {
        assert!(rewrite(p + 2, p, "繁體中文😀"));
        assert_eq!(b[0], 0xeeee);
        assert!(!rewrite(p, p, &"a".repeat(1024)));
        assert!(!rewrite(p + 2, p, &"a".repeat(1023)));
        assert!(!rewrite(p, p, "bad\0text"));
    }
}
#[test]
fn bounded_ascii_reader_rejects_bad_unterminated_and_non_ascii_inputs() {
    let mut out = [0u8; 8];
    assert!(unsafe { ascii(1, false, &mut out) }.is_none());
    let value = b"toolongwithoutnull";
    assert!(unsafe { ascii(value.as_ptr() as usize, false, &mut out) }.is_none());
    let value = wide("非ASCII");
    assert!(unsafe { ascii(value.as_ptr() as usize, true, &mut out) }.is_none());
    let value = wide("cht");
    assert_eq!(
        unsafe { ascii(value.as_ptr() as usize, true, &mut out) },
        Some("cht")
    );
}
// Execute the retail six-argument cdecl call block, with native localization
// replaced by the real adapter plus recording native lookup. NOT rendered GFx.
thread_local! { static ABI:RefCell<Option<Fixture>>=const{RefCell::new(None)}; }
unsafe extern "C" fn abi_adapter(
    s: usize,
    k: usize,
    f: usize,
    l: usize,
    o: i32,
    r: usize,
) -> usize {
    ABI.with(|state| {
        let mut state = state.borrow_mut();
        let state = state.as_mut().unwrap();
        let p = state.buffer.as_mut_ptr() as usize;
        assert_eq!(o, 1);
        assert_eq!(r, 0);
        assert_eq!(l, 0);
        assert_eq!(s, state.section.as_ptr() as usize);
        assert_eq!(k, state.key.as_ptr() as usize);
        assert_eq!(f, state.file.as_ptr() as usize);
        let native = wide("NATIVE DESCRIPTION");
        localize_with(
            Args {
                section: s,
                key: k,
                file: f,
                language: l,
                optional: o,
                resolver: r,
            },
            true,
            state.lang.as_ptr() as usize,
            true,
            p,
            |a| {
                assert_eq!(a.optional, 1);
                std::ptr::copy_nonoverlapping(native.as_ptr(), p as *mut u16, native.len());
                p
            },
        )
    })
}
#[test]
fn retail_description_call_block_proves_six_argument_cdecl_utf16_return() {
    use sha2::{Digest, Sha256};
    use windows::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE,
    };
    let root = std::path::PathBuf::from(
        std::env::var("SCBL_R58_C31_FIXTURE").expect("C31 native description fixture"),
    );
    let block = std::fs::read(root.join("native-00cdd47d.bin")).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&block)),
        "f7c41092bf14ac323d324873977043c4433594baeb9c7c918a71ed671c882c35"
    );
    ABI.with(|s| *s.borrow_mut() = Some(Fixture::new(ENTRIES[0].key)));
    unsafe {
        let memory =
            VirtualAlloc(None, 4096, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
        assert_ne!(memory, 0);
        // eax=key; preserve EBX, use native zero/default fields.
        let mut code = vec![0x8b, 0x44, 0x24, 0x04, 0x53, 0x31, 0xdb];
        code.extend_from_slice(&block[..23]);
        code.extend([0x5b, 0xc3]);
        ABI.with(|s| {
            let s = s.borrow();
            let s = s.as_ref().unwrap();
            code[7 + 5..7 + 9].copy_from_slice(&(s.file.as_ptr() as u32).to_le_bytes());
            code[7 + 11..7 + 15].copy_from_slice(&(s.section.as_ptr() as u32).to_le_bytes());
        });
        assert_eq!(code[7 + 15], 0xe8);
        code[7 + 16..7 + 20].copy_from_slice(
            &((abi_adapter as usize as isize - (memory + 7 + 20) as isize) as i32).to_le_bytes(),
        );
        std::ptr::copy_nonoverlapping(code.as_ptr(), memory as *mut u8, code.len());
        let invoke: unsafe extern "C" fn(usize) -> usize = std::mem::transmute(memory);
        for i in 0..1000 {
            let key = ABI.with(|s| {
                let mut s = s.borrow_mut();
                let s = s.as_mut().unwrap();
                s.lang = if i % 2 == 0 {
                    b"int\0".to_vec()
                } else {
                    b"cht\0".to_vec()
                };
                s.key.as_ptr() as usize
            });
            let result = invoke(key);
            assert_eq!(
                native_text(result),
                if i % 2 == 0 {
                    ENTRIES[0].en
                } else {
                    ENTRIES[0].cht
                }
            );
        }
        VirtualFree(memory as *mut _, 0, MEM_RELEASE).unwrap();
    }
    ABI.with(|s| *s.borrow_mut() = None);
    println!("C31_NATIVE_DESCRIPTION callblock=CDD47D cdecl_six_args=1000 same_native_ring=true utf16_en_cht=true; lookup stubbed, actual game rendering pending");
}

#[test]
fn retail_flow_does_not_disable_descriptions_but_id_and_annotated_modes_do() {
    assert!(normal_locale_flags(Some(1), Some(0), Some(0)));
    for flags in [
        (None, Some(0), Some(0)),
        (Some(0), Some(0), Some(0)),
        (Some(1), None, Some(0)),
        (Some(1), Some(0), None),
        (Some(1), Some(1), Some(0)),
        (Some(1), Some(0), Some(1)),
    ] {
        assert!(!normal_locale_flags(flags.0, flags.1, flags.2));
    }
    for locale in [b"int\0", b"cht\0"] {
        let mut f = Fixture::new("LocID_CDESC_MPArmorGlovesGhost");
        f.lang = locale.to_vec();
        let p = f
            .run(true, normal_locale_flags(Some(1), Some(0), Some(0)), 0, 0)
            .0;
        assert!(native_text(p).contains("20%"));
    }
}

#[test]
fn c62_equipment_text_is_concise_and_not_patch_commentary() {
    for e in ENTRIES {
        let en_body = e.en.split('\n').last().unwrap();
        let cht_body = e.cht.split('\n').last().unwrap();
        assert!(en_body.chars().count() <= 180, "{}", e.key);
        assert!(cht_body.chars().count() <= 65, "{}", e.key);
        for phrase in ["NATIVE", "UNCHANGED", "ADDED EFFECT", "ANIMATIONS", "CTRL"] {
            assert!(!e.en.contains(phrase), "{}", e.key);
        }
        for phrase in ["原生", "不變", "新增效果", "攻擊者會收到"] {
            assert!(!e.cht.contains(phrase), "{}", e.key);
        }
    }
}
