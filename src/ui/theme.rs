use egui::{Color32, Context, CornerRadius, Frame, Margin, Stroke, Vec2};

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub light: bool,
    pub bg_canvas: Color32,
    pub bg_panel: Color32,
    pub bg_card: Color32,
    pub bg_sub: Color32,
    pub bg_chip: Color32,
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
    pub cyan: Color32,
    pub amber: Color32,
    pub grid_line: Color32,
    pub px_col_bg: Color32,
    pub ladder_hdr_bg: Color32,
    pub tooltip_bg: Color32,
    pub tooltip_fg: Color32,
}

impl ThemePalette {
    pub fn dark() -> Self {
        Self {
            light: false,
            bg_canvas: Color32::from_rgb(9, 12, 18),
            bg_panel: Color32::from_rgb(13, 17, 27),
            bg_card: Color32::from_rgb(17, 22, 34),
            bg_sub: Color32::from_rgb(13, 18, 29),
            bg_chip: Color32::from_rgb(24, 32, 49),
            bg_pill: Color32::from_rgb(19, 25, 39),
            border: Color32::from_rgba_unmultiplied(148, 163, 184, 24),
            border_strong: Color32::from_rgba_unmultiplied(148, 163, 184, 42),
            text_primary: Color32::from_rgb(248, 250, 252),
            text_secondary: Color32::from_rgb(203, 213, 225),
            text_muted: Color32::from_rgb(138, 154, 178),
            green: Color32::from_rgb(0, 211, 149),
            green_text: Color32::from_rgb(167, 243, 208),
            red: Color32::from_rgb(255, 77, 106),
            red_text: Color32::from_rgb(254, 205, 211),
            blue: Color32::from_rgb(46, 124, 246),
            cyan: Color32::from_rgb(56, 189, 248),
            amber: Color32::from_rgb(251, 191, 36),
            grid_line: Color32::from_rgba_unmultiplied(148, 163, 184, 16),
            px_col_bg: Color32::from_rgb(18, 24, 36),
            ladder_hdr_bg: Color32::from_rgb(20, 27, 40),
            tooltip_bg: Color32::from_rgba_unmultiplied(15, 23, 42, 242),
            tooltip_fg: Color32::from_rgb(241, 245, 249),
        }
    }

    pub fn light() -> Self {
        Self {
            light: true,
            bg_canvas: Color32::from_rgb(241, 245, 249),
            bg_panel: Color32::from_rgb(255, 255, 255),
            bg_card: Color32::from_rgb(255, 255, 255),
            bg_sub: Color32::from_rgb(248, 250, 252),
            bg_chip: Color32::from_rgb(241, 245, 249),
            bg_pill: Color32::from_rgb(248, 250, 252),
            border: Color32::from_rgb(226, 232, 240),
            border_strong: Color32::from_rgb(203, 213, 225),
            text_primary: Color32::from_rgb(15, 23, 42),
            text_secondary: Color32::from_rgb(51, 65, 85),
            text_muted: Color32::from_rgb(100, 116, 139),
            green: Color32::from_rgb(5, 150, 105),
            green_text: Color32::from_rgb(4, 120, 87),
            red: Color32::from_rgb(225, 29, 72),
            red_text: Color32::from_rgb(190, 18, 60),
            blue: Color32::from_rgb(37, 99, 235),
            cyan: Color32::from_rgb(2, 132, 199),
            amber: Color32::from_rgb(217, 119, 6),
            grid_line: Color32::from_rgba_unmultiplied(15, 23, 42, 16),
            px_col_bg: Color32::from_rgb(241, 245, 249),
            ladder_hdr_bg: Color32::from_rgb(248, 250, 252),
            tooltip_bg: Color32::from_rgba_unmultiplied(15, 23, 42, 240),
            tooltip_fg: Color32::from_rgb(248, 250, 252),
        }
    }

    pub fn from_light(is_light: bool) -> Self {
        if is_light { Self::light() } else { Self::dark() }
    }

    pub fn surface_card(&self) -> Frame {
        Frame::NONE
            .fill(self.bg_card)
            .stroke(Stroke::new(1.0, self.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::same(14))
    }

    pub fn pill_frame(&self) -> Frame {
        Frame::NONE
            .fill(self.bg_pill)
            .stroke(Stroke::new(1.0, self.border_strong))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(12, 6))
    }
}

pub fn apply_theme(ctx: &Context, light_mode: bool) {
    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(11.0, 6.5);
    style.spacing.window_margin = Margin::same(16);
    style.spacing.scroll.bar_width = 6.0;

    if light_mode {
        let mut v = egui::Visuals::light();
        v.panel_fill = Color32::from_rgb(241, 245, 249);
        v.window_fill = Color32::from_rgb(255, 255, 255);
        v.extreme_bg_color = Color32::from_rgb(248, 250, 252);
        v.window_corner_radius = CornerRadius::same(14);
        v.window_stroke = Stroke::new(1.0, Color32::from_rgb(203, 213, 225));

        v.widgets.noninteractive.bg_fill = Color32::from_rgb(255, 255, 255);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(226, 232, 240));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(30, 41, 59));
        v.widgets.noninteractive.corner_radius = CornerRadius::same(9);

        v.widgets.inactive.bg_fill = Color32::from_rgb(241, 245, 249);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(248, 250, 252);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(203, 213, 225));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(15, 23, 42));
        v.widgets.inactive.corner_radius = CornerRadius::same(9);

        v.widgets.hovered.bg_fill = Color32::from_rgb(226, 232, 240);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(235, 240, 247);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(37, 99, 235));
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::from_rgb(15, 23, 42));
        v.widgets.hovered.corner_radius = CornerRadius::same(9);

        v.widgets.active.bg_fill = Color32::from_rgb(219, 234, 254);
        v.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(37, 99, 235));
        v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::from_rgb(15, 23, 42));
        v.widgets.active.corner_radius = CornerRadius::same(9);

        v.selection.bg_fill = Color32::from_rgba_unmultiplied(37, 99, 235, 34);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(37, 99, 235));
        style.visuals = v;
    } else {
        let mut v = egui::Visuals::dark();
        v.panel_fill = Color32::from_rgb(9, 12, 18);
        v.window_fill = Color32::from_rgb(15, 20, 31);
        v.extreme_bg_color = Color32::from_rgb(10, 14, 23);
        v.window_corner_radius = CornerRadius::same(14);
        v.window_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(148, 163, 184, 38));

        v.widgets.noninteractive.bg_fill = Color32::from_rgb(17, 22, 34);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(148, 163, 184, 22));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(203, 213, 225));
        v.widgets.noninteractive.corner_radius = CornerRadius::same(9);

        v.widgets.inactive.bg_fill = Color32::from_rgb(22, 29, 44);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(19, 25, 39);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(148, 163, 184, 24));
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(230, 236, 245));
        v.widgets.inactive.corner_radius = CornerRadius::same(9);

        v.widgets.hovered.bg_fill = Color32::from_rgb(30, 40, 60);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(27, 36, 54);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(56, 189, 248));
        v.widgets.hovered.corner_radius = CornerRadius::same(9);

        v.widgets.active.bg_fill = Color32::from_rgb(37, 50, 76);
        v.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(46, 124, 246));
        v.widgets.active.corner_radius = CornerRadius::same(9);

        v.selection.bg_fill = Color32::from_rgba_unmultiplied(46, 124, 246, 48);
        v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(56, 189, 248));
        style.visuals = v;
    }

    ctx.set_global_style(style);
}
