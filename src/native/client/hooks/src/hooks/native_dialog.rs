use std::ffi::c_void;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use retour::static_detour;
use tracing::error;
use tracing::info;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_LBUTTON;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_SPACE;

use crate::addresses::Addresses;

const MESSAGE_LIST_OFFSET: usize = 0x350;
const MESSAGE_COUNT_OFFSET: usize = 0x354;
const MESSAGE_SIZE: usize = 0x84;
const MESSAGE_KIND_OFFSET: usize = 0x1C;
const MESSAGE_BUTTON_COUNT_OFFSET: usize = 0x64;
const MESSAGE_CALLBACK_CONTEXT_OFFSET: usize = 0x68;
const MESSAGE_TOKEN_OFFSET: usize = 0x7C;
const MESSAGE_REQUIRE_DISPATCH_SUCCESS_OFFSET: usize = 0x80;
const CONTROLLER_TRANSITION_ACTIVE_OFFSET: usize = 0x304;
const CONTROLLER_ACTION_DISPATCH_OFFSET: usize = 0x344;

const RETAIL_DX11_SCENE_UPDATE: usize = 0x019D_1D10;
const RETAIL_DX11_MESSAGE_ADD: usize = 0x019C_5370;
const RETAIL_DX11_MESSAGE_REMOVE: usize = 0x019C_53E0;
const RETAIL_DX11_ACTION_DISPATCH: usize = 0x0191_0220;
const RETAIL_DX11_ONLINE_ROOT: usize = 0x0338_3DF8;
const RETAIL_DX11_LOST_CONNECTION_VTABLE: usize = 0x029A_0364;
const MINIMUM_PRESENTED_SECONDS: f32 = 0.35;
const MAX_SUPPORTED_MESSAGES: u32 = 64;
const MAX_SUPPORTED_BUTTONS: u32 = 8;
const INTERNAL_RESTORE_WINDOW: Duration = Duration::from_secs(15);
const INTERNAL_RESTORE_ACTION_DELAY: f32 = 0.05;

type SceneMessageBoxUpdate = unsafe extern "thiscall" fn(*mut c_void, f32);
type SceneMessageBoxAdd = unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
type SceneMessageBoxRemove = unsafe extern "thiscall" fn(*mut c_void, u32);
type SceneMessageBoxDispatch = unsafe extern "thiscall" fn(*mut c_void, *const DefaultActionPayload) -> bool;

static TRACKER: Mutex<RecoveryTracker> = Mutex::new(RecoveryTracker::new());
static INTERNAL_RESTORE: Mutex<Option<InternalRestoreArm>> = Mutex::new(None);

static_detour! {
    static SceneMessageBoxUpdateHook: unsafe extern "thiscall" fn(*mut c_void, f32);
    static SceneMessageBoxAddHook: unsafe extern "thiscall" fn(*mut c_void, *mut c_void);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DefaultActionPayload {
    selection: u32,
    event_action: i32,
    callback_context: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MessageIdentity {
    controller: usize,
    token: u32,
    kind: u32,
    button_count: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct InputState {
    mouse_down: bool,
    enter_down: bool,
    space_down: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryAction {
    InternalLobbyRestore,
    MouseRelease,
    EnterPress,
    SpacePress,
}

#[derive(Clone, Copy, Debug)]
struct InternalRestoreArm {
    invite_id: i64,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryMode {
    ManualLostConnection,
    InternalLobbyRestore,
}

#[derive(Debug)]
struct RecoveryTracker {
    target: Option<MessageIdentity>,
    presented_seconds: f32,
    previous_input: InputState,
    mouse_press_armed: bool,
    generation: u64,
    mode: RecoveryMode,
}

impl RecoveryTracker {
    const fn new() -> Self {
        Self {
            target: None,
            presented_seconds: 0.0,
            previous_input: InputState {
                mouse_down: false,
                enter_down: false,
                space_down: false,
            },
            mouse_press_armed: false,
            generation: 0,
            mode: RecoveryMode::ManualLostConnection,
        }
    }

    fn capture(&mut self, message: MessageIdentity, input: InputState, mode: RecoveryMode) {
        self.target = Some(message);
        self.presented_seconds = 0.0;
        self.previous_input = input;
        self.mouse_press_armed = false;
        self.mode = mode;
    }

    fn target_for(&self, controller: usize) -> Option<MessageIdentity> {
        self.target.filter(|target| controller != 0 && target.controller == controller)
    }

    fn observe(&mut self, visible: bool, still_present: bool, delta_seconds: f32, input: InputState) -> Option<(u64, RecoveryAction)> {
        if !still_present {
            self.clear();
            self.previous_input = input;
            return None;
        }
        if !visible {
            self.presented_seconds = 0.0;
            self.previous_input = input;
            self.mouse_press_armed = false;
            return None;
        }

        self.presented_seconds += delta_seconds.max(0.0);
        let eligible = self.presented_seconds
            >= if self.mode == RecoveryMode::InternalLobbyRestore {
                INTERNAL_RESTORE_ACTION_DELAY
            } else {
                MINIMUM_PRESENTED_SECONDS
            };
        if eligible && input.mouse_down && !self.previous_input.mouse_down {
            self.mouse_press_armed = true;
        }

        let action = if eligible && self.mode == RecoveryMode::InternalLobbyRestore {
            Some(RecoveryAction::InternalLobbyRestore)
        } else if eligible && !input.mouse_down && self.previous_input.mouse_down && self.mouse_press_armed {
            Some(RecoveryAction::MouseRelease)
        } else if eligible && input.enter_down && !self.previous_input.enter_down {
            Some(RecoveryAction::EnterPress)
        } else if eligible && input.space_down && !self.previous_input.space_down {
            Some(RecoveryAction::SpacePress)
        } else {
            None
        };

        self.previous_input = input;
        if action.is_some() {
            self.mouse_press_armed = false;
            self.generation = self.generation.wrapping_add(1).max(1);
        }
        action.map(|action| (self.generation, action))
    }

    fn clear(&mut self) {
        self.target = None;
        self.presented_seconds = 0.0;
        self.mouse_press_armed = false;
    }
}

pub(crate) fn arm_internal_lobby_restore(invite_id: i64) {
    *INTERNAL_RESTORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(InternalRestoreArm {
        invite_id,
        deadline: Instant::now() + INTERNAL_RESTORE_WINDOW,
    });
    info!(
        "NativeDialogRecovery phase=internal-restore-arm invite_id={invite_id} window_ms={}",
        INTERNAL_RESTORE_WINDOW.as_millis()
    );
}

pub(crate) fn cancel_internal_lobby_restore(invite_id: i64) {
    let mut arm = INTERNAL_RESTORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if arm.is_some_and(|current| current.invite_id == invite_id) {
        *arm = None;
    }
}

fn take_active_internal_restore(now: Instant) -> Option<InternalRestoreArm> {
    let mut arm = INTERNAL_RESTORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if arm.is_some_and(|current| now > current.deadline) {
        *arm = None;
    }
    arm.take()
}

unsafe fn read_u32(base: *const c_void, offset: usize) -> u32 {
    std::ptr::read_unaligned(base.cast::<u8>().add(offset).cast::<u32>())
}

unsafe fn message_at(controller: *mut c_void, index: u32) -> Option<MessageIdentity> {
    if controller.is_null() {
        return None;
    }
    let count = read_u32(controller, MESSAGE_COUNT_OFFSET);
    if count == 0 || count > MAX_SUPPORTED_MESSAGES || index >= count {
        return None;
    }
    let list = std::ptr::read_unaligned(controller.cast::<u8>().add(MESSAGE_LIST_OFFSET).cast::<*mut u8>());
    if list.is_null() {
        return None;
    }
    let message = list.add(index as usize * MESSAGE_SIZE);
    let button_count = std::ptr::read_unaligned(message.add(MESSAGE_BUTTON_COUNT_OFFSET).cast::<u32>());
    // Retail default actions do not occupy the custom-button array. Zero
    // therefore does not mean "no action".
    if button_count > MAX_SUPPORTED_BUTTONS {
        return None;
    }
    Some(MessageIdentity {
        controller: controller as usize,
        token: std::ptr::read_unaligned(message.add(MESSAGE_TOKEN_OFFSET).cast::<u32>()),
        kind: std::ptr::read_unaligned(message.add(MESSAGE_KIND_OFFSET).cast::<u32>()),
        button_count,
    })
}

unsafe fn message_pointer(target: MessageIdentity) -> Option<*mut u8> {
    let controller = target.controller as *mut c_void;
    if controller.is_null() {
        return None;
    }
    let count = read_u32(controller, MESSAGE_COUNT_OFFSET);
    if count == 0 || count > MAX_SUPPORTED_MESSAGES {
        return None;
    }
    let list = std::ptr::read_unaligned(controller.cast::<u8>().add(MESSAGE_LIST_OFFSET).cast::<*mut u8>());
    if list.is_null() {
        return None;
    }
    (0..count).find_map(|index| {
        let message = list.add(index as usize * MESSAGE_SIZE);
        let token = std::ptr::read_unaligned(message.add(MESSAGE_TOKEN_OFFSET).cast::<u32>());
        (token == target.token).then_some(message)
    })
}

unsafe fn top_message(controller: *mut c_void) -> Option<MessageIdentity> {
    let count = if controller.is_null() { 0 } else { read_u32(controller, MESSAGE_COUNT_OFFSET) };
    count.checked_sub(1).and_then(|index| message_at(controller, index))
}

unsafe fn contains_message(target: MessageIdentity) -> bool {
    let controller = target.controller as *mut c_void;
    if controller.is_null() {
        return false;
    }
    let count = read_u32(controller, MESSAGE_COUNT_OFFSET);
    if count == 0 || count > MAX_SUPPORTED_MESSAGES {
        return false;
    }
    (0..count).any(|index| message_at(controller, index).is_some_and(|message| message.token == target.token))
}

unsafe fn lost_connection_error_is_active() -> bool {
    let root = std::ptr::read_unaligned(RETAIL_DX11_ONLINE_ROOT as *const *const u8);
    if root.is_null() {
        return false;
    }
    let error = std::ptr::read_unaligned(root.add(0x10).cast::<*const u8>());
    if error.is_null() {
        return false;
    }
    std::ptr::read_unaligned(error.cast::<usize>()) == RETAIL_DX11_LOST_CONNECTION_VTABLE
}

fn is_lost_connection_continue(message: MessageIdentity) -> bool {
    message.kind == 0 && message.button_count == 0
}

fn input_state() -> InputState {
    unsafe {
        InputState {
            mouse_down: GetAsyncKeyState(VK_LBUTTON.0.into()) < 0,
            enter_down: GetAsyncKeyState(VK_RETURN.0.into()) < 0,
            space_down: GetAsyncKeyState(VK_SPACE.0.into()) < 0,
        }
    }
}

fn scene_message_box_add(controller: *mut c_void, descriptor: *mut c_void) {
    unsafe {
        let lost_connection = lost_connection_error_is_active();
        SceneMessageBoxAddHook.call(controller, descriptor);
        let Some(message) = top_message(controller) else {
            info!("NativeDialogRecovery phase=capture result=ignored reason=message-not-present");
            return;
        };
        if !is_lost_connection_continue(message) {
            info!(
                "NativeDialogRecovery phase=capture result=ignored reason=unsupported-message token={} type={} buttons={}",
                message.token, message.kind, message.button_count
            );
            return;
        }

        if let Some(arm) = take_active_internal_restore(Instant::now()) {
            TRACKER
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .capture(message, input_state(), RecoveryMode::InternalLobbyRestore);
            info!(
                "NativeDialogRecovery phase=capture result=armed token={} type={} buttons={} source=internal-lobby-restore invite_id={}",
                message.token, message.kind, message.button_count, arm.invite_id
            );
            return;
        }
        if !lost_connection {
            return;
        }

        TRACKER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .capture(message, input_state(), RecoveryMode::ManualLostConnection);
        info!(
            "NativeDialogRecovery phase=capture result=armed token={} type={} buttons={} source=lost-connection-to-host",
            message.token, message.kind, message.button_count
        );
    }
}

fn scene_message_box_update(controller: *mut c_void, delta_seconds: f32) {
    unsafe {
        SceneMessageBoxUpdateHook.call(controller, delta_seconds);

        let target = TRACKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).target_for(controller as usize);
        let Some(target) = target else {
            return;
        };
        let target_controller = target.controller as *mut c_void;
        let visible = top_message(target_controller) == Some(target);
        let still_present = contains_message(target);
        let decision = TRACKER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .observe(visible, still_present, delta_seconds, input_state());
        let Some((generation, action)) = decision else {
            return;
        };

        if std::ptr::read_unaligned(target_controller.cast::<u8>().add(CONTROLLER_TRANSITION_ACTIVE_OFFSET).cast::<u8>()) != 0 {
            info!(
                "NativeDialogRecovery phase=explicit-action result=ignored reason=controller-transition-active token={} type={}",
                target.token, target.kind
            );
            return;
        }
        let Some(message) = message_pointer(target) else {
            TRACKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
            return;
        };
        let payload = DefaultActionPayload {
            selection: 0,
            event_action: -1,
            callback_context: std::ptr::read_unaligned(message.add(MESSAGE_CALLBACK_CONTEXT_OFFSET).cast::<u32>()),
        };
        let require_dispatch_success = std::ptr::read_unaligned(message.add(MESSAGE_REQUIRE_DISPATCH_SUCCESS_OFFSET).cast::<u8>()) != 0;
        let dispatch: SceneMessageBoxDispatch = std::mem::transmute(RETAIL_DX11_ACTION_DISPATCH);
        let native_result = dispatch(target_controller.cast::<u8>().add(CONTROLLER_ACTION_DISPATCH_OFFSET).cast::<c_void>(), &payload);
        if !require_dispatch_success || native_result {
            let remove: SceneMessageBoxRemove = std::mem::transmute(RETAIL_DX11_MESSAGE_REMOVE);
            remove(target_controller, target.token);
        }
        let consumed = !contains_message(target);
        info!(
            "NativeDialogRecovery phase=explicit-action generation={} action={:?} token={} type={} native_result={} consumed={}",
            generation, action, target.token, target.kind, native_result, consumed
        );
        if consumed {
            TRACKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
        }
    }
}

pub unsafe fn init(addresses: &Addresses) {
    let Some(update_address) = addresses.func_scene_message_box_update else {
        info!("NativeDialogRecovery result=disabled reason=unsupported-game-build");
        return;
    };
    if update_address != RETAIL_DX11_SCENE_UPDATE {
        info!("NativeDialogRecovery result=disabled reason=unverified-message-callback-chain");
        return;
    }

    let update: SceneMessageBoxUpdate = std::mem::transmute(update_address);
    let add: SceneMessageBoxAdd = std::mem::transmute(RETAIL_DX11_MESSAGE_ADD);
    if let Err(err) = SceneMessageBoxUpdateHook.initialize(update, scene_message_box_update) {
        error!("NativeDialogRecovery result=disabled reason=update-hook-init-failed error={err:?}");
        return;
    }
    if let Err(err) = SceneMessageBoxAddHook.initialize(add, scene_message_box_add) {
        error!("NativeDialogRecovery result=disabled reason=add-hook-init-failed error={err:?}");
        return;
    }
    if let Err(err) = SceneMessageBoxAddHook.enable() {
        error!("NativeDialogRecovery result=disabled reason=add-hook-enable-failed error={err:?}");
        return;
    }
    if let Err(err) = SceneMessageBoxUpdateHook.enable() {
        let _ = SceneMessageBoxAddHook.disable();
        error!("NativeDialogRecovery result=disabled reason=update-hook-enable-failed error={err:?}");
        return;
    }

    info!("NativeDialogRecovery result=enabled mode=lost-connection-default-action-rearm");
}

pub unsafe fn deinit() {
    let _ = SceneMessageBoxUpdateHook.disable();
    let _ = SceneMessageBoxAddHook.disable();
    TRACKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    *INTERNAL_RESTORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::is_lost_connection_continue;
    use super::InputState;
    use super::MessageIdentity;
    use super::RecoveryAction;
    use super::RecoveryMode;
    use super::RecoveryTracker;

    fn message(token: u32) -> MessageIdentity {
        MessageIdentity {
            controller: 1,
            token,
            kind: 0,
            button_count: 0,
        }
    }

    #[test]
    fn inherited_input_does_not_accept_a_new_dialog() {
        let mut tracker = RecoveryTracker::new();
        let pressed = InputState {
            mouse_down: true,
            ..InputState::default()
        };
        tracker.capture(message(1), pressed, RecoveryMode::ManualLostConnection);
        assert_eq!(tracker.observe(true, true, 0.5, InputState::default()), None);
    }

    #[test]
    fn deliberate_mouse_release_rearms_default_action() {
        let mut tracker = RecoveryTracker::new();
        tracker.capture(message(1), InputState::default(), RecoveryMode::ManualLostConnection);
        tracker.observe(true, true, 0.5, InputState::default());
        tracker.observe(
            true,
            true,
            0.01,
            InputState {
                mouse_down: true,
                ..InputState::default()
            },
        );
        assert_eq!(tracker.observe(true, true, 0.01, InputState::default()), Some((1, RecoveryAction::MouseRelease)));
    }

    #[test]
    fn covered_dialog_cannot_consume_input() {
        let mut tracker = RecoveryTracker::new();
        tracker.capture(message(1), InputState::default(), RecoveryMode::ManualLostConnection);
        tracker.observe(false, true, 1.0, InputState::default());
        assert_eq!(
            tracker.observe(
                false,
                true,
                0.01,
                InputState {
                    enter_down: true,
                    ..InputState::default()
                }
            ),
            None
        );
    }

    #[test]
    fn removed_dialog_clears_recovery_state() {
        let mut tracker = RecoveryTracker::new();
        tracker.capture(message(1), InputState::default(), RecoveryMode::ManualLostConnection);
        tracker.observe(true, false, 0.1, InputState::default());
        assert_eq!(tracker.target, None);
    }

    #[test]
    fn only_the_lost_connection_continue_action_is_supported() {
        assert!(is_lost_connection_continue(message(1)));
        assert!(!is_lost_connection_continue(MessageIdentity { kind: 2, ..message(1) }));
        assert!(!is_lost_connection_continue(MessageIdentity { button_count: 1, ..message(1) }));
    }

    #[test]
    fn internal_lobby_restore_dispatches_without_player_input() {
        let mut tracker = RecoveryTracker::new();
        tracker.capture(message(1), InputState::default(), RecoveryMode::InternalLobbyRestore);
        assert_eq!(tracker.observe(true, true, 0.01, InputState::default()), None);
        assert_eq!(tracker.observe(true, true, 0.05, InputState::default()), Some((1, RecoveryAction::InternalLobbyRestore)));
    }

    #[test]
    fn recovery_target_is_owned_by_current_controller_only() {
        let mut tracker = RecoveryTracker::new();
        tracker.capture(message(1), InputState::default(), RecoveryMode::ManualLostConnection);
        assert_eq!(tracker.target_for(1), Some(message(1)));
        assert_eq!(tracker.target_for(2), None);
        assert_eq!(tracker.target_for(0), None);
        assert_eq!(tracker.target, Some(message(1)));
    }

    #[test]
    fn clearing_and_rebinding_never_reuses_other_controller_target() {
        let mut tracker = RecoveryTracker::new();
        tracker.capture(message(1), InputState::default(), RecoveryMode::InternalLobbyRestore);
        tracker.clear();
        assert_eq!(tracker.target_for(1), None);
        let other = MessageIdentity { controller: 2, ..message(2) };
        tracker.capture(other, InputState::default(), RecoveryMode::ManualLostConnection);
        assert_eq!(tracker.target_for(1), None);
        assert_eq!(tracker.target_for(2), Some(other));
    }
}
