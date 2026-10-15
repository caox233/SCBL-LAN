extern crate std;
use crate as hooks_plugin_api;
use std::println;
use hooks_plugin_api::*;
use hooks_plugin_api::hud::*;
use std::mem::{align_of, offset_of, size_of};

#[test]
fn windows_v1_field_offsets_and_widths_are_frozen() {
    assert_eq!(offset_of!(HostApiV1, abi_version), 0);
    assert_eq!(offset_of!(HostApiV1, struct_size), 4);
    assert_eq!(offset_of!(HostApiV1, log), 8);
    let pointer = size_of::<usize>();
    assert_eq!(size_of::<HostApiV1>(), 8 + pointer);
    assert_eq!(align_of::<HostApiV1>(), pointer);
    assert_eq!(offset_of!(PluginConfigV1, abi_version), 0);
    assert_eq!(offset_of!(PluginConfigV1, struct_size), 4);
    assert_eq!(offset_of!(PluginConfigV1, feature_bits), 8);
    assert_eq!(offset_of!(PluginConfigV1, world_raycast), 16);
    assert_eq!(offset_of!(PluginConfigV1, fov_factor_bits), 20);
    assert_eq!(offset_of!(PluginConfigV1, loadout_sidecar_path), 24);
    assert_eq!(offset_of!(PluginConfigV1, loadout_sidecar_path_len), 24 + pointer);
    assert_eq!(offset_of!(PluginConfigV1, reserved2), 28 + pointer);
    assert_eq!(size_of::<PluginConfigV1>(), 40);
    assert_eq!(align_of::<PluginConfigV1>(), 8);
    assert_eq!(size_of::<Option<HostLogFn>>(), pointer);
    assert_eq!(size_of::<Option<InitializeFn>>(), pointer);
    println!("ABI pointer_bytes={pointer} host_bytes={} config_bytes={} hud_bytes={}", size_of::<HostApiV1>(),size_of::<PluginConfigV1>(),size_of::<HudFrame>());
}

#[test]
fn compatibility_requires_version_and_full_v1_prefix_but_allows_extensions() {
    let mut host = HostApiV1 { abi_version: ABI_VERSION, struct_size: 0, log: None };
    let mut config = PluginConfigV1 { abi_version: ABI_VERSION, struct_size: 0, feature_bits: 0, world_raycast: 0, fov_factor_bits: 1.0f32.to_bits(), loadout_sidecar_path: std::ptr::null(), loadout_sidecar_path_len: 0, reserved2: 0 };
    for size in 0..size_of::<HostApiV1>() {host.struct_size=size as u32;assert!(!host_api_compatible(&host));}
    for size in 0..size_of::<PluginConfigV1>() {config.struct_size=size as u32;assert!(!plugin_config_compatible(&config));}
    for extra in [0, 4, 64] {
        host.struct_size=(size_of::<HostApiV1>()+extra) as u32;
        config.struct_size=(size_of::<PluginConfigV1>()+extra) as u32;
        assert!(host_api_compatible(&host));assert!(plugin_config_compatible(&config));
    }
    for version in [0, ABI_VERSION+1, u32::MAX] {
        host.abi_version=version;config.abi_version=version;
        assert!(!host_api_compatible(&host));assert!(!plugin_config_compatible(&config));
    }
}

#[test]
fn public_export_names_and_feature_bits_are_stable() {
    assert_eq!(ABI_VERSION,1);assert_eq!(EVENT_NATIVE_SAVE_OPENED,1);
    assert_eq!([INITIALIZE_EXPORT, SHUTDOWN_EXPORT, HANDLE_EVENT_EXPORT, VALIDATE_EXPORT, RESUME_EXPORT, HUD_EXPORT],
      [b"scbl_plugin_initialize\0".as_slice(),b"scbl_plugin_shutdown\0",b"scbl_plugin_handle_event\0",b"scbl_plugin_validate\0",b"scbl_plugin_resume\0",b"scbl_mod_hud_snapshot_v1\0"]);
    assert_eq!([FEATURE_WEAPON_OBSTRUCTION_FIX,FEATURE_EXTENDED_LOADOUTS,FEATURE_HIDE_EXTENDED_LOADOUTS_IN_MATCH,FEATURE_INTEL_SUIT_DISRUPTOR_IMMUNITY,FEATURE_MERC_SPRINT_VERTICAL_LOOK], [1,2,4,8,16]);
    assert_eq!(GAMEPLAY_FEATURE_BITS_DEFAULT,31);
}

#[test]
fn hud_wire_layout_is_pointer_independent() {
    assert_eq!(HUD_ABI,1);assert_eq!(HUD_MAX_RECTS,8);
    assert_eq!(size_of::<HudRect>(),20);assert_eq!(align_of::<HudRect>(),4);
    assert_eq!(offset_of!(HudRect,rgba),16);
    assert_eq!(offset_of!(HudFrame,rects),16);assert_eq!(size_of::<HudFrame>(),176);
    let frame=HudFrame::default();assert_eq!((frame.abi,frame.size,frame.count,frame.reserved),(1,176,0,0));assert!(frame.valid());
}

#[test]
fn malformed_hud_frames_fail_closed_without_panics() {
    for count in [9,u32::MAX] {assert!(!HudFrame{count,..HudFrame::default()}.valid());}
    for size in [0,175,177,u32::MAX] {assert!(!HudFrame{size,..HudFrame::default()}.valid());}
    assert!(!HudFrame{reserved:1,..HudFrame::default()}.valid());
    for value in [f32::NAN, f32::INFINITY,f32::NEG_INFINITY,-0.01,1.01] {
        for field in 0..4 {
            let mut f=HudFrame{count:1,..HudFrame::default()};
            match field {0=>f.rects[0].x=value,1=>f.rects[0].y=value,2=>f.rects[0].w=value,_=>f.rects[0].h=value}
            assert!(!f.valid());
        }
    }
    // Unused array entries are not part of the active drawing payload.
    let mut f=HudFrame::default();f.rects[7].x=f32::NAN;assert!(f.valid());
}
