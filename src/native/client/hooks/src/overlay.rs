use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use hudhook::ImguiRenderLoop;
use imgui::StyleVar;
use server_api::misc::InviteEvent;
use tracing::info;
use tracing::warn;
use windows::core::PCSTR;
use windows::Win32::System::LibraryLoader::GetModuleHandleA;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_F5;

use crate::uplay_r1_loader::Event;

pub(crate) mod contracts;
mod invites;
mod acceptance;
mod presentation;
mod gameplay_hud;
use invites::*;
use presentation::*;

static INITIAL_POPUP_DURATION: Duration = Duration::from_secs(5);
static STATUS_MESSAGE: Mutex<Option<(String, NotificationKind)>> = Mutex::new(None);
static CHINESE_UI: AtomicBool = AtomicBool::new(true);

pub fn notify(message: impl Into<String>) {
    if let Ok(mut slot) = STATUS_MESSAGE.lock() {
        *slot = Some((message.into(), NotificationKind::Info));
    }
}

pub fn notify_error(message: impl Into<String>) {
    if let Ok(mut slot) = STATUS_MESSAGE.lock() {
        *slot = Some((message.into(), NotificationKind::Error));
    }
}

pub(crate) fn set_ui_language(language_code: &str) {
    CHINESE_UI.store(is_chinese_language_code(language_code), Ordering::Release);
}

pub(crate) fn is_chinese_ui() -> bool {
    CHINESE_UI.load(Ordering::Acquire)
}

fn is_chinese_language_code(language_code: &str) -> bool {
    let normalized = language_code.trim().to_ascii_lowercase().replace('_', "-");
    normalized.is_empty() || normalized == "cn" || normalized.starts_with("zh") || normalized.contains("chinese")
}

fn gameplay_mode_label(mode: contracts::GameplayModeKind, chinese: bool) -> &'static str {
    match (mode, chinese) {
        (contracts::GameplayModeKind::Official, true) => "官方",
        (contracts::GameplayModeKind::Official, false) => "Official",
        (contracts::GameplayModeKind::Mod, true) => "模组",
        (contracts::GameplayModeKind::Mod, false) => "MOD",
    }
}

fn current_gameplay_mode_label(chinese: bool) -> &'static str {
    gameplay_mode_label(contracts::current_gameplay_mode(), chinese)
}

fn take_status_message() -> Option<(String, NotificationKind)> {
    STATUS_MESSAGE.lock().ok()?.take()
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub enum Engine {
    DX9,
    DX11,
}

impl Engine {
    pub fn detect() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let fname = exe.file_name()?.to_str()?.to_lowercase();
        match fname.as_str() {
            "blacklist_game.exe" => Some(Self::DX9),
            "blacklist_dx11_game.exe" => Some(Self::DX11),
            _ => None,
        }
    }
}

struct MyRenderLoop {
    tx: mpsc::Sender<Event>,
    invite_notification: Option<(Instant, String, NotificationKind)>,
    new_invites: crossbeam_channel::Receiver<Result<Option<InviteEvent>, crate::api::Error>>,
    invites: InviteController,
    pending_accepts: acceptance::AcceptanceQueue,
    connection_error: Option<crate::api::Error>,
    // Armed on the first frame that Hudhook actually asks us to render.  The
    // graphics hook can be initialized several seconds before a visible game
    // frame, so starting the deadline in `initialize` can hide the card before
    // the player ever has a chance to see it.
    initial_popup: Option<Instant>,
    chinese_ui: bool,
}

pub(crate) fn format_api_error(error: &crate::api::Error, chinese: bool) -> String {
    if chinese {
        match error {
            crate::api::Error::IO(_) | crate::api::Error::Transport(_) => String::from("网络连接失败"),
            crate::api::Error::MissingUrl => String::from("服务器地址未配置"),
            crate::api::Error::GRPCStatus(status) => match status.code() {
                tonic::Code::DeadlineExceeded => String::from("服务器连接超时"),
                tonic::Code::Unauthenticated => String::from("登录已失效，请重启游戏"),
                tonic::Code::NotFound => String::from("邀请已过期或房间已关闭"),
                _ => String::from("服务器暂时不可用"),
            },
            crate::api::Error::LoginFailure => String::from("登录失败"),
            crate::api::Error::InvalidToken(_) => String::from("登录已失效，请重启游戏"),
        }
    } else {
        match error {
            crate::api::Error::IO(_) | crate::api::Error::Transport(_) => String::from("Network connection failed"),
            crate::api::Error::MissingUrl => String::from("Server address is not configured"),
            crate::api::Error::GRPCStatus(status) => match status.code() {
                tonic::Code::DeadlineExceeded => String::from("Server connection timed out"),
                tonic::Code::Unauthenticated => String::from("Login expired · Restart the game"),
                tonic::Code::NotFound => String::from("Invitation expired or the room was closed"),
                _ => String::from("Server is temporarily unavailable"),
            },
            crate::api::Error::LoginFailure => String::from("Login failed"),
            crate::api::Error::InvalidToken(_) => String::from("Login expired · Restart the game"),
        }
    }
}

fn api_error_notification_kind(error: &crate::api::Error) -> NotificationKind {
    match error {
        crate::api::Error::GRPCStatus(status) if status.code() == tonic::Code::Unauthenticated => NotificationKind::AuthError,
        crate::api::Error::InvalidToken(_) => NotificationKind::AuthError,
        _ => NotificationKind::Error,
    }
}

fn invite_error_invalidates_local_entry(error: &crate::api::Error) -> bool {
    matches!(error, crate::api::Error::GRPCStatus(status) if status.code() == tonic::Code::NotFound)
}

fn accepted_invite_notification(kind: i32, sender_name: &str, chinese: bool) -> Option<String> {
    if kind == 4 {
        return None;
    }
    if chinese {
        Some(if matches!(kind, 3 | 5) {
            format!("正在跟随 {sender_name} 进入房间…")
        } else if kind == 2 {
            format!("正在加入 {sender_name} 的队伍…")
        } else {
            format!("正在加入 {sender_name} 的房间…")
        })
    } else {
        Some(format!("Joining {sender_name}..."))
    }
}

impl MyRenderLoop {
    fn initial_popup_remaining(&self) -> Option<Duration> {
        self.initial_popup?.checked_duration_since(Instant::now())
    }

    fn accept_invite_at(&mut self, index: usize) {
        let Some(invite) = self.invites.event(index).cloned() else {
            return;
        };
        let Some(_) = invite.sender.as_ref() else {
            return;
        };
        let invite_id = invite.id;
        let kind = invite.kind;
        let game_session = invite.game_session.clone();

        if !accepted_event_payload_ready(kind, game_session.as_ref()) {
            info!("InviteAcceptDeferredMissingUplayGameSession invite_id={invite_id} kind={kind}");
            self.invite_notification = Some((
                Instant::now(),
                if self.chinese_ui {
                    String::from("正在同步房间信息…")
                } else {
                    String::from("Syncing room information...")
                },
                NotificationKind::Info,
            ));
            return;
        }

        info!("InviteAcceptClicked invite_id={invite_id} kind={kind}");
        self.pending_accepts.submit(invite);
    }

    fn finish_invite_accept(&mut self, invite: InviteEvent, result: Result<(bool, i64), crate::api::Error>) {
        let Some(sender) = invite.sender.as_ref() else {
            return;
        };
        let invite_id = invite.id;
        let sender_id = sender.id.clone();
        let sender_name = sender.username.clone();
        let kind = invite.kind;
        let game_session = invite.game_session.clone();
        let projected_target_id = game_session.as_ref().map_or(0, |session| session.id);
        let proof_sender_id = sender_id.clone();
        match result {
            Ok((true, server_invite_id)) => {
                let game_event = match accepted_event_for_kind(kind, server_invite_id, sender_id, sender_name.clone(), game_session) {
                    Ok(event) => event,
                    Err(message) => {
                        warn!("InviteAcceptedButEventPayloadInvalid invite_id={invite_id} kind={kind} error={message}");
                        self.invite_notification = Some((
                            Instant::now(),
                            if self.chinese_ui {
                                String::from("房间信息未就绪，请稍后重试")
                            } else {
                                String::from("Room information is not ready · Try again shortly")
                            },
                            NotificationKind::Error,
                        ));
                        return;
                    }
                };
                crate::uplay_r1_loader::arm_party_presence_proof(kind, server_invite_id, projected_target_id, &proof_sender_id);
                info!(
                    "PartyPresenceProof47 phase=overlay-arm local_invite_id={invite_id} server_invite_id={server_invite_id} kind={kind} expected_session_id={projected_target_id} behavior_changed=false"
                );
                let event_type = match &game_event {
                    Event::FriendsPrivateBlobInviteAccepted(_) => crate::uplay_r1_loader::PRIVATE_INVITE_MODE_LABEL,
                    Event::PartyGameInviteAccepted(_) => "PartyGameInviteAccepted",
                    Event::FriendsGameInviteAccepted(_, _, _) => "FriendsGameInviteAccepted",
                    Event::FriendsFriendListUpdated(_) => "FriendsFriendListUpdated",
                    Event::FriendsFriendListRetry(_) => "FriendsFriendListRetry",
                    Event::PartyMemberListChanged => "PartyMemberListChanged",
                    Event::UserAccountSharing => "UserAccountSharing",
                };
                if kind == 1 {
                    info!(
                        "PrivateInviteFriendsSearchRoute local_invite_id={invite_id} server_invite_id={server_invite_id} projected_target_id={projected_target_id} event=FriendsGameInviteAccepted standard_friends_abi=true uplay_payload_used=false expected_flow=Search-NAT-JoinSession"
                    );
                }
                if kind == 4 {
                    crate::hooks::native_dialog::arm_internal_lobby_restore(server_invite_id);
                }
                if self.tx.send(game_event).is_err() {
                    if kind == 4 {
                        crate::hooks::native_dialog::cancel_internal_lobby_restore(server_invite_id);
                    }
                    self.invite_notification = Some((
                        Instant::now(),
                        if self.chinese_ui {
                            String::from("邀请处理失败，请重启游戏")
                        } else {
                            String::from("Invitation failed · Restart the game")
                        },
                        NotificationKind::Error,
                    ));
                    return;
                }
                info!("InviteAcceptedEventQueued local_invite_id={invite_id} server_invite_id={server_invite_id} kind={kind} event_type={event_type}");
                self.invites.remove_id(invite_id);
                if let Some(message) = accepted_invite_notification(kind, &sender_name, self.chinese_ui) {
                    self.invite_notification = Some((Instant::now(), message, NotificationKind::Info));
                }
            }
            Ok((false, server_invite_id)) => {
                info!("InviteRejected local_invite_id={invite_id} server_invite_id={server_invite_id} kind={kind}");
                self.invites.remove_id(invite_id);
                self.invite_notification = Some((
                    Instant::now(),
                    if self.chinese_ui {
                        String::from("邀请已失效，请让好友重新邀请")
                    } else {
                        String::from("Invitation expired · Ask your friend to invite you again")
                    },
                    NotificationKind::Error,
                ));
            }
            Err(error) => {
                if invite_error_invalidates_local_entry(&error) {
                    self.invites.remove_id(invite_id);
                    info!("InviteInvalidated local_invite_id={invite_id} kind={kind} reason=server_not_found");
                }
                let kind = api_error_notification_kind(&error);
                self.invite_notification = Some((Instant::now(), format_api_error(&error, self.chinese_ui), kind));
            }
        }
    }

    fn show_notifications(&mut self, ui: &imgui::Ui) {
        if let Some((created, message, kind)) = self.invite_notification.as_ref() {
            let elapsed = created.elapsed();
            let lifetime = kind.lifetime();
            if lifetime.is_some_and(|value| elapsed > value) {
                self.invite_notification.take();
            } else {
                let alpha = lifetime.map_or_else(
                    || (elapsed.as_secs_f32() / CARD_FADE_DURATION.as_secs_f32()).clamp(0.0, 1.0),
                    |value| timed_card_alpha(elapsed, value),
                );
                let layout = card_layout(ui.io().display_size);
                let tone = kind.tone();
                let mut y = layout.top;
                if self.connection_error.is_some() {
                    y += layout.stack_step(CardTone::Error);
                }
                let _padding = ui.push_style_var(StyleVar::WindowPadding([18.0 * layout.scale, 11.0 * layout.scale]));
                ui.window(if self.chinese_ui {
                    "提示##scbl_notification"
                } else {
                    "Notification##scbl_notification"
                })
                .bg_alpha(0.0)
                .no_decoration()
                .no_inputs()
                .no_nav()
                .movable(false)
                .resizable(false)
                .menu_bar(false)
                .size([layout.width, layout.height(tone)], imgui::Condition::Always)
                .position([layout.x, y], imgui::Condition::Always)
                .build(|| {
                    draw_tactical_card(ui, layout, tone, message, self.chinese_ui, elapsed, alpha, lifetime);
                });
            }
        }
    }

    fn show_initial_info(&self, ui: &imgui::Ui) {
        if let Some(remaining) = self.initial_popup_remaining() {
            let elapsed = INITIAL_POPUP_DURATION.saturating_sub(remaining);
            let alpha = timed_card_alpha(elapsed, INITIAL_POPUP_DURATION);
            let layout = card_layout(ui.io().display_size);
            let tone = CardTone::Ready;
            let _padding = ui.push_style_var(StyleVar::WindowPadding([18.0 * layout.scale, 11.0 * layout.scale]));
            ui.window(if self.chinese_ui {
                "联机组件##scbl_initial"
            } else {
                "Online component##scbl_initial"
            })
            .bg_alpha(0.0)
            .no_decoration()
            .no_inputs()
            .no_nav()
            .movable(false)
            .resizable(false)
            .menu_bar(false)
            .size([layout.width, layout.height(tone)], imgui::Condition::Always)
            .position([layout.x, layout.top], imgui::Condition::Always)
            .build(|| {
                let mode = current_gameplay_mode_label(self.chinese_ui);
                let message = if self.chinese_ui {
                    format!("联机服务已就绪 · {mode}")
                } else {
                    format!("Online services ready · {mode}")
                };
                draw_tactical_card(ui, layout, tone, &message, self.chinese_ui, elapsed, alpha, Some(INITIAL_POPUP_DURATION));
            });
        }
    }

    fn show_errors(&mut self, ui: &imgui::Ui) {
        if let Some(error) = &self.connection_error {
            let layout = card_layout(ui.io().display_size);
            let tone = CardTone::Error;
            let elapsed = Duration::from_secs_f64(ui.time());
            let message = format_api_error(error, self.chinese_ui);
            let _padding = ui.push_style_var(StyleVar::WindowPadding([18.0 * layout.scale, 11.0 * layout.scale]));
            ui.window(if self.chinese_ui { "服务器错误##scbl_error" } else { "Server Error##scbl_error" })
                .bg_alpha(0.0)
                .no_decoration()
                .no_inputs()
                .no_nav()
                .movable(false)
                .resizable(false)
                .menu_bar(false)
                .size([layout.width, layout.height(tone)], imgui::Condition::Always)
                .position([layout.x, layout.top], imgui::Condition::Always)
                .build(|| {
                    draw_tactical_card(ui, layout, tone, &message, self.chinese_ui, elapsed, 1.0, None);
                });
        }
    }
}

#[cfg(test)]
mod invitation_shortcut_tests {
    use super::*;

    #[test]
    fn overlay_language_follows_game_language_code() {
        assert!(is_chinese_language_code("zh-CN"));
        assert!(is_chinese_language_code("schinese"));
        assert!(!is_chinese_language_code("en-US"));
        assert!(!is_chinese_language_code("English"));
    }

    #[test]
    fn gameplay_modes_use_the_approved_player_facing_terms() {
        assert_eq!(gameplay_mode_label(contracts::GameplayModeKind::Official, true), "官方");
        assert_eq!(gameplay_mode_label(contracts::GameplayModeKind::Official, false), "Official");
        assert_eq!(gameplay_mode_label(contracts::GameplayModeKind::Mod, true), "模组");
        assert_eq!(gameplay_mode_label(contracts::GameplayModeKind::Mod, false), "MOD");
    }

    #[test]
    fn notification_breathing_has_a_visible_but_restrained_range() {
        let minimum = breathing_brightness(Duration::from_millis(1_800));
        let maximum = breathing_brightness(Duration::from_millis(600));
        assert!((minimum - 0.84).abs() < 0.001);
        assert!((maximum - 1.0).abs() < 0.001);
    }

    #[test]
    fn internal_lobby_restore_has_no_player_notification() {
        assert_eq!(accepted_invite_notification(4, "self", true), None);
        assert_eq!(accepted_invite_notification(4, "self", false), None);
        assert!(accepted_invite_notification(1, "friend", true).is_some());
    }

    #[test]
    fn only_server_not_found_invalidates_an_invite_card() {
        assert!(invite_error_invalidates_local_entry(&crate::api::Error::GRPCStatus(tonic::Status::not_found("expired"))));
        assert!(!invite_error_invalidates_local_entry(&crate::api::Error::GRPCStatus(tonic::Status::unavailable("retry"))));
    }
}

impl ImguiRenderLoop for MyRenderLoop {
    fn initialize(&mut self, ctx: &mut imgui::Context, _render_context: &mut dyn hudhook::RenderContext) {
        sc_style(ctx.style_mut());
        let chinese_font_available = setup_fonts(ctx);
        if self.chinese_ui && !chinese_font_available {
            warn!("Chinese overlay requested but no Chinese font was found; falling back to English");
            self.chinese_ui = false;
        }
        // Both the system CJK font and the bundled fallback now use the same
        // base pixel size. Per-window scaling follows the active viewport.
        ctx.io_mut().font_global_scale = 1.0;
        // The welcome-card timer is armed in `render`, which is the first point
        // at which we know the game can actually present overlay pixels.
        self.initial_popup = None;
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        // Gameplay HUD is independent of the welcome/invitation cards.
        gameplay_hud::render(ui);
        while let Some((invite, result)) = self.pending_accepts.poll() {
            self.finish_invite_accept(invite, result);
        }
        if self.initial_popup.is_none() {
            self.initial_popup = Some(Instant::now() + INITIAL_POPUP_DURATION);
            info!("Initial overlay card armed on first rendered frame");
        }

        if let Some((message, kind)) = take_status_message() {
            self.invite_notification = Some((Instant::now(), message, kind));
        }

        #[allow(clippy::cast_possible_wrap)]
        let f5 = unsafe { GetAsyncKeyState(VK_F5.0.into()) & 0x8000u16 as i16 != 0 };
        if let Some(index) = self.invites.accept_key_pressed(f5) {
            self.accept_invite_at(index);
        }

        if let Some((index, invite_id)) = self.invites.take_due_party_follow(Instant::now()) {
            info!("PartyFollowAutoAcceptDue local_invite_id={invite_id} delay_ms={}", PARTY_FOLLOW_ACCEPT_DELAY.as_millis());
            self.accept_invite_at(index);
        }

        // Rendering can pause while the game is minimized or its swapchain is
        // rebuilt. Drain every result accumulated during that pause so an
        // invitation cannot remain hidden behind stale transport errors.
        while let Ok(evt) = self.new_invites.try_recv() {
            match evt {
                Err(error) => self.connection_error = Some(error),
                Ok(evt) => {
                    self.connection_error = None;
                    if evt.is_none() && self.invite_notification.as_ref().is_some_and(|(_, _, kind)| *kind == NotificationKind::AuthError) {
                        self.invite_notification = None;
                    }
                    if let Some(evt) = evt {
                        if !crate::api::invite_is_pending(evt.id) {
                            continue;
                        }
                        let update = self.invites.upsert(evt, self.chinese_ui);
                        // The server may redeliver the same invitation for
                        // transport reliability. A repeated server ID updates
                        // its payload in place but must never restart the card
                        // countdown or make an expired-looking invite reappear.
                        if update.is_new {
                            if let Some(message) = update.message {
                                self.invite_notification = Some((Instant::now(), message, NotificationKind::Invite));
                            }
                        }
                        let auto_join = update.force_join || hooks_config::get().is_some_and(|config| config.auto_join_invite);
                        if auto_join {
                            let invite_id = update.invite_id;
                            let kind = update.kind;
                            if matches!(kind, 3 | 5) {
                                self.invites.schedule_party_follow(invite_id, Instant::now());
                                info!(
                                    "PartyFollowAutoAcceptScheduled local_invite_id={invite_id} delay_ms={}",
                                    PARTY_FOLLOW_ACCEPT_DELAY.as_millis()
                                );
                            } else {
                                self.accept_invite_at(update.index);
                            }
                        }
                    }
                }
            }
        }

        // The welcome message yields immediately to actionable information;
        // it never pushes an invitation or a persistent error down the screen.
        if self.connection_error.is_none() && self.invite_notification.is_none() {
            self.show_initial_info(ui);
        }
        self.show_errors(ui);
        self.show_notifications(ui);
    }
}

fn init_hudhook<T: hudhook::Hooks + 'static>(invites: crossbeam_channel::Receiver<Result<Option<InviteEvent>, crate::api::Error>>) -> anyhow::Result<()> {
    let tx = crate::uplay_r1_loader::event_sender();
    hudhook::Hudhook::builder()
        .with::<T>(MyRenderLoop {
            tx,
            invite_notification: None,
            new_invites: invites,
            invites: InviteController::new(),
            pending_accepts: acceptance::AcceptanceQueue::default(),
            connection_error: None,
            initial_popup: None,
            chinese_ui: is_chinese_ui(),
        })
        .with_hmodule(unsafe { GetModuleHandleA(PCSTR::null())?.into() })
        .build()
        .apply()
        .map_err(|e| anyhow::anyhow!("Error adding gui hook: {e:?}"))
}

fn init_dx9(invites: crossbeam_channel::Receiver<Result<Option<InviteEvent>, crate::api::Error>>) -> anyhow::Result<()> {
    init_hudhook::<hudhook::hooks::dx9::ImguiDx9Hooks>(invites)
}

fn init_dx11(invites: crossbeam_channel::Receiver<Result<Option<InviteEvent>, crate::api::Error>>) -> anyhow::Result<()> {
    init_hudhook::<hudhook::hooks::dx11::ImguiDx11Hooks>(invites)
}

pub fn init(engine: Engine, invites: crossbeam_channel::Receiver<Result<Option<InviteEvent>, crate::api::Error>>) -> anyhow::Result<()> {
    match engine {
        Engine::DX9 => init_dx9(invites),
        Engine::DX11 => init_dx11(invites),
    }
}
