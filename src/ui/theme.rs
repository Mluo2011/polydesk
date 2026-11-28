use egui::{Color32, Context, CornerRadius, FontFamily, FontId, Frame, Margin, Stroke, Vec2};

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
    /// Carbon Obsidian dark palette — crisp neutral obsidian/zinc surfaces with calibrated semantic accents.
    pub fn dark() -> Self {
        Self {
            light: false,
            bg_canvas: Color32::from_rgb(9, 9, 11),
            bg_panel: Color32::from_rgb(13, 14, 18),
            bg_card: Color32::from_rgb(18, 20, 26),
            bg_sub: Color32::from_rgb(23, 26, 34),
            bg_chip: Color32::from_rgb(30, 34, 45),
            bg_pill: Color32::from_rgb(23, 26, 34),
            border: Color32::from_rgb(34, 38, 49),
            border_strong: Color32::from_rgb(52, 58, 74),
            text_primary: Color32::from_rgb(244, 244, 246),
            text_secondary: Color32::from_rgb(156, 163, 175),
            text_muted: Color32::from_rgb(100, 108, 126),
            green: Color32::from_rgb(16, 185, 129),
            green_text: Color32::from_rgb(52, 211, 153),
            red: Color32::from_rgb(244, 63, 94),
            red_text: Color32::from_rgb(251, 113, 133),
            blue: Color32::from_rgb(59, 130, 246),
            cyan: Color32::from_rgb(56, 189, 248),
            amber: Color32::from_rgb(245, 158, 11),
            grid_line: Color32::from_rgb(24, 27, 36),
            px_col_bg: Color32::from_rgb(14, 16, 21),
            ladder_hdr_bg: Color32::from_rgb(13, 14, 18),
            tooltip_bg: Color32::from_rgba_unmultiplied(17, 19, 25, 244),
            tooltip_fg: Color32::from_rgb(244, 244, 246),
            ind_ema: Color32::from_rgb(96, 165, 250),
            ind_sma: Color32::from_rgb(245, 176, 65),
            ind_bb: Color32::from_rgb(129, 140, 248),
            ind_vwap: Color32::from_rgb(167, 139, 250),
            row_selected_tint: Color32::from_rgba_unmultiplied(59, 130, 246, 22),
            row_hover_tint: Color32::from_rgba_unmultiplied(255, 255, 255, 8),
            crosshair: Color32::from_rgba_unmultiplied(244, 244, 246, 64),
            cta_bg: Color32::from_rgb(244, 244, 246),
            cta_fg: Color32::from_rgb(9, 9, 11),
            cta_border: Color32::from_rgb(244, 244, 246),
        }
    }

    /// Editorial Swiss light palette.
    pub fn light() -> Self {
        Self {
            light: true,
            bg_canvas: Color32::from_rgb(244, 245, 247),
            bg_panel: Color32::from_rgb(255, 255, 255),
            bg_card: Color32::from_rgb(255, 255, 255),
            bg_sub: Color32::from_rgb(241, 243, 246),
            bg_chip: Color32::from_rgb(230, 233, 239),
            bg_pill: Color32::from_rgb(241, 243, 246),
            border: Color32::from_rgb(226, 229, 236),
            border_strong: Color32::from_rgb(203, 210, 223),
            text_primary: Color32::from_rgb(15, 23, 42),
            text_secondary: Color32::from_rgb(71, 85, 105),
            text_muted: Color32::from_rgb(90, 101, 119),
            green: Color32::from_rgb(5, 150, 105),
            green_text: Color32::from_rgb(4, 120, 87),
            red: Color32::from_rgb(225, 29, 72),
            red_text: Color32::from_rgb(190, 18, 60),
            blue: Color32::from_rgb(37, 99, 235),
            cyan: Color32::from_rgb(2, 132, 199),
            amber: Color32::from_rgb(180, 83, 9),
            grid_line: Color32::from_rgb(238, 240, 244),
            px_col_bg: Color32::from_rgb(245, 247, 250),
            ladder_hdr_bg: Color32::from_rgb(247, 249, 252),
            tooltip_bg: Color32::from_rgba_unmultiplied(15, 23, 42, 244),
            tooltip_fg: Color32::from_rgb(248, 250, 252),
            ind_ema: Color32::from_rgb(37, 99, 235),
            ind_sma: Color32::from_rgb(180, 83, 9),
            ind_bb: Color32::from_rgb(79, 70, 229),
            ind_vwap: Color32::from_rgb(109, 40, 217),
            row_selected_tint: Color32::from_rgba_unmultiplied(37, 99, 235, 16),
            row_hover_tint: Color32::from_rgba_unmultiplied(15, 23, 42, 7),
            crosshair: Color32::from_rgba_unmultiplied(15, 23, 42, 64),
            cta_bg: Color32::from_rgb(15, 23, 42),
            cta_fg: Color32::WHITE,
            cta_border: Color32::from_rgb(15, 23, 42),
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
            Color32::from_rgb(8, 11, 16)
        } else {
            Color32::WHITE
        }
    }

    #[inline]
    pub fn semibold(size: f32) -> FontId {
        FontId::new(size, FontFamily::Name("semibold".into()))
    }

    pub fn surface_card(&self) -> Frame {
        Frame::NONE
            .fill(self.bg_card)
            .stroke(Stroke::new(1.0, self.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(12))
    }

    pub fn pill_container(&self) -> Frame {
        Frame::NONE
            .fill(self.bg_sub)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(3))
    }

    /// Crisp vector-painted PolyDirect brand mark (two opposing probability bars inside a diamond badge).
    pub fn draw_brand_mark(ui: &mut egui::Ui, pal: ThemePalette, ws_connected: bool) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(22.0, 22.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(6), pal.bg_chip);
        p.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, pal.border_strong),
            egui::StrokeKind::Inside,
        );
        let c = rect.center();
        // Upward emerald leg + downward crimson/azure counterweight bar
        p.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(c.x - 5.5, c.y - 3.5),
                egui::pos2(c.x - 1.5, c.y + 5.5),
            ),
            CornerRadius::same(1),
            pal.green,
        );
        p.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(c.x + 1.5, c.y - 5.5),
                egui::pos2(c.x + 5.5, c.y + 3.5),
            ),
            CornerRadius::same(1),
            if ws_connected { pal.blue } else { pal.red },
        );
    }
}

/// Elevated segmented pill button inside a recessed `pill_container`.
pub fn seg_item(ui: &mut egui::Ui, pal: ThemePalette, active: bool, label: &str) -> egui::Response {
    let font = if active {
        ThemePalette::semibold(11.5)
    } else {
        FontId::proportional(11.5)
    };
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, Color32::PLACEHOLDER);
    let desired = Vec2::new((galley.size().x + 18.0).max(34.0), 23.0);
    let (rect, resp) = ui.allocate_exact_size(desired, egui::Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let fill = if active {
        if pal.light { pal.bg_panel } else { pal.bg_chip }
    } else if resp.hovered() {
        ThemePalette::tint(pal.bg_chip, if pal.light { 160 } else { 150 })
    } else {
        Color32::TRANSPARENT
    };
    let text_col = if active {
        pal.text_primary
    } else if resp.hovered() {
        pal.text_primary
    } else {
        pal.text_secondary
    };
    let p = ui.painter();
    if fill != Color32::TRANSPARENT {
        p.rect_filled(rect, CornerRadius::same(6), fill);
    }
    if active {
        p.rect_stroke(
            rect,
            CornerRadius::same(6),
            Stroke::new(1.0, pal.border_strong),
            egui::StrokeKind::Inside,
        );
    }
    p.galley(
        egui::pos2(
            rect.center().x - galley.size().x * 0.5,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        text_col,
    );
    resp
}

/// Calm borderless ghost action button (lifts softly on hover).
pub fn ghost_button(
    ui: &mut egui::Ui,
    pal: ThemePalette,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    let resp = ui.add(
        egui::Button::new(text)
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(6)),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let _ = pal;
    resp
}

pub fn apply_theme(ctx: &Context, light_mode: bool) {
    let pal = ThemePalette::from_light(light_mode);
    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(6.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
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
        Color32::from_rgba_unmultiplied(15, 23, 42, 6)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 5)
    };
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0, pal.border_strong);

    let r6 = CornerRadius::same(6);

    v.widgets.noninteractive.bg_fill = pal.bg_card;
    v.widgets.noninteractive.weak_bg_fill = pal.bg_sub;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, pal.text_secondary);
    v.widgets.noninteractive.corner_radius = r6;

    v.widgets.inactive.bg_fill = pal.bg_chip;
    v.widgets.inactive.weak_bg_fill = pal.bg_sub;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.inactive.corner_radius = r6;

    let (hov_bg, hov_weak, act_bg) = if light_mode {
        (
            Color32::from_rgb(224, 228, 236),
            Color32::from_rgb(232, 236, 243),
            Color32::from_rgb(212, 218, 229),
        )
    } else {
        (
            pal.bg_chip,
            Color32::from_rgb(34, 39, 52),
            Color32::from_rgb(40, 46, 61),
        )
    };

    v.widgets.hovered.bg_fill = hov_bg;
    v.widgets.hovered.weak_bg_fill = hov_weak;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.hovered.corner_radius = r6;
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.bg_fill = act_bg;
    v.widgets.active.weak_bg_fill = act_bg;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.active.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.active.corner_radius = r6;
    v.widgets.active.expansion = 0.0;

    v.widgets.open.bg_fill = hov_bg;
    v.widgets.open.weak_bg_fill = hov_weak;
    v.widgets.open.bg_stroke = Stroke::new(1.0, pal.border_strong);
    v.widgets.open.fg_stroke = Stroke::new(1.0, pal.text_primary);
    v.widgets.open.corner_radius = r6;

    v.selection.bg_fill = ThemePalette::tint(pal.blue, if light_mode { 34 } else { 44 });
    v.selection.stroke = Stroke::new(1.2, pal.blue);

    style.visuals = v;
    ctx.set_global_style(style);
}
