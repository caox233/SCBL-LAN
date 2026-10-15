//! C72 DX11 x86, owned by the SCBL LAN main DLL (Official and MOD). Keep original UI/window handling.
//! Hooks are resident for the process lifetime; no window subclass callback is
//! installed, no NOLEGACY flag and no background/global mouse registration.
use super::memory::{patch, readable, signature, word};
use super::raw_mouse::{decode_x86, Accumulator, Identity};
use retour::static_detour;
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

const EVENT: usize = 0x9316d0;
const WINDOW: usize = 0x1af55c0;
const FRAME: usize = 0x1af39f9;
const ORIGINAL: [u8; 7] = [0x83, 0xbe, 0x4c, 0x08, 0, 0, 0];
static READY: AtomicBool = AtomicBool::new(false);
static REGISTERED: AtomicUsize = AtomicUsize::new(0);
static ABORTED: AtomicBool = AtomicBool::new(false);
static THUNK: AtomicUsize = AtomicUsize::new(0);
static LOGGED: AtomicBool = AtomicBool::new(false);
static ABSOLUTE_LOGGED: AtomicBool = AtomicBool::new(false);
static RECOVERY_LOGGED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Accumulator> = Mutex::new(Accumulator::new());
static_detour! {
    static Event:unsafe extern "thiscall" fn(usize,usize,i32,i32,f32,i32,i32)->i32;
    static Window:unsafe extern "thiscall" fn(usize,u32,usize,isize)->isize;
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Device {
    page: u16,
    usage: u16,
    flags: u32,
    window: usize,
}
#[link(name = "user32")]
extern "system" {
    fn RegisterRawInputDevices(dev: *const Device, count: u32, size: u32) -> i32;
    fn GetRegisteredRawInputDevices(dev: *mut Device, count: *mut u32, size: u32) -> u32;
    fn GetRawInputData(
        raw: usize,
        command: u32,
        data: *mut c_void,
        size: *mut u32,
        header: u32,
    ) -> u32;
    fn GetForegroundWindow() -> usize;
    fn IsWindow(window: usize) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(p: *mut c_void, n: usize, t: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(p: *mut c_void, n: usize, protect: u32, old: *mut u32) -> i32;
    fn FlushInstructionCache(p: *mut c_void, address: *const c_void, n: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn GetModuleHandleExW(flags: u32, address: *const u16, module: *mut usize) -> i32;
}
unsafe fn mouse_owner() -> Result<Option<Device>, ()> {
    let mut devices = [Device::default(); 32];
    let mut count = devices.len() as u32;
    let n = GetRegisteredRawInputDevices(
        devices.as_mut_ptr(),
        &mut count,
        std::mem::size_of::<Device>() as u32,
    );
    if n == u32::MAX || n as usize > devices.len() {
        return Err(());
    }
    Ok(devices[..n as usize]
        .iter()
        .copied()
        .find(|d| d.page == 1 && d.usage == 2))
}
fn fail(reason: &str) {
    if !ABORTED.swap(true, Ordering::AcqRel) {
        tracing::warn!(
            "SCBL_CORE_RAW_MOUSE revision=C72R1 active=false fallback=native reason={}",
            reason
        );
    }
    if let Ok(mut s) = STATE.lock() {
        s.fail();
    }
}
unsafe fn register(window: usize) -> bool {
    if ABORTED.load(Ordering::Acquire) || window == 0 || IsWindow(window) == 0 {
        return false;
    }
    let owned = REGISTERED.load(Ordering::Acquire);
    if owned == window {
        if matches!(mouse_owner(),Ok(Some(d)) if d.window==window&&d.flags==0) {
            return true;
        }
        fail("raw-owner-changed");
        return false;
    }
    if owned != 0 {
        fail("window-changed-restart-required");
        return false;
    }
    // Never steal an existing mouse registration, including another plugin's
    // registration targeting the same game window.
    if !matches!(mouse_owner(), Ok(None)) {
        fail("existing-raw-owner");
        return false;
    }
    let d = Device {
        page: 1,
        usage: 2,
        flags: 0,
        window,
    };
    if RegisterRawInputDevices(&d, 1, std::mem::size_of::<Device>() as u32) == 0 {
        fail("register");
        return false;
    }
    REGISTERED.store(window, Ordering::Release);
    tracing::info!(
        "SCBL_CORE_RAW_MOUSE revision=C72R1 registered=true native_menu=true nolegacy=false"
    );
    true
}

pub(super) unsafe fn local_world_pair(pc: usize, pawn: usize) -> bool {
    // Match the local controller and possessed pawn to the same live world.
    // Do not require multiplayer session flags or spy/merc role bits.
    word(pc, 0x144).is_some_and(|w| w >= 0x10000 && word(pawn, 0x144) == Some(w))
}

unsafe fn context(v: usize) -> Option<Identity> {
    if word(v, 0)? != 0x2c4ea4c || word(v, 0x84c)? != 0 {
        return None;
    }
    let window = word(word(v, 0x82c)? as usize, 4)? as usize;
    if window == 0 || GetForegroundWindow() != window {
        return None;
    }
    // Mirror the native legacy capture path 1AF6DB6..1AF6DD8.
    if word(v, 0x510)? != 0 || word(v, 0x5cc)? == 0 || word(v, 0x514)? != 0 {
        return None;
    }
    let client = word(v, 0x18)? as usize;
    if word(client, 0xfc)? != 0 {
        return None;
    }
    if word(v, 0x840)? & 1 == 0 && word(v, 0x850)? == u32::MAX {
        return None;
    }
    let engine = word(client, 0x28)? as usize;
    let query = word(word(engine, 0)? as usize, 0x108)? as usize;
    // Exactly the native viewport's UI/capture query, not an invented menu flag.
    if !(0x400000..0x2900000).contains(&query) || !readable(query, 16) {
        return None;
    }
    let query: unsafe extern "thiscall" fn(usize, usize) -> i32 = std::mem::transmute(query);
    if query(engine, v) == 1 {
        return None;
    }
    let global = if word(0x330c198, 0)? == 0 {
        word(0x330c4fc, 0)?
    } else {
        0
    };
    let global = if global != 0 {
        global
    } else {
        word(0x330c4f8, 0)?
    } as usize;
    if word(word(global, 0)? as usize, 0x134)? != 0x5863d0 {
        return None;
    }
    let pc = word(word(global, 0x194)? as usize, 0x2c8)? as usize;
    let pawn = word(pc, 0x520)? as usize;
    let camera = word(pc, 0x6a4)? as usize;
    let core = word(camera, 0x61c)? as usize;
    if pawn == 0
        || word(pc, 0x310)? as usize != pawn
        || word(pawn, 0x938)? as usize != pc
        || word(pc, 0x60)? & 0x40000 == 0
        || word(pc, 0x4c)? & 0xff00 == 0x0200
        || word(core, 0x20)? as usize != pc
    {
        return None;
    }
    // Mode-agnostic local gameplay scope; retain all preceding native capture,
    // foreground and reciprocal controller/camera ownership checks.
    if !local_world_pair(pc, pawn) {
        return None;
    }
    let mode = word(core, 0x2c)? as usize;
    if mode == 0 {
        return None;
    }
    Some(Identity {
        viewport: v,
        window,
        pawn,
        mode,
    })
}

fn event(
    v: usize,
    input: usize,
    key: i32,
    action: i32,
    delta: f32,
    controller: i32,
    a7: i32,
) -> i32 {
    unsafe {
        if READY.load(Ordering::Acquire)
            && !ABORTED.load(Ordering::Acquire)
            && action == 6
            && (key == 0xe4 || key == 0xe5)
        {
            let hwnd = REGISTERED.load(Ordering::Acquire);
            let suppress = STATE
                .lock()
                .map(|s| s.suppress_legacy(v, hwnd != 0 && GetForegroundWindow() == hwnd))
                .unwrap_or(false);
            if suppress {
                return 1;
            }
        }
        Event.call(v, input, key, action, delta, controller, a7)
    }
}
fn window(v: usize, message: u32, wp: usize, lp: isize) -> isize {
    unsafe {
        if READY.load(Ordering::Acquire) {
            if message == 0x8 || (message == 0x1c && wp == 0) || message == 0x215 || message == 0x2
            {
                if let Ok(mut s) = STATE.lock() {
                    s.suspend();
                }
            }
            if message == 0xff && !ABORTED.load(Ordering::Acquire) {
                let hwnd = REGISTERED.load(Ordering::Acquire);
                let eligible = hwnd != 0
                    && GetForegroundWindow() == hwnd
                    && STATE
                        .lock()
                        .map(|s| s.wants_raw(v, true))
                        .unwrap_or(false);
                if eligible {
                    // One fixed-size read per WM_INPUT; no heap allocation or
                    // calls through the gameplay axis chain for every report.
                    let mut buffer = [0u32; 32];
                    let mut n = std::mem::size_of_val(&buffer) as u32;
                    let got = GetRawInputData(
                        lp as usize,
                        0x10000003,
                        buffer.as_mut_ptr() as _,
                        &mut n,
                        16,
                    );
                    if got == u32::MAX {
                        // A foreign HID/keyboard registration can send larger
                        // packets to this window; it is not our mouse failure.
                        let mut header = [0u32; 4];
                        let mut hn = 16;
                        let h = GetRawInputData(
                            lp as usize,
                            0x10000005,
                            header.as_mut_ptr() as _,
                            &mut hn,
                            16,
                        );
                        if h != 16 || header[0] == 0 {
                            fail("raw-read");
                        }
                    } else if got as usize > std::mem::size_of_val(&buffer) {
                        fail("raw-size");
                    } else if got >= 16 && buffer[0] != 0 { /* Other device: original window handles it. */
                    } else {
                        let bytes =
                            std::slice::from_raw_parts(buffer.as_ptr() as *const u8, got as usize);
                        if let Some((flags, x, y)) = decode_x86(bytes) {
                            let (failed, absolute) = if let Ok(mut s) = STATE.lock() {
                                s.add(flags, x, y);
                                (s.failed, s.recovering && flags & 1 != 0)
                            } else {
                                (true, false)
                            };
                            if failed {
                                fail("relative-overflow-or-state");
                            } else if absolute && !ABSOLUTE_LOGGED.swap(true, Ordering::Relaxed) {
                                tracing::info!("SCBL_CORE_RAW_MOUSE revision=C72R1 active=false fallback=native reason=absolute-input recoverable=true");
                            }
                        } else {
                            fail("raw-layout");
                        }
                    }
                }
            }
        }
        // Original DefWindowProc path cleans WM_INPUT. Do not swallow buttons,
        // wheel, keys, legacy menu motion, window lifetime or focus messages.
        Window.call(v, message, wp, lp)
    }
}

unsafe extern "system" fn frame(v: usize) {
    if !READY.load(Ordering::Acquire) || ABORTED.load(Ordering::Acquire) {
        return;
    }
    // The engine also updates offscreen/secondary viewports. They must not
    // clear the foreground game's accumulated movement.
    let window = word(v, 0x82c)
        .and_then(|p| word(p as usize, 4))
        .unwrap_or(0) as usize;
    if window == 0 {
        return;
    }
    let owner = REGISTERED.load(Ordering::Acquire);
    if owner != 0 && window != owner {
        return;
    }
    let ctx = context(v);
    let eligible = ctx.is_some_and(|c| register(c.window));
    let identity = ctx.unwrap_or_default();
    let result = STATE.lock().ok().and_then(|mut s| {
        let was_recovering = s.recovering;
        let result = s.frame(identity, eligible);
        if was_recovering && !s.recovering && s.active
            && !RECOVERY_LOGGED.swap(true, Ordering::Relaxed) {
            tracing::info!("SCBL_CORE_RAW_MOUSE revision=C72R1 recovered=true boundary=native-frame trigger_packet_replayed=false");
        }
        result
    });
    if let Some((x, y, reports)) = result {
        let Some(input) = word(v, 0x58).map(|p| p as usize).filter(|p| *p != 0) else {
            fail("input-owner");
            return;
        };
        // Original trampoline, never our Event replacement: no double count.
        if x != 0.0 {
            Event.call(v, input, 0xe4, 6, x, 0, 0);
        }
        if y != 0.0 {
            Event.call(v, input, 0xe5, 6, y, 0, 0);
        }
        if reports > 0 && !LOGGED.swap(true, Ordering::AcqRel) {
            tracing::info!("SCBL_CORE_RAW_MOUSE revision=C72R1 applied=true reports={} x={} y={} once_per_native_update=true",reports,x,y);
        }
    }
}

pub(super) fn bridge_bytes(base: u32, callback: u32, resume: u32) -> Vec<u8> {
    // Save flags/GPRs and aligned x87/SSE; callback takes original ESI viewport.
    let mut b = vec![
        0x9c, 0x60, 0x89, 0xe0, 0x81, 0xec, 0x20, 0x02, 0, 0, 0x83, 0xe4, 0xf0, 0x89, 0x84, 0x24,
        0, 0x02, 0, 0, 0x0f, 0xae, 0x04, 0x24, 0xfc, 0x56, 0xb8,
    ];
    b.extend(callback.to_le_bytes());
    b.extend([
        0xff, 0xd0, 0x0f, 0xae, 0x0c, 0x24, 0x8b, 0xa4, 0x24, 0, 0x02, 0, 0, 0x61, 0x9d,
    ]);
    b.extend(ORIGINAL);
    let end = base.wrapping_add(b.len() as u32 + 5);
    b.push(0xe9);
    b.extend(resume.wrapping_sub(end).to_le_bytes());
    b
}
fn jump(to: usize) -> [u8; 7] {
    let mut b = [0xe9, 0, 0, 0, 0, 0x90, 0x90];
    b[1..5].copy_from_slice(&(to as u32).wrapping_sub(FRAME as u32 + 5).to_le_bytes());
    b
}
pub(crate) unsafe fn init(world: usize) {
    if READY.load(Ordering::Acquire) || world != 0x589860 {
        return;
    }
    let contracts = [
        (
            WINDOW,
            211,
            "719875c850c116747e0ea6a662f7769c323845bf83166a831f44f175e19ad4e1",
        ),
        (
            EVENT,
            80,
            "3140f805099807a6ffe665cad9896bef5ee049b44659866568d2e80714484375",
        ),
        (
            FRAME,
            48,
            "f63017a265b2160776c87ea12a9a4d24dbe16448f31b5f317a7610f463783732",
        ),
        (
            0x1af6da9,
            70,
            "0a187024f654d1b039f72f0106f20f26c1c683cf6790526072a8ce2f18444076",
        ),
        (
            0x1af7066,
            317,
            "ad2a16a3a839aadbc71d2e02c7d5f00ae9869054c4a48164842a92a95d4034be",
        ),
        (
            0x1af7670,
            167,
            "46dcc8186defa1bcc30337ee2d0f3a83602debe92c053fbda75376c4e3fb6386",
        ),
    ];
    if !contracts.iter().all(|&(a, n, h)| signature(a, n, h))
        || word(0x2c4eac4, 0) != Some(EVENT as u32)
    {
        fail("native-contract");
        return;
    }
    // A callback or mid-bridge must never reference an unloaded plugin. These
    // hooks require a process restart; preserve code even if teardown fails.
    let mut module = 0;
    if GetModuleHandleExW(5, frame as *const () as *const u16, &mut module) == 0 {
        fail("resident-lifetime");
        return;
    }
    if Event
        .initialize(
            std::mem::transmute::<
                usize,
                unsafe extern "thiscall" fn(usize, usize, i32, i32, f32, i32, i32) -> i32,
            >(EVENT),
            event,
        )
        .is_err()
        || Window
            .initialize(
                std::mem::transmute::<
                    usize,
                    unsafe extern "thiscall" fn(usize, u32, usize, isize) -> isize,
                >(WINDOW),
                window,
            )
            .is_err()
    {
        fail("initialize-hooks");
        return;
    }
    let memory = VirtualAlloc(std::ptr::null_mut(), 128, 0x3000, 4);
    if memory.is_null() {
        fail("bridge-allocation");
        return;
    }
    let code = bridge_bytes(memory as u32, frame as *const () as u32, (FRAME + 7) as u32);
    std::ptr::copy_nonoverlapping(code.as_ptr(), memory as _, code.len());
    let mut old = 0;
    if VirtualProtect(memory, 128, 0x20, &mut old) == 0
        || FlushInstructionCache(GetCurrentProcess(), memory, code.len()) == 0
    {
        fail("bridge-executable");
        return;
    }
    THUNK.store(memory as usize, Ordering::Release);
    if Event.enable().is_err()
        || Window.enable().is_err()
        || !patch(FRAME, &ORIGINAL, &jump(memory as usize))
    {
        let _ = Event.disable();
        let _ = Window.disable();
        fail("enable-hooks");
        return;
    }
    READY.store(true, Ordering::Release);
    tracing::info!("SCBL_CORE_RAW_MOUSE revision=C72R1 initialized=true accumulate=true native_sensitivity=true smoothing=false fps_scaling=false resident_until_exit=true");
}
pub(crate) unsafe fn deinit() {
    READY.store(false, Ordering::Release);
    if let Ok(mut s) = STATE.lock() {
        s.suspend();
    }
    let owned = REGISTERED.load(Ordering::Acquire);
    if owned != 0 {
        let released = match mouse_owner() {
            Ok(Some(d)) if d.window == owned && d.flags == 0 => {
                let d = Device {
                    page: 1,
                    usage: 2,
                    flags: 1,
                    window: 0,
                };
                RegisterRawInputDevices(&d, 1, std::mem::size_of::<Device>() as u32) != 0
            }
            Ok(_) => true, // Absent or another owner: never remove their registration.
            Err(()) => false,
        };
        if released {
            // Failed enumeration/removal retains a retry ticket. Do not clear a
            // newer ticket if ownership changed while the OS call was running.
            let _ = REGISTERED.compare_exchange(owned, 0, Ordering::AcqRel, Ordering::Acquire);
        }
    }
    let thunk = THUNK.load(Ordering::Acquire);
    if thunk != 0 {
        let _ = patch(FRAME, &jump(thunk), &ORIGINAL);
    }
    let _ = Event.disable();
    let _ = Window.disable();
    // Intentionally retain pinned code + executable bridge until process exit.
}
