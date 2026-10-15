use std::time::Duration;

use imgui::Style;
use imgui::StyleColor;
use tracing::info;

const INFO_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(5);
const INVITE_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const CARD_FADE_DURATION: Duration = Duration::from_millis(360);

const REFERENCE_WIDTH: f32 = 1920.0;
const REFERENCE_HEIGHT: f32 = 1080.0;
const MIN_UI_SCALE: f32 = 0.82;
const MAX_UI_SCALE: f32 = 1.35;
const BASE_CARD_X: f32 = 28.0;
const BASE_CARD_TOP: f32 = 32.0;
const BASE_CARD_WIDTH: f32 = 390.0;
const BASE_CARD_HEIGHT: f32 = 88.0;
const BASE_INVITE_CARD_HEIGHT: f32 = 112.0;
const BASE_CARD_STACK_GAP: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CardLayout {
    pub x: f32,
    pub top: f32,
    pub width: f32,
    pub scale: f32,
}

impl CardLayout {
    pub(super) fn height(self, tone: CardTone) -> f32 {
        let base = if tone == CardTone::Invite { BASE_INVITE_CARD_HEIGHT } else { BASE_CARD_HEIGHT };
        base * self.scale
    }

    pub(super) fn stack_step(self, tone: CardTone) -> f32 {
        self.height(tone) + BASE_CARD_STACK_GAP * self.scale
    }
}

pub(super) fn card_layout(display_size: [f32; 2]) -> CardLayout {
    let width = display_size[0].max(1.0);
    let height = display_size[1].max(1.0);
    let scale = (width / REFERENCE_WIDTH).min(height / REFERENCE_HEIGHT).clamp(MIN_UI_SCALE, MAX_UI_SCALE);
    let x = BASE_CARD_X * scale;
    let available_width = (width - x * 2.0).max(240.0 * scale);

    CardLayout {
        x,
        top: BASE_CARD_TOP * scale,
        width: (BASE_CARD_WIDTH * scale).min(available_width),
        scale,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NotificationKind {
    Info,
    Invite,
    Error,
    AuthError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardTone {
    Ready,
    Info,
    Invite,
    Error,
}

impl CardTone {
    fn accent(self) -> [f32; 4] {
        match self {
            Self::Ready => [0.25, 0.92, 0.34, 1.0],
            Self::Info => [0.62, 0.82, 0.88, 1.0],
            Self::Invite => [0.32, 0.82, 0.92, 1.0],
            Self::Error => [0.98, 0.27, 0.30, 1.0],
        }
    }
}

impl NotificationKind {
    pub(super) fn lifetime(self) -> Option<Duration> {
        match self {
            Self::Info => Some(INFO_NOTIFICATION_TIMEOUT),
            Self::Invite => Some(INVITE_NOTIFICATION_TIMEOUT),
            Self::Error | Self::AuthError => None,
        }
    }

    pub(super) fn tone(self) -> CardTone {
        match self {
            Self::Info => CardTone::Info,
            Self::Invite => CardTone::Invite,
            Self::Error | Self::AuthError => CardTone::Error,
        }
    }
}

pub(super) fn timed_card_alpha(elapsed: Duration, lifetime: Duration) -> f32 {
    let fade = CARD_FADE_DURATION.as_secs_f32();
    let fade_in = (elapsed.as_secs_f32() / fade).clamp(0.0, 1.0);
    let remaining = lifetime.saturating_sub(elapsed).as_secs_f32();
    let fade_out = (remaining / fade).clamp(0.0, 1.0);
    fade_in.min(fade_out)
}

pub(super) fn breathing_brightness(elapsed: Duration) -> f32 {
    // Keep the copy bright enough to stand out from Blacklist's busy menus while
    // retaining a restrained breathing cue. A darker minimum looked acceptable
    // on screenshots but was too easy to miss during play.
    let phase = elapsed.as_secs_f32() * std::f32::consts::TAU / 2.4;
    0.84 + 0.16 * (phase.sin() * 0.5 + 0.5)
}

fn with_alpha(mut color: [f32; 4], alpha: f32) -> [f32; 4] {
    color[3] *= alpha;
    color
}

fn draw_card_background(ui: &imgui::Ui, tone: CardTone, alpha: f32, scale: f32) {
    let position = ui.window_pos();
    let size = ui.window_size();
    let bottom_right = [position[0] + size[0], position[1] + size[1]];
    let draw_list = ui.get_window_draw_list();
    draw_list.add_rect_filled_multicolor(
        position,
        bottom_right,
        [0.006, 0.010, 0.012, 0.88 * alpha],
        [0.006, 0.010, 0.012, 0.68 * alpha],
        [0.004, 0.007, 0.009, 0.74 * alpha],
        [0.004, 0.007, 0.009, 0.91 * alpha],
    );

    let accent = with_alpha(tone.accent(), 0.92 * alpha);
    draw_list
        .add_rect(position, [position[0] + 3.0 * scale, position[1] + size[1]], accent)
        .filled(true)
        .build();
}

fn draw_countdown_bar(ui: &imgui::Ui, tone: CardTone, elapsed: Duration, lifetime: Duration, alpha: f32, scale: f32) {
    let remaining = 1.0 - (elapsed.as_secs_f32() / lifetime.as_secs_f32()).clamp(0.0, 1.0);
    let position = ui.window_pos();
    let size = ui.window_size();
    let left = position[0];
    let right = left + size[0] * remaining;
    let y = position[1] + size[1] - 1.5 * scale;
    let draw_list = ui.get_window_draw_list();
    draw_list
        .add_line([left, y], [position[0] + size[0], y], [0.72, 0.78, 0.80, 0.16 * alpha])
        .thickness(1.5 * scale)
        .build();
    draw_list
        .add_line([left, y], [right, y], with_alpha(tone.accent(), 0.82 * alpha))
        .thickness(2.0 * scale)
        .build();
}

fn draw_status_header(ui: &imgui::Ui, tone: CardTone, pulse: f32, alpha: f32, scale: f32) {
    let cursor = ui.cursor_pos();
    let screen = ui.cursor_screen_pos();
    let radius = 2.8 * scale;
    let center = [screen[0] + radius, screen[1] + ui.current_font_size() * 0.5];
    let draw_list = ui.get_window_draw_list();
    draw_list.add_circle(center, radius, with_alpha(tone.accent(), pulse * alpha)).filled(true).build();
    ui.set_cursor_pos([cursor[0] + 13.0 * scale, cursor[1]]);
    ui.text_colored([0.72, 0.78, 0.81, alpha], "SCBL CONNECT");
    let next_y = ui.cursor_pos()[1];
    ui.set_cursor_pos([cursor[0], next_y]);
}

fn invite_text<'a>(message: &'a str, chinese: bool) -> (&'a str, Option<&'static str>) {
    let suffix = if chinese { " · F5 接受" } else { " · F5 to accept" };
    if let Some(body) = message.strip_suffix(suffix) {
        (body, Some(if chinese { "F5  接受" } else { "F5  ACCEPT" }))
    } else {
        (message, None)
    }
}

fn draw_action_hint(ui: &imgui::Ui, hint: &str, tone: CardTone, alpha: f32, scale: f32) {
    let position = ui.cursor_screen_pos();
    let text_size = ui.calc_text_size(hint);
    let horizontal_padding = 6.0 * scale;
    let vertical_padding = 2.0 * scale;
    let draw_list = ui.get_window_draw_list();
    draw_list
        .add_rect(
            [position[0] - horizontal_padding, position[1] - vertical_padding],
            [position[0] + text_size[0] + horizontal_padding, position[1] + text_size[1] + vertical_padding],
            with_alpha(tone.accent(), 0.17 * alpha),
        )
        .filled(true)
        .build();
    draw_list
        .add_rect(
            [position[0] - horizontal_padding, position[1] - vertical_padding],
            [position[0] + text_size[0] + horizontal_padding, position[1] + text_size[1] + vertical_padding],
            with_alpha(tone.accent(), 0.62 * alpha),
        )
        .thickness(scale.max(1.0))
        .build();
    ui.text_colored(with_alpha(tone.accent(), alpha), hint);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_tactical_card(ui: &imgui::Ui, layout: CardLayout, tone: CardTone, message: &str, chinese: bool, elapsed: Duration, alpha: f32, lifetime: Option<Duration>) {
    ui.set_window_font_scale(layout.scale);
    draw_card_background(ui, tone, alpha, layout.scale);
    let pulse = breathing_brightness(elapsed);
    draw_status_header(ui, tone, pulse, alpha, layout.scale);
    ui.dummy([0.0, 2.0 * layout.scale]);

    let (body, action) = if tone == CardTone::Invite { invite_text(message, chinese) } else { (message, None) };
    let _text_color = ui.push_style_color(StyleColor::Text, [0.94, 0.97, 0.98, alpha]);
    ui.text_wrapped(body);
    drop(_text_color);

    if let Some(hint) = action {
        ui.dummy([0.0, 3.0 * layout.scale]);
        draw_action_hint(ui, hint, tone, alpha, layout.scale);
    }

    if let Some(lifetime) = lifetime {
        draw_countdown_bar(ui, tone, elapsed, lifetime, alpha, layout.scale);
    }
}

pub(super) fn sc_style(style: &mut Style) {
    style.colors[StyleColor::Text as usize] = [0.94, 0.96, 0.98, 1.00];
    style.colors[StyleColor::TextDisabled as usize] = [0.50, 0.50, 0.50, 1.00];
    // Notification windows draw their own subtle left-to-right gradient.
    style.colors[StyleColor::WindowBg as usize] = [0.004, 0.007, 0.009, 0.00];
    style.colors[StyleColor::ChildBg as usize] = [0.00, 0.00, 0.00, 0.00];
    style.colors[StyleColor::PopupBg as usize] = [0.004, 0.007, 0.009, 0.992];
    style.colors[StyleColor::Border as usize] = [1.00, 1.00, 1.00, 0.00];
    style.colors[StyleColor::BorderShadow as usize] = [0.00, 0.00, 0.00, 0.55];
    style.colors[StyleColor::FrameBg as usize] = [1.00, 1.00, 1.00, 0.08];
    style.colors[StyleColor::FrameBgHovered as usize] = [1.00, 1.00, 1.00, 0.16];
    style.colors[StyleColor::FrameBgActive as usize] = [1.00, 1.00, 1.00, 0.24];
    style.colors[StyleColor::TitleBg as usize] = [0.04, 0.04, 0.05, 1.00];
    style.colors[StyleColor::TitleBgActive as usize] = [0.09, 0.09, 0.10, 1.00];
    style.colors[StyleColor::TitleBgCollapsed as usize] = [0.04, 0.04, 0.05, 0.72];
    style.colors[StyleColor::MenuBarBg as usize] = [0.06, 0.06, 0.07, 1.00];
    style.colors[StyleColor::ScrollbarBg as usize] = [0.00, 0.00, 0.00, 0.42];
    style.colors[StyleColor::ScrollbarGrab as usize] = [0.42, 0.42, 0.44, 1.00];
    style.colors[StyleColor::ScrollbarGrabHovered as usize] = [0.68, 0.68, 0.70, 1.00];
    style.colors[StyleColor::ScrollbarGrabActive as usize] = [0.88, 0.88, 0.90, 1.00];
    style.colors[StyleColor::CheckMark as usize] = [0.94, 0.94, 0.94, 1.00];
    style.colors[StyleColor::SliderGrab as usize] = [0.68, 0.68, 0.70, 1.00];
    style.colors[StyleColor::SliderGrabActive as usize] = [0.96, 0.96, 0.96, 1.00];
    style.colors[StyleColor::Button as usize] = [1.00, 1.00, 1.00, 0.10];
    style.colors[StyleColor::ButtonHovered as usize] = [1.00, 1.00, 1.00, 0.20];
    style.colors[StyleColor::ButtonActive as usize] = [1.00, 1.00, 1.00, 0.28];
    style.colors[StyleColor::Header as usize] = [1.00, 1.00, 1.00, 0.10];
    style.colors[StyleColor::HeaderHovered as usize] = [1.00, 1.00, 1.00, 0.20];
    style.colors[StyleColor::HeaderActive as usize] = [1.00, 1.00, 1.00, 0.28];
    style.colors[StyleColor::Separator as usize] = [1.00, 1.00, 1.00, 0.22];
    style.colors[StyleColor::SeparatorHovered as usize] = [1.00, 1.00, 1.00, 0.45];
    style.colors[StyleColor::SeparatorActive as usize] = [1.00, 1.00, 1.00, 0.70];
    style.colors[StyleColor::ResizeGrip as usize] = [1.00, 1.00, 1.00, 0.14];
    style.colors[StyleColor::ResizeGripHovered as usize] = [1.00, 1.00, 1.00, 0.34];
    style.colors[StyleColor::ResizeGripActive as usize] = [1.00, 1.00, 1.00, 0.54];
    style.colors[StyleColor::Tab as usize] = [0.12, 0.12, 0.13, 0.86];
    style.colors[StyleColor::TabHovered as usize] = [0.32, 0.32, 0.34, 0.92];
    style.colors[StyleColor::TabActive as usize] = [0.24, 0.24, 0.26, 1.00];
    style.colors[StyleColor::TabUnfocused as usize] = [0.06, 0.06, 0.07, 0.97];
    style.colors[StyleColor::TabUnfocusedActive as usize] = [0.16, 0.16, 0.17, 1.00];
    style.colors[StyleColor::PlotLines as usize] = [0.61, 0.61, 0.61, 1.00];
    style.colors[StyleColor::PlotLinesHovered as usize] = [1.00, 0.43, 0.35, 1.00];
    style.colors[StyleColor::PlotHistogram as usize] = [0.90, 0.70, 0.00, 1.00];
    style.colors[StyleColor::PlotHistogramHovered as usize] = [1.00, 0.60, 0.00, 1.00];
    style.colors[StyleColor::TableHeaderBg as usize] = [0.19, 0.19, 0.20, 1.00];
    style.colors[StyleColor::TableBorderStrong as usize] = [0.31, 0.31, 0.35, 1.00];
    style.colors[StyleColor::TableBorderLight as usize] = [0.23, 0.23, 0.25, 1.00];
    style.colors[StyleColor::TableRowBg as usize] = [0.00, 0.00, 0.00, 0.00];
    style.colors[StyleColor::TableRowBgAlt as usize] = [1.00, 1.00, 1.00, 0.06];
    style.colors[StyleColor::TextSelectedBg as usize] = [1.00, 1.00, 1.00, 0.24];
    style.colors[StyleColor::DragDropTarget as usize] = [1.00, 1.00, 0.00, 0.90];
    style.colors[StyleColor::NavHighlight as usize] = [0.26, 0.98, 0.35, 1.00];
    style.colors[StyleColor::NavWindowingHighlight as usize] = [1.00, 1.00, 1.00, 0.70];
    style.colors[StyleColor::NavWindowingDimBg as usize] = [0.80, 0.80, 0.80, 0.20];
    style.colors[StyleColor::ModalWindowDimBg as usize] = [0.00, 0.00, 0.00, 0.62];
    style.window_padding = [18.0, 11.0];
    style.window_rounding = 0.0;
    style.frame_rounding = 0.0;
    style.item_spacing = [10.0, 6.0];
}

pub(super) fn setup_fonts(imgui: &mut imgui::Context) -> bool {
    let font_size = 17.0;
    for path in [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ] {
        let Ok(data) = std::fs::read(path) else {
            continue;
        };
        imgui.fonts().add_font(&[imgui::FontSource::TtfData {
            data: data.as_slice(),
            size_pixels: font_size,
            config: Some(imgui::FontConfig {
                name: Some(String::from("SCBL Chinese UI")),
                glyph_ranges: imgui::FontGlyphRanges::chinese_full(),
                ..imgui::FontConfig::default()
            }),
        }]);
        info!("Loaded Chinese overlay font from {path}");
        return true;
    }

    imgui.fonts().add_font(&[imgui::FontSource::TtfData {
        data: include_bytes!("../../assets/Orbitron-Regular.ttf"),
        size_pixels: font_size,
        config: Some(imgui::FontConfig {
            name: Some(String::from("Orbitron")),
            ..imgui::FontConfig::default()
        }),
    }]);
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_layout_scales_and_stays_inside_common_viewports() {
        let hd = card_layout([1280.0, 720.0]);
        let full_hd = card_layout([1920.0, 1080.0]);
        let ultra_hd = card_layout([3840.0, 2160.0]);

        assert_eq!(hd.scale, MIN_UI_SCALE);
        assert_eq!(full_hd.scale, 1.0);
        assert_eq!(ultra_hd.scale, MAX_UI_SCALE);
        for (layout, width) in [(hd, 1280.0), (full_hd, 1920.0), (ultra_hd, 3840.0)] {
            assert!(layout.x >= 0.0);
            assert!(layout.width > 0.0);
            assert!(layout.x + layout.width < width);
        }
    }

    #[test]
    fn invite_action_is_separated_from_the_message() {
        assert_eq!(invite_text("BaiYe 邀请加入私人房间 · F5 接受", true), ("BaiYe 邀请加入私人房间", Some("F5  接受")));
        assert_eq!(
            invite_text("BaiYe invited you to a party · F5 to accept", false),
            ("BaiYe invited you to a party", Some("F5  ACCEPT"))
        );
        assert_eq!(invite_text("正在跟随 BaiYe…", true), ("正在跟随 BaiYe…", None));
    }
}
