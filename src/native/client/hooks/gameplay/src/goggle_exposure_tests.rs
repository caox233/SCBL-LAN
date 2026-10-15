use super::*;

fn frame(now_ms: u64) -> Frame {
    Frame {
        life: Life { scene: 1, spawn: 1 },
        now_ms,
        dx11_mod: true,
        local_alive_spy: true,
        tactical_goggles_on: true,
        native_marked: Some(false),
        detections: ActualDetections::default(),
    }
}
fn detected(now: u64, source: Source) -> Frame {
    let mut f = frame(now);
    f.detections.record(source, true);
    f
}

#[test]
fn no_native_result_never_warns_just_for_goggles_on() {
    let mut w = Warning::default();
    for now in [0, 1000, 5000] {
        assert_eq!(w.update(frame(now)), Presentation::Hidden);
    }
}
#[test]
fn all_three_real_result_sources_can_warn() {
    for source in [Source::Rfd, Source::Ats, Source::Bms] {
        assert_eq!(
            Warning::default().update(detected(0, source)),
            Presentation::Exposed
        );
    }
}
#[test]
fn independent_sources_union_without_clearing_each_other() {
    let mut f = detected(0, Source::Rfd);
    f.detections.record(Source::Bms, true);
    f.detections.record(Source::Rfd, false);
    assert_eq!(Warning::default().update(f), Presentation::Exposed);
    f.detections.record(Source::Bms, false);
    assert_eq!(Warning::default().update(f), Presentation::Hidden);
}
#[test]
fn release_hold_is_two_seconds_after_last_detection_not_first_show() {
    let mut w = Warning::default();
    assert_eq!(w.update(detected(100, Source::Ats)), Presentation::Exposed);
    assert_eq!(w.update(detected(300, Source::Ats)), Presentation::Exposed);
    assert_eq!(w.update(frame(2099)), Presentation::Exposed);
    assert_eq!(w.update(frame(2100)), Presentation::Exposed);
    assert_eq!(w.update(frame(2299)), Presentation::Exposed);
    assert_eq!(w.update(frame(2300)), Presentation::Hidden);
    assert_eq!(w.update(frame(2301)), Presentation::Hidden);
}
#[test]
fn long_detection_still_gets_full_release_hold_when_it_stops() {
    let mut w = Warning::default();
    assert_eq!(w.update(detected(0, Source::Rfd)), Presentation::Exposed);
    assert_eq!(w.update(detected(5000, Source::Rfd)), Presentation::Exposed);
    assert_eq!(w.update(frame(5001)), Presentation::Exposed);
    assert_eq!(w.update(frame(6999)), Presentation::Exposed);
    assert_eq!(w.update(frame(7000)), Presentation::Hidden);
}

#[test]
fn repeated_silent_gaps_do_not_split_the_exposure_episode() {
    let mut w = Warning::default();
    let samples = [
        (0, true),
        (2200, true),
        (2300, false),
        (3000, true),
        (5200, true),
        (5300, false),
        (6000, true),
        (10000, true),
        (11999, false),
    ];
    for (now, seen) in samples {
        let f = if seen {
            detected(now, Source::Ats)
        } else {
            frame(now)
        };
        assert_eq!(w.update(f), Presentation::Exposed, "at {now}");
    }
    assert_eq!(w.update(frame(12000)), Presentation::Hidden);
    assert_eq!(w.update(frame(12500)), Presentation::Hidden);
    assert_eq!(
        w.update(detected(13000, Source::Ats)),
        Presentation::Exposed
    );
    assert_eq!(w.update(frame(14999)), Presentation::Exposed);
    assert_eq!(w.update(frame(15000)), Presentation::Hidden);
}

#[test]
fn another_real_goggle_result_renews_same_hold_but_absent_frames_do_not() {
    let mut w = Warning::default();
    assert_eq!(w.update(detected(0, Source::Ats)), Presentation::Exposed);
    assert_eq!(w.update(frame(1999)), Presentation::Exposed);
    assert_eq!(w.update(detected(2000, Source::Bms)), Presentation::Exposed);
    assert_eq!(w.update(frame(3999)), Presentation::Exposed);
    assert_eq!(w.update(detected(4000, Source::Rfd)), Presentation::Exposed);
    for time in [4001, 4500, 5000, 5500, 5999] {
        assert_eq!(w.update(frame(time)), Presentation::Exposed);
    }
    assert_eq!(w.update(frame(6000)), Presentation::Hidden);
    assert_eq!(w.update(frame(6001)), Presentation::Hidden);
}
#[test]
fn new_episode_gets_new_minimum_after_hide() {
    let mut w = Warning::default();
    w.update(detected(0, Source::Ats));
    assert_eq!(w.update(frame(2000)), Presentation::Hidden);
    assert_eq!(w.update(detected(3000, Source::Bms)), Presentation::Exposed);
    assert_eq!(w.update(frame(4999)), Presentation::Exposed);
    assert_eq!(w.update(frame(5000)), Presentation::Hidden);
}
#[test]
fn real_mark_has_priority_and_cannot_leave_a_stale_yellow_hold() {
    let mut w = Warning::default();
    w.update(detected(0, Source::Rfd));
    let mut real = detected(100, Source::Rfd);
    real.native_marked = Some(true);
    assert_eq!(w.update(real), Presentation::Native);
    assert_eq!(w.update(frame(200)), Presentation::Hidden);
    assert_eq!(w.update(detected(201, Source::Rfd)), Presentation::Exposed);
}
#[test]
fn unreadable_native_warning_is_not_assumed_unmarked() {
    let mut w = Warning::default();
    w.update(detected(0, Source::Bms));
    let mut f = detected(100, Source::Bms);
    f.native_marked = None;
    assert_eq!(w.update(f), Presentation::Native);
    assert_eq!(w.update(frame(101)), Presentation::Hidden);
}
#[test]
fn mode_role_and_goggle_off_each_clear_even_during_minimum() {
    for field in 0..3 {
        let mut w = Warning::default();
        w.update(detected(0, Source::Rfd));
        let mut f = detected(1, Source::Rfd);
        match field {
            0 => f.dx11_mod = false,
            1 => f.local_alive_spy = false,
            _ => f.tactical_goggles_on = false,
        }
        assert_eq!(w.update(f), Presentation::Hidden);
        assert_eq!(w.update(frame(2)), Presentation::Hidden);
    }
}
#[test]
fn native_warning_is_never_replaced_even_outside_feature_scope() {
    let mut f = detected(0, Source::Rfd);
    f.dx11_mod = false;
    f.local_alive_spy = false;
    f.native_marked = Some(true);
    assert_eq!(Warning::default().update(f), Presentation::Native);
}
#[test]
fn scene_or_respawn_change_does_not_inherit_hold() {
    for scene_changed in [true, false] {
        let mut w = Warning::default();
        w.update(detected(100, Source::Ats));
        let mut f = frame(101);
        if scene_changed {
            f.life.scene += 1;
        } else {
            f.life.spawn += 1;
        }
        assert_eq!(w.update(f), Presentation::Hidden);
    }
}
#[test]
fn unknown_life_rejects_even_a_detection() {
    for scene_unknown in [true, false] {
        let mut f = detected(0, Source::Bms);
        if scene_unknown {
            f.life.scene = 0;
        } else {
            f.life.spawn = 0;
        }
        assert_eq!(Warning::default().update(f), Presentation::Hidden);
    }
}
#[test]
fn reversed_time_drops_hold_and_integer_limit_does_not_overflow() {
    let mut w = Warning::default();
    w.update(detected(100, Source::Rfd));
    assert_eq!(w.update(frame(99)), Presentation::Hidden);
    w.update(detected(u64::MAX - 2500, Source::Rfd));
    assert_eq!(w.update(frame(u64::MAX - 501)), Presentation::Exposed);
    assert_eq!(w.update(frame(u64::MAX - 500)), Presentation::Hidden);
}
#[test]
fn native_abi_text_is_terminated_utf16_in_both_supported_languages() {
    for (lang, expected) in [
        (Language::English, "EXPOSED"),
        (Language::TraditionalChinese, "已暴露"),
    ] {
        let t = text(lang);
        assert_eq!(t.last(), Some(&0));
        assert_eq!(String::from_utf16(&t[..t.len() - 1]).unwrap(), expected);
        assert!(!t[..t.len() - 1].contains(&0));
    }
}
#[test]
fn same_input_warning_contract() {
    let mut w = Warning::default();
    let mut tagged = detected(1101, Source::Ats);
    tagged.native_marked = Some(true);
    let inputs = [
        frame(0),
        detected(100, Source::Rfd),
        frame(1099),
        frame(1100),
        tagged,
    ];
    let observed: Vec<_> = inputs.into_iter().map(|f| w.update(f)).collect();
    assert_eq!(
        observed,
        [
            Presentation::Hidden,
            Presentation::Exposed,
            Presentation::Exposed,
            Presentation::Exposed,
            Presentation::Native
        ]
    );
    println!("C29_POLICY actual_results=none,rfd,none,none,ats+native_mark output={observed:?} native_state_writes=0");
}
