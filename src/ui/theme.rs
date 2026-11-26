use egui::{Color32, Context, CornerRadius, Frame, Margin, Stroke, Vec2};

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub light: bool,
    pub bg_canvas: Color32,
    pub bg_panel: Color32,
    pub bg_card: Color32,
    pub bg_sub: Color32,
    pub bg_chip: Color32,
    #[allow(dead_code)]
    pub bg_pill: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text_primary: Color32,
    pub text_secondary: Color32,
    pub text_muted: Color32,
    pub green: Color32,
    pub green_text: Color32,
    pub red: Color32,
    pub red_text: Color32,
    pub blue: Color32,
    #[allow(dead_code)]
    pub cyan: Color32,
    pub amber: Color32,
    pub grid_line: Color32,
    pub px_col_bg: Color32,
    pub ladder_hdr_bg: Color32,
    pub tooltip_bg: Color32,
    pub tooltip_fg: Color32,
    pub ind_ema: Color32,
    pub ind_sma: Color32,
    pub ind_bb: Color32,
    pub ind_vwap: Color32,
    pub row_selected_tint: Color32,
    pub row_hover_tint: Color32,
    pub crosshair: Color32,
    pub cta_bg: Color32,
    pub cta_fg: Color32,
    pub cta_border: Color32,
}

impl ThemePalette {
    /// Matte Graphite Desk dark palette.
    pub fn dark() -> Self {
        Self {
            light: false,
            bg_canvas: Color32::from_rgb(15, 16, 19),
            bg_panel: Color32::from_rgb(20, 22, 26),
            bg_card: Color32::from_rgb(24, 26, 31),
            bg_sub: Color32::from_rgb(18, 20, 24),
            bg_chip: Color32::from_rgb(30, 33, 40),
            bg_pill: Color32::from_rgb(30, 33, 40),
            border: Color32::from_rgb(39, 43, 52),
            border_strong: Color32::from_rgb(59, 65, 80),
            text_primary: Color32::from_rgb(243, 244, 246),
            text_secondary: Color32::from_rgb(185, 192, 202),
            text_muted: Color32::from_rgb(122, 130, 144),
            green: Color32::from_rgb(34, 197, 94),
            green_text: Color32::from_rgb(134, 239, 172),
            red: Color32::from_rgb(239, 68, 68),
            red_text: Color32::from_rgb(252, 165, 165),
            blue: Color32::from_rgb(59, 130, 246),
            cyan: Color32::from_rgb(100, 116, 139),
            amber: Color32::from_rgb(234, 179, 8),
            grid_line: Color32::from_rgba_unmultiplied(255, 255, 255, 12),
            px_col_bg: Color32::from_rgb(20, 22, 27),
            ladder_hdr_bg: Color32::from_rgb(18, 20, 24),
            tooltip_bg: Color32::from_rgba_unmultiplied(20, 22, 26, 246),
            tooltip_fg: Color32::from_rgb(243, 244, 246),
            ind_ema: Color32::from_rgb(96, 165, 250),
            ind_sma: Color32::from_rgb(234, 179, 8),
            ind_bb: Color32::from_rgb(129, 140, 248),
            ind_vwap: Color32::from_rgb(168, 139, 250),
            row_selected_tint: Color32::from_rgba_unmultiplied(255, 255, 255, 10),
            row_hover_tint: Color32::from_rgba_unmultiplied(255, 255, 255, 6),
            crosshair: Color32::from_rgba_unmultiplied(243, 244, 246, 56),
            cta_bg: Color32::from_rgb(35, 39, 48),
            cta_fg: Color32::from_rgb(243, 244, 246),
            cta_border: Color32::from_rgb(59, 65, 80),
        }
    }

    /// Warm Paper Desk light palette.
    pub fn light() -> Self {
        Self {
            light: true,
            bg_canvas: Color32::from_rgb(245, 244, 239),
            bg_panel: Color32::from_rgb(255, 255, 255),
            bg_card: Color32::from_rgb(255, 255, 255),
            bg_sub: Color32::from_rgb(249, 248, 245),
            bg_chip: Color32::from_rgb(239, 237, 230),
            bg_pill: Color32::from_rgb(239, 237, 230),
            border: Color32::from_rgb(223, 220, 210),
            border_strong: Color32::from_rgb(196, 192, 180),
            text_primary: Color32::from_rgb(22, 22, 20),
            text_secondary: Color32::from_rgb(78, 77, 72),
            text_muted: Color32::from_rgb(107, 105, 97),
            green: Color32::from_rgb(22, 128, 61),
            green_text: Color32::from_rgb(21, 101, 52),
            red: Color32::from_rgb(185, 28, 28),
            red_text: Color32::from_rgb(153, 27, 27),
            blue: Color32::from_rgb(29, 78, 216),
            cyan: Color32::from_rgb(71, 85, 105),
            amber: Color32::from_rgb(180, 83, 9),
            grid_line: Color32::from_rgba_unmultiplied(22, 22, 20, 14),
            px_col_bg: Color32::from_rgb(245, 244, 239),
            ladder_hdr_bg: Color32::from_rgb(249, 248, 245),
            tooltip_bg: Color32::from_rgba_unmultiplied(22, 22, 20, 242),
            tooltip_fg: Color32::from_rgb(249, 248, 245),
            ind_ema: Color32::from_rgb(29, 78, 216),
            ind_sma: Color32::from_rgb(180, 83, 9),
            ind_bb: Color32::from_rgb(79, 70, 229),
            ind_vwap: Color32::from_rgb(109, 40, 217),
            row_selected_tint: Color32::from_rgba_unmultiplied(22, 22, 20, 10),
            row_hover_tint: Color32::from_rgba_unmultiplied(22, 22, 20, 6),
            crosshair: Color32::from_rgba_unmultiplied(22, 22, 20, 56),
            cta_bg: Color32::from_rgb(22, 22, 20),
            cta_fg: Color32::from_rgb(255, 255, 255),
            cta_border: Color32::from_rgb(22, 22, 20),
        }
    }

    pub fn from_light(is_light: bool) -> Self {
        if is_light { Self::light() } else { Self::dark() }
    }

    #[inline]
    pub fn tint(col: Color32, alpha: u8) -> Color32 {
        Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha)
    }

    /// Pick high-contrast ink for a filled badge or button background.
    #[inline]
    pub fn contrast_on(bg: Color32) -> Color32 {
        let luma = 0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32;
        if luma > 140.0 {
            Color32::from_rgb(15, 16, 19)
        } else {
            Color32::WHITE
        }
    }

    pub fn surface_card(&self) -> Frame {
        Frame::NONE
            .fill(self.bg_card)
            .stroke(Stroke::new(1.0, self.border))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(10))
    }
}

pub fn apply_theme(ctx: &Context, light_mode: bool) {
    let pal = ThemePalette::from_light(light_mode);
    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(6.0, 6.0);
    style.spacing.button_padding = Vec2::new(9.0, 5.0);
    style.spacing.window_margin = Margin::same(12);
    style.spacing.scroll.bar_width = 6.0;

    let mut v = if light_mode {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };

    v.panel_fill = pal.bg_canvas;
    v.window_fill = pal.bg_panel;
    v.extreme_bg_color = pal.bg_sub;
    v.faint_bg_color = if light_mode {
        Color32::from_rgba_unmultiplied(22, 22, 20, 6)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 5)
    };
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.window_stroke = Stroke::new(1.0, pal.border_strong);

    let r4 = CornerRadius::same(4);

    v.widgets.noninteractive.bg_fill = pal.bg_card;
    v.widgets.noninteractive.weak_bg_fill = pal.bg_sub;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.text_secondary);
    v.widgets.noninteractive.corner_radius = r4;

    v.widgets.inactive.bg_fill = pal.bg_chip;
    v.widgets.inactive.weak_bg_fill = pal.bg_sub;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.inactive.corner_radius = r4;

    let (hov_bg, hov_weak, act_bg) = if light_mode {
        (
            Color32::from_rgb(231, 228, 219),
            Color32::from_rgb(236, 233, 225),
            Color32::from_rgb(223, 220, 210),
        )
    } else {
        (
            Color32::from_rgb(38, 42, 51),
            Color32::from_rgb(32, 36, 44),
            Color32::from_rgb(44, 49, 60),
        )
    };

    v.widgets.hovered.bg_fill = hov_bg;
    v.widgets.hovered.weak_bg_fill = hov_weak;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal.border_strong);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.hovered.corner_radius = r4;
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.bg_fill = act_bg;
    v.widgets.active.weak_bg_fill = act_bg;
    v.widgets.active.bg_stroke = Stroke::new(1.0, pal.border_strong);
    v.widgets.active.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.active.corner_radius = r4;
    v.widgets.active.expansion = 0.0;

    v.widgets.open.bg_fill = hov_bg;
    v.widgets.open.weak_bg_fill = hov_weak;
    v.widgets.open.bg_stroke = Stroke::new(1.0, pal.border_strong);
    v.widgets.open.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.open.corner_radius = r4;

    v.selection.bg_fill = ThemePalette::tint(pal.blue, if light_mode { 28 } else { 40 });
    v.selection.stroke = Stroke::new(1.0, pal.blue);

    style.visuals = v;
    ctx.set_global_style(style);
}
