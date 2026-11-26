use std::collections::VecDeque;

use egui::{Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use polydesk::automation::{AutomationRule, RuleKind};
use polydesk::i18n::{Lang, tr};
use polydesk::market::PricePoint;
use polydesk::odds::{OddsFormat, format_odds};
use polydesk::oracle_feed::{OracleSample, compute_twap_60s};
use polydesk::paper::PaperPosition;

use super::theme::ThemePalette;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartMode {
    Line,
    Candles,
    Oracle,
}

pub struct OracleChartData<'a> {
    pub symbol: &'a str,
    pub samples: &'a [(i64, f64)],
    pub twap_60s: Option<f64>,
    pub price_to_beat: Option<f64>,
    pub binance_mid: Option<f64>,
}

pub fn format_oracle_usd(p: f64) -> String {
    let abs = p.abs();
    if abs >= 100.0 {
        format!("${p:.2}")
    } else if abs >= 1.0 {
        format!("${p:.3}")
    } else {
        format!("${p:.4}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndicatorConfig {
    pub ema9: bool,
    pub sma20: bool,
    pub bollinger: bool,
    pub vwap: bool,
    pub rsi14: bool,
}

struct Candle {
    t: i64,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChartCacheKey {
    asset_id: String,
    interval: String,
    len: usize,
    first_t: i64,
    last_t: i64,
    last_p_bits: u64,
    mode: ChartMode,
    indicators: IndicatorConfig,
}

#[derive(Default)]
pub struct ChartCache {
    key: Option<ChartCacheKey>,
    min_t: i64,
    max_t: i64,
    base_min_p: f64,
    base_max_p: f64,
    ema9: Option<Vec<f64>>,
    sma20: Option<Vec<Option<f64>>>,
    bb: Option<Vec<(Option<f64>, Option<f64>)>>,
    vwap: Option<Vec<f64>>,
    rsi14: Option<Vec<Option<f64>>>,
    candles: Vec<Candle>,
}

impl ChartCache {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Recomputes cached series only when the underlying data, interval, mode, or indicator toggles change.
    /// Returns `true` if a recomputation occurred, `false` on cache hit.
    pub fn ensure(
        &mut self,
        asset_id: &str,
        interval: &str,
        points: &[PricePoint],
        mode: ChartMode,
        indicators: IndicatorConfig,
    ) -> bool {
        let first_t = points.first().map(|p| p.t).unwrap_or(0);
        let last_t = points.last().map(|p| p.t).unwrap_or(0);
        let last_p_bits = points.last().map(|p| p.p.to_bits()).unwrap_or(0);

        if let Some(ref existing) = self.key
            && existing.asset_id == asset_id
            && existing.interval == interval
            && existing.len == points.len()
            && existing.first_t == first_t
            && existing.last_t == last_t
            && existing.last_p_bits == last_p_bits
            && existing.mode == mode
            && existing.indicators == indicators
        {
            return false;
        }

        let next_key = ChartCacheKey {
            asset_id: asset_id.to_string(),
            interval: interval.to_string(),
            len: points.len(),
            first_t,
            last_t,
            last_p_bits,
            mode,
            indicators,
        };

        if points.is_empty() {
            self.min_t = 0;
            self.max_t = 1;
            self.base_min_p = 0.0;
            self.base_max_p = 1.0;
            self.ema9 = None;
            self.sma20 = None;
            self.bb = None;
            self.vwap = None;
            self.rsi14 = None;
            self.candles.clear();
            self.key = Some(next_key);
            return true;
        }

        let mut min_t = i64::MAX;
        let mut max_t = i64::MIN;
        let mut min_p = f64::INFINITY;
        let mut max_p = f64::NEG_INFINITY;
        let mut prices = Vec::with_capacity(points.len());
        for pt in points {
            min_t = min_t.min(pt.t);
            max_t = max_t.max(pt.t);
            min_p = min_p.min(pt.p);
            max_p = max_p.max(pt.p);
            prices.push(pt.p);
        }
        if max_t <= min_t {
            max_t = min_t + 1;
        }

        self.ema9 = indicators.ema9.then(|| compute_ema(&prices, 9));
        self.sma20 = indicators.sma20.then(|| compute_sma(&prices, 20));
        self.bb = indicators.bollinger.then(|| compute_bollinger(&prices, 20, 2.0));
        self.vwap = indicators.vwap.then(|| compute_running_mean(&prices));
        self.rsi14 = indicators.rsi14.then(|| compute_rsi(&prices, 14));
        self.candles = if mode == ChartMode::Candles {
            build_candles(points)
        } else {
            Vec::new()
        };

        if let Some(ref bb) = self.bb {
            for &(lo, hi) in bb {
                if let (Some(l), Some(h)) = (lo, hi) {
                    min_p = min_p.min(l);
                    max_p = max_p.max(h);
                }
            }
        }

        self.min_t = min_t;
        self.max_t = max_t;
        self.base_min_p = min_p;
        self.base_max_p = max_p;
        self.key = Some(next_key);
        true
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_price_chart(
    ui: &mut Ui,
    lang: Lang,
    pal: ThemePalette,
    asset_id: &str,
    interval: &str,
    points: &[PricePoint],
    history_loading: bool,
    history_error: Option<&str>,
    mode: ChartMode,
    indicators: IndicatorConfig,
    odds_fmt: OddsFormat,
    tick_size: f64,
    top_of_book: Option<(Option<f64>, Option<f64>)>,
    position: Option<&PaperPosition>,
    all_rules: &[AutomationRule],
    cache: &mut ChartCache,
    height: f32,
) {
    let desired = vec2(ui.available_width().max(120.0), height.max(160.0));
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, CornerRadius::same(6), pal.bg_card);
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    let pad_l = 12.0;
    let pad_r = 66.0;
    let pad_t = 12.0;
    let pad_b = 22.0;
    let rsi_h = if indicators.rsi14 && height >= 210.0 { 56.0 } else { 0.0 };
    let rsi_gap = if rsi_h > 0.0 { 8.0 } else { 0.0 };

    let plot = Rect::from_min_max(
        pos2(rect.left() + pad_l, rect.top() + pad_t),
        pos2(
            (rect.right() - pad_r).max(rect.left() + pad_l + 20.0),
            (rect.bottom() - pad_b - rsi_h - rsi_gap).max(rect.top() + pad_t + 30.0),
        ),
    );

    if points.len() < 2 {
        let (msg, col) = if let Some(err) = history_error {
            (err, pal.red_text)
        } else if history_loading {
            (
                tr(lang, "Loading live Polymarket chart...", "正在加载 Polymarket 实时走势..."),
                pal.text_muted,
            )
        } else {
            (
                tr(lang, "No chart history available", "暂无历史走势数据"),
                pal.text_muted,
            )
        };
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            msg,
            FontId::proportional(13.0),
            col,
        );
        return;
    }

    cache.ensure(asset_id, interval, points, mode, indicators);

    let min_t = cache.min_t;
    let max_t = cache.max_t;
    let span_t = (max_t - min_t).max(1) as f64;

    let mut min_p = cache.base_min_p;
    let mut max_p = cache.base_max_p;
    if let Some(pos) = position
        && pos.shares > 1e-6
    {
        min_p = min_p.min(pos.avg_entry);
        max_p = max_p.max(pos.avg_entry);
    }
    for r in all_rules {
        if r.active && r.asset_id == asset_id {
            min_p = min_p.min(r.trigger_price);
            max_p = max_p.max(r.trigger_price);
        }
    }

    let span_pad = ((max_p - min_p) * 0.14).max(0.02);
    min_p = (min_p - span_pad).max(0.001);
    max_p = (max_p + span_pad).min(0.999);
    let range_p = (max_p - min_p).max(0.005);

    let y_of = |p: f64| -> f32 {
        plot.top() + (((max_p - p) / range_p) as f32) * plot.height()
    };
    let x_of_t = |t: i64| -> f32 {
        let frac = ((t - min_t) as f64 / span_t).clamp(0.0, 1.0) as f32;
        plot.left() + frac * plot.width()
    };

    // Subtle hairline horizontal gridlines & clean right Y-axis
    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let y = plot.top() + (frac as f32) * plot.height();
        let p = max_p - frac * range_p;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0, pal.grid_line),
        );
        painter.text(
            pos2(plot.right() + 8.0, y),
            egui::Align2::LEFT_CENTER,
            format_odds(Some(p), odds_fmt, tick_size),
            FontId::monospace(10.0),
            pal.text_muted,
        );
    }

    let _ = top_of_book;

    // Bollinger Bands (20, 2σ) shaded envelope rendered as a single GPU Mesh strip
    if let Some(ref bb) = cache.bb {
        let mut upper_pts = Vec::with_capacity(bb.len());
        let mut lower_pts = Vec::with_capacity(bb.len());
        let mut bb_mesh = Mesh::default();
        bb_mesh.vertices.reserve(bb.len() * 2);
        bb_mesh.indices.reserve(bb.len().saturating_sub(1) * 6);
        let bb_fill = ThemePalette::tint(pal.ind_bb, if pal.light { 16 } else { 18 });
        let bb_stroke = Stroke::new(1.0, ThemePalette::tint(pal.ind_bb, 115));

        for (i, &(lo, hi)) in bb.iter().enumerate() {
            if let (Some(l), Some(h)) = (lo, hi) {
                let x = x_of_t(points[i].t);
                let pu = pos2(x, y_of(h));
                let pl = pos2(x, y_of(l));
                let u_idx = bb_mesh.vertices.len() as u32;
                let l_idx = u_idx + 1;
                bb_mesh.colored_vertex(pu, bb_fill);
                bb_mesh.colored_vertex(pl, bb_fill);
                if u_idx >= 2 {
                    let prev_u = u_idx - 2;
                    let prev_l = u_idx - 1;
                    bb_mesh.add_triangle(prev_u, u_idx, l_idx);
                    bb_mesh.add_triangle(prev_u, l_idx, prev_l);
                }
                upper_pts.push(pu);
                lower_pts.push(pl);
            }
        }
        if !bb_mesh.indices.is_empty() {
            painter.add(Shape::mesh(bb_mesh));
        }
        if upper_pts.len() >= 2 {
            painter.add(Shape::line(upper_pts, bb_stroke));
            painter.add(Shape::line(lower_pts, bb_stroke));
        }
    }

    let up_col = pal.green;
    let dn_col = pal.red;
    let first_p = points[0].p;
    let last_p = points.last().map(|pt| pt.p).unwrap_or(0.5);
    let is_up = last_p >= first_p;
    let trend_col = if is_up { up_col } else { dn_col };

    match mode {
        ChartMode::Line | ChartMode::Oracle => {
            let mut poly = Vec::with_capacity(points.len());
            let mut mesh = Mesh::default();
            mesh.vertices.reserve(points.len() * 2);
            mesh.indices.reserve(points.len().saturating_sub(1) * 6);

            let top_alpha = if pal.light { 22 } else { 18 };
            let top_fill = ThemePalette::tint(trend_col, top_alpha);
            let bot_fill = ThemePalette::tint(trend_col, 0);

            for (i, pt) in points.iter().enumerate() {
                let scr = pos2(x_of_t(pt.t), y_of(pt.p));
                poly.push(scr);
                let top_idx = (i * 2) as u32;
                let bot_idx = top_idx + 1;
                mesh.colored_vertex(scr, top_fill);
                mesh.colored_vertex(pos2(scr.x, plot.bottom()), bot_fill);
                if i > 0 {
                    let prev_top = top_idx - 2;
                    let prev_bot = top_idx - 1;
                    mesh.add_triangle(prev_top, top_idx, bot_idx);
                    mesh.add_triangle(prev_top, bot_idx, prev_bot);
                }
            }

            if poly.len() >= 2 {
                let last_pt = poly.last().copied();
                painter.add(Shape::mesh(mesh));
                painter.add(Shape::line(poly, Stroke::new(1.75, trend_col)));
                if let Some(pt) = last_pt {
                    painter.circle_filled(pt, 3.0, trend_col);
                    painter.circle_stroke(pt, 3.0, Stroke::new(1.2, pal.bg_card));
                }
            }
        }
        ChartMode::Candles => {
            let n = cache.candles.len().max(1);
            let bar_w = ((plot.width() / n as f32) * 0.62).clamp(3.0, 14.0);
            for c in &cache.candles {
                let x = x_of_t(c.t).clamp(plot.left() + bar_w * 0.5, plot.right() - bar_w * 0.5);
                let col = if c.c >= c.o { up_col } else { dn_col };
                painter.line_segment([pos2(x, y_of(c.h)), pos2(x, y_of(c.l))], Stroke::new(1.0, col));
                let top = y_of(c.o.max(c.c));
                let bot = y_of(c.o.min(c.c)).max(top + 1.5);
                painter.rect_filled(
                    Rect::from_min_max(pos2(x - bar_w * 0.5, top), pos2(x + bar_w * 0.5, bot)),
                    1.0,
                    col,
                );
            }
        }
    }

    // Optional indicator overlay curves (OFF by default)
    if let Some(ref ema) = cache.ema9 {
        let pts: Vec<Pos2> = ema
            .iter()
            .enumerate()
            .map(|(i, &v)| pos2(x_of_t(points[i].t), y_of(v)))
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.25, pal.ind_ema)));
    }
    if indicators.sma20
        && let Some(ref sma) = cache.sma20
    {
        let pts: Vec<Pos2> = sma
            .iter()
            .enumerate()
            .filter_map(|(i, &o)| o.map(|v| pos2(x_of_t(points[i].t), y_of(v))))
            .collect();
        if pts.len() >= 2 {
            painter.add(Shape::line(pts, Stroke::new(1.25, pal.ind_sma)));
        }
    }
    if let Some(ref vw) = cache.vwap {
        let pts: Vec<Pos2> = vw
            .iter()
            .enumerate()
            .map(|(i, &v)| pos2(x_of_t(points[i].t), y_of(v)))
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.2, pal.ind_vwap)));
    }

    // Indicator legend top-left (only shown if user toggled at least one indicator on)
    let mut legend = Vec::new();
    if let Some(ref e) = cache.ema9
        && let Some(&v) = e.last()
    {
        legend.push((format!("EMA9 {:.1}¢", v * 100.0), pal.ind_ema));
    }
    if indicators.sma20
        && let Some(ref s) = cache.sma20
        && let Some(Some(v)) = s.last()
    {
        legend.push((format!("SMA20 {:.1}¢", v * 100.0), pal.ind_sma));
    }
    if indicators.bollinger {
        legend.push(("BB(20,2)".into(), pal.ind_bb));
    }
    if let Some(ref vw) = cache.vwap
        && let Some(&v) = vw.last()
    {
        legend.push((format!("VWAP {:.1}¢", v * 100.0), pal.ind_vwap));
    }
    let mut lx = plot.left() + 4.0;
    for (txt, col) in legend {
        painter.text(
            pos2(lx, plot.top() + 2.0),
            egui::Align2::LEFT_TOP,
            &txt,
            FontId::monospace(9.5),
            col,
        );
        lx += (txt.len() as f32) * 6.0 + 10.0;
    }

    // Dashed horizontal lines for Avg Entry / SL / TP / Trail + crisp right-axis live price tag
    let draw_dashed_tag = |p: f64, col: Color32, line_alpha: u8, tag: String| {
        let y = y_of(p);
        if y < plot.top() - 4.0 || y > plot.bottom() + 4.0 {
            return;
        }
        let dash_col = ThemePalette::tint(col, line_alpha);
        let mut x = plot.left();
        while x < plot.right() {
            let x_end = (x + 5.0).min(plot.right());
            painter.line_segment([pos2(x, y), pos2(x_end, y)], Stroke::new(1.0, dash_col));
            x += 9.0;
        }
        let badge = Rect::from_min_max(
            pos2(plot.right() + 4.0, y - 9.0),
            pos2(rect.right() - 4.0, y + 9.0),
        );
        painter.rect_filled(badge, CornerRadius::same(4), col);
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            tag,
            FontId::monospace(9.5),
            ThemePalette::contrast_on(col),
        );
    };

    if let Some(pos) = position
        && pos.shares > 1e-6
    {
        draw_dashed_tag(
            pos.avg_entry,
            pal.blue,
            150,
            format!("AVG {:.1}", pos.avg_entry * 100.0),
        );
    }
    for r in all_rules {
        if !r.active || r.asset_id != asset_id {
            continue;
        }
        match r.kind {
            RuleKind::StopLoss => draw_dashed_tag(
                r.trigger_price,
                dn_col,
                150,
                format!("SL {:.1}", r.trigger_price * 100.0),
            ),
            RuleKind::TakeProfit => draw_dashed_tag(
                r.trigger_price,
                up_col,
                150,
                format!("TP {:.1}", r.trigger_price * 100.0),
            ),
            RuleKind::TrailingStop => draw_dashed_tag(
                r.trigger_price,
                pal.amber,
                150,
                format!("TRL {:.1}", r.trigger_price * 100.0),
            ),
            RuleKind::DripFeed => {}
        }
    }
    draw_dashed_tag(last_p, trend_col, 55, format_odds(Some(last_p), odds_fmt, tick_size));

    // RSI(14) Sub-Pane (only when toggled on)
    if rsi_h > 0.0
        && let Some(ref rsi) = cache.rsi14
    {
        let r_rect = Rect::from_min_max(
            pos2(plot.left(), plot.bottom() + rsi_gap),
            pos2(plot.right(), rect.bottom() - pad_b),
        );
        painter.rect_filled(r_rect, CornerRadius::same(4), pal.bg_sub);
        painter.rect_stroke(
            r_rect,
            CornerRadius::same(4),
            Stroke::new(1.0, pal.border),
            egui::StrokeKind::Inside,
        );

        let y_rsi = |v: f64| r_rect.top() + (((100.0 - v.clamp(0.0, 100.0)) / 100.0) as f32) * r_rect.height();
        let y70 = y_rsi(70.0);
        let y30 = y_rsi(30.0);
        painter.line_segment(
            [pos2(r_rect.left(), y70), pos2(r_rect.right(), y70)],
            Stroke::new(0.8, ThemePalette::tint(pal.red, 70)),
        );
        painter.line_segment(
            [pos2(r_rect.left(), y30), pos2(r_rect.right(), y30)],
            Stroke::new(0.8, ThemePalette::tint(pal.green, 70)),
        );
        painter.text(
            pos2(r_rect.right() + 8.0, y70),
            egui::Align2::LEFT_CENTER,
            "70",
            FontId::monospace(9.0),
            pal.red,
        );
        painter.text(
            pos2(r_rect.right() + 8.0, y30),
            egui::Align2::LEFT_CENTER,
            "30",
            FontId::monospace(9.0),
            pal.green,
        );

        let r_pts: Vec<Pos2> = rsi
            .iter()
            .enumerate()
            .filter_map(|(i, &o)| o.map(|v| pos2(x_of_t(points[i].t), y_rsi(v))))
            .collect();
        if r_pts.len() >= 2 {
            painter.add(Shape::line(r_pts, Stroke::new(1.3, pal.amber)));
        }
        if let Some(Some(last_r)) = rsi.last() {
            let rc = if *last_r >= 70.0 {
                dn_col
            } else if *last_r <= 30.0 {
                up_col
            } else {
                pal.amber
            };
            painter.text(
                pos2(r_rect.left() + 6.0, r_rect.top() + 3.0),
                egui::Align2::LEFT_TOP,
                format!("RSI(14) {:.1}", last_r),
                FontId::monospace(9.5),
                rc,
            );
        }
    }

    // Evenly spaced time axis from true min_t..max_t
    let fmt_str = if span_t > 86_400.0 * 2.0 { "%m/%d" } else { "%H:%M" };
    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let t_lbl = min_t + (frac * span_t).round() as i64;
        let dt = chrono::DateTime::from_timestamp(t_lbl, 0)
            .map(|d| d.with_timezone(&chrono::Local).format(fmt_str).to_string())
            .unwrap_or_default();
        let x = (plot.left() + (frac as f32) * plot.width()).clamp(plot.left() + 20.0, plot.right() - 20.0);
        painter.text(
            pos2(x, rect.bottom() - 10.0),
            egui::Align2::CENTER_CENTER,
            dt,
            FontId::monospace(9.5),
            pal.text_muted,
        );
    }

    // Hover crosshair (snaps cleanly to nearest timestamp point)
    if let Some(hover) = response.hover_pos()
        && plot.contains(hover)
    {
        let target_t =
            min_t + ((((hover.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64) * span_t).round() as i64;
        let idx = points
            .binary_search_by_key(&target_t, |pt| pt.t)
            .unwrap_or_else(|ins| {
                if ins == 0 {
                    0
                } else if ins >= points.len() {
                    points.len() - 1
                } else if (points[ins].t - target_t).abs() < (target_t - points[ins - 1].t).abs() {
                    ins
                } else {
                    ins - 1
                }
            });
        let pt = &points[idx];
        let cx = x_of_t(pt.t);
        let cy = y_of(pt.p);

        painter.line_segment(
            [pos2(cx, plot.top()), pos2(cx, rect.bottom() - pad_b)],
            Stroke::new(1.0, pal.crosshair),
        );
        painter.line_segment(
            [pos2(plot.left(), cy), pos2(plot.right(), cy)],
            Stroke::new(1.0, pal.crosshair),
        );
        painter.circle_filled(pos2(cx, cy), 3.5, trend_col);
        painter.circle_stroke(pos2(cx, cy), 3.5, Stroke::new(1.2, pal.bg_card));

        let tm = chrono::DateTime::from_timestamp(pt.t, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%m/%d %H:%M").to_string())
            .unwrap_or_default();
        let tip = format!("{}  ·  {}", format_odds(Some(pt.p), odds_fmt, tick_size), tm);
        let bx = (cx + 10.0).min(plot.right() - 144.0).max(plot.left() + 6.0);
        let by = (cy - 28.0).max(plot.top() + 6.0);
        let tip_rect = Rect::from_min_size(pos2(bx, by), vec2(138.0, 22.0));
        painter.rect_filled(tip_rect, CornerRadius::same(4), pal.tooltip_bg);
        painter.rect_stroke(
            tip_rect,
            CornerRadius::same(4),
            Stroke::new(1.0, pal.border_strong),
            egui::StrokeKind::Inside,
        );
        painter.text(
            tip_rect.center(),
            egui::Align2::CENTER_CENTER,
            tip,
            FontId::monospace(10.0),
            pal.tooltip_fg,
        );
    }
}

pub fn draw_oracle_chart(
    ui: &mut Ui,
    lang: Lang,
    pal: ThemePalette,
    data: Option<&OracleChartData<'_>>,
    height: f32,
) {
    let desired = vec2(ui.available_width().max(120.0), height.max(160.0));
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, CornerRadius::same(6), pal.bg_card);
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    let pad_l = 12.0;
    let pad_r = 84.0;
    let pad_t = 14.0;
    let pad_b = 22.0;
    let plot = Rect::from_min_max(
        pos2(rect.left() + pad_l, rect.top() + pad_t),
        pos2(
            (rect.right() - pad_r).max(rect.left() + pad_l + 20.0),
            (rect.bottom() - pad_b).max(rect.top() + pad_t + 30.0),
        ),
    );

    let Some(d) = data else {
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            tr(
                lang,
                "Select a 5m / 15m crypto Up/Down market for Chainlink Oracle view",
                "请选择 5m / 15m 加密货币涨跌市场查看 Chainlink 预言机走势",
            ),
            FontId::proportional(13.0),
            pal.text_muted,
        );
        return;
    };

    if d.samples.is_empty() {
        let wait_msg = match d.price_to_beat {
            Some(beat) => format!(
                "{} · {} {}",
                tr(lang, "Waiting for live Chainlink RTDS ticks...", "正在等待 Chainlink 实时报价..."),
                tr(lang, "Strike", "基准价"),
                format_oracle_usd(beat)
            ),
            None => tr(
                lang,
                "Connecting live Chainlink RTDS feed...",
                "正在连接 Chainlink 实时预言机...",
            )
            .to_string(),
        };
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            wait_msg,
            FontId::proportional(13.0),
            pal.text_muted,
        );
        return;
    }

    let mut rolling: VecDeque<OracleSample> = VecDeque::new();
    let mut twap_pts: Vec<(i64, f64)> = Vec::with_capacity(d.samples.len());
    let mut min_t = i64::MAX;
    let mut max_t = i64::MIN;
    let mut min_p = f64::INFINITY;
    let mut max_p = f64::NEG_INFINITY;

    for &(t_ms, p) in d.samples {
        min_t = min_t.min(t_ms);
        max_t = max_t.max(t_ms);
        min_p = min_p.min(p);
        max_p = max_p.max(p);
        rolling.push_back(OracleSample {
            timestamp_ms: t_ms,
            price: p,
        });
        while rolling.front().is_some_and(|s| s.timestamp_ms < t_ms - 600_000) {
            rolling.pop_front();
        }
        if let Some(tw) = compute_twap_60s(&rolling, t_ms) {
            min_p = min_p.min(tw);
            max_p = max_p.max(tw);
            twap_pts.push((t_ms, tw));
        }
    }
    if let Some(beat) = d.price_to_beat {
        min_p = min_p.min(beat);
        max_p = max_p.max(beat);
    }
    if max_t <= min_t {
        max_t = min_t + 1000;
    }

    let span_t = (max_t - min_t).max(1) as f64;
    let min_pad = (max_p.abs() * 0.0005).max(0.0005);
    let span_pad = ((max_p - min_p) * 0.16).max(min_pad);
    min_p -= span_pad;
    max_p += span_pad;
    let range_p = (max_p - min_p).max(1e-6);

    let y_of = |p: f64| -> f32 {
        plot.top() + (((max_p - p) / range_p) as f32) * plot.height()
    };
    let x_of_t = |t_ms: i64| -> f32 {
        let frac = ((t_ms - min_t) as f64 / span_t).clamp(0.0, 1.0) as f32;
        plot.left() + frac * plot.width()
    };

    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let y = plot.top() + (frac as f32) * plot.height();
        let p = max_p - frac * range_p;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0, pal.grid_line),
        );
        painter.text(
            pos2(plot.right() + 8.0, y),
            egui::Align2::LEFT_CENTER,
            format_oracle_usd(p),
            FontId::monospace(10.0),
            pal.text_muted,
        );
    }

    // Price to Beat horizontal dashed reference line
    if let Some(beat) = d.price_to_beat {
        let y = y_of(beat);
        let beat_col = pal.amber;
        let dash_col = ThemePalette::tint(beat_col, 170);
        let mut x = plot.left();
        while x < plot.right() {
            let x_end = (x + 6.0).min(plot.right());
            painter.line_segment([pos2(x, y), pos2(x_end, y)], Stroke::new(1.2, dash_col));
            x += 10.0;
        }
        let badge = Rect::from_min_max(
            pos2(plot.right() + 4.0, y - 9.0),
            pos2(rect.right() - 4.0, y + 9.0),
        );
        painter.rect_filled(badge, CornerRadius::same(4), beat_col);
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            format!("BEAT {}", format_oracle_usd(beat)),
            FontId::monospace(9.0),
            ThemePalette::contrast_on(beat_col),
        );
    }

    let cl_pts: Vec<Pos2> = d
        .samples
        .iter()
        .map(|&(t, p)| pos2(x_of_t(t), y_of(p)))
        .collect();
    if cl_pts.len() >= 2 {
        painter.add(Shape::line(cl_pts.clone(), Stroke::new(1.6, pal.blue)));
    }
    if let Some(&last_pt) = cl_pts.last() {
        painter.circle_filled(last_pt, 3.0, pal.blue);
        painter.circle_stroke(last_pt, 3.0, Stroke::new(1.1, pal.bg_card));
    }

    let twap_scr: Vec<Pos2> = twap_pts
        .iter()
        .map(|&(t, p)| pos2(x_of_t(t), y_of(p)))
        .collect();
    if twap_scr.len() >= 2 {
        painter.add(Shape::line(twap_scr, Stroke::new(1.5, pal.ind_sma)));
    }

    let spot = d.samples.last().map(|s| s.1).unwrap_or(0.0);
    let mut legend_parts = vec![(
        format!("CL {} {}", d.symbol.to_uppercase(), format_oracle_usd(spot)),
        pal.blue,
    )];
    if let Some(tw) = d.twap_60s {
        legend_parts.push((format!("TWAP60 {}", format_oracle_usd(tw)), pal.ind_sma));
    }
    if let Some(bm) = d.binance_mid {
        legend_parts.push((format!("BINANCE {}", format_oracle_usd(bm)), pal.text_muted));
    }
    let mut lx = plot.left() + 6.0;
    for (txt, col) in legend_parts {
        painter.text(
            pos2(lx, plot.top() + 2.0),
            egui::Align2::LEFT_TOP,
            &txt,
            FontId::monospace(9.5),
            col,
        );
        lx += (txt.len() as f32) * 6.0 + 14.0;
    }

    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let t_lbl = min_t + (frac * span_t).round() as i64;
        let dt = chrono::DateTime::from_timestamp_millis(t_lbl)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        let x = (plot.left() + (frac as f32) * plot.width())
            .clamp(plot.left() + 24.0, plot.right() - 24.0);
        painter.text(
            pos2(x, rect.bottom() - 10.0),
            egui::Align2::CENTER_CENTER,
            dt,
            FontId::monospace(9.5),
            pal.text_muted,
        );
    }

    if let Some(hover) = response.hover_pos()
        && plot.contains(hover)
    {
        let target_t =
            min_t + ((((hover.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64) * span_t).round() as i64;
        let idx = d
            .samples
            .binary_search_by_key(&target_t, |pt| pt.0)
            .unwrap_or_else(|ins| {
                if ins == 0 {
                    0
                } else if ins >= d.samples.len() {
                    d.samples.len() - 1
                } else if (d.samples[ins].0 - target_t).abs() < (target_t - d.samples[ins - 1].0).abs() {
                    ins
                } else {
                    ins - 1
                }
            });
        let (st, sp) = d.samples[idx];
        let cx = x_of_t(st);
        let cy = y_of(sp);
        painter.line_segment(
            [pos2(cx, plot.top()), pos2(cx, rect.bottom() - pad_b)],
            Stroke::new(1.0, pal.crosshair),
        );
        painter.line_segment(
            [pos2(plot.left(), cy), pos2(plot.right(), cy)],
            Stroke::new(1.0, pal.crosshair),
        );
        painter.circle_filled(pos2(cx, cy), 3.5, pal.blue);

        let tm = chrono::DateTime::from_timestamp_millis(st)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        let tip = format!("{} · {}", format_oracle_usd(sp), tm);
        let bx = (cx + 10.0).min(plot.right() - 160.0).max(plot.left() + 6.0);
        let by = (cy - 28.0).max(plot.top() + 6.0);
        let tip_rect = Rect::from_min_size(pos2(bx, by), vec2(154.0, 22.0));
        painter.rect_filled(tip_rect, CornerRadius::same(4), pal.tooltip_bg);
        painter.rect_stroke(
            tip_rect,
            CornerRadius::same(4),
            Stroke::new(1.0, pal.border_strong),
            egui::StrokeKind::Inside,
        );
        painter.text(
            tip_rect.center(),
            egui::Align2::CENTER_CENTER,
            tip,
            FontId::monospace(10.0),
            pal.tooltip_fg,
        );
    }
}

fn compute_ema(vals: &[f64], period: usize) -> Vec<f64> {
    if vals.is_empty() {
        return Vec::new();
    }
    let k = 2.0 / (period as f64 + 1.0);
    let mut out = Vec::with_capacity(vals.len());
    let mut prev = vals[0];
    out.push(prev);
    for &v in &vals[1..] {
        prev = v * k + prev * (1.0 - k);
        out.push(prev);
    }
    out
}

fn compute_sma(vals: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = Vec::with_capacity(vals.len());
    let mut sum = 0.0;
    for i in 0..vals.len() {
        sum += vals[i];
        if i >= period {
            sum -= vals[i - period];
        }
        if i + 1 >= period {
            out.push(Some(sum / period as f64));
        } else {
            out.push(None);
        }
    }
    out
}

fn compute_bollinger(vals: &[f64], period: usize, mult: f64) -> Vec<(Option<f64>, Option<f64>)> {
    let mut out = Vec::with_capacity(vals.len());
    for i in 0..vals.len() {
        if i + 1 < period {
            out.push((None, None));
            continue;
        }
        let slice = &vals[i + 1 - period..=i];
        let mean = slice.iter().sum::<f64>() / period as f64;
        let var = slice.iter().map(|&v| (v - mean).powi(2)).sum::<f64>() / period as f64;
        let sd = var.sqrt();
        out.push((
            Some((mean - mult * sd).max(0.001)),
            Some((mean + mult * sd).min(0.999)),
        ));
    }
    out
}

fn compute_running_mean(vals: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(vals.len());
    let mut sum = 0.0;
    for (i, &v) in vals.iter().enumerate() {
        sum += v;
        out.push(sum / (i as f64 + 1.0));
    }
    out
}

fn compute_rsi(vals: &[f64], period: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; vals.len()];
    if vals.len() <= period {
        return out;
    }
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;
    for i in 1..=period {
        let diff = vals[i] - vals[i - 1];
        if diff >= 0.0 {
            avg_gain += diff;
        } else {
            avg_loss -= diff;
        }
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;
    let calc = |g: f64, l: f64| {
        if l < 1e-12 {
            100.0
        } else {
            100.0 - (100.0 / (1.0 + g / l))
        }
    };
    out[period] = Some(calc(avg_gain, avg_loss));
    for i in (period + 1)..vals.len() {
        let diff = vals[i] - vals[i - 1];
        let g = if diff > 0.0 { diff } else { 0.0 };
        let l = if diff < 0.0 { -diff } else { 0.0 };
        avg_gain = (avg_gain * (period as f64 - 1.0) + g) / period as f64;
        avg_loss = (avg_loss * (period as f64 - 1.0) + l) / period as f64;
        out[i] = Some(calc(avg_gain, avg_loss));
    }
    out
}

fn build_candles(pts: &[PricePoint]) -> Vec<Candle> {
    if pts.is_empty() {
        return Vec::new();
    }
    let span = (pts.last().unwrap().t - pts.first().unwrap().t).max(60);
    let bucket = (span / 48).max(15);
    let mut map = std::collections::BTreeMap::<i64, Candle>::new();
    for pt in pts {
        let k = (pt.t / bucket) * bucket;
        map.entry(k)
            .and_modify(|c| {
                c.h = c.h.max(pt.p);
                c.l = c.l.min(pt.p);
                c.c = pt.p;
            })
            .or_insert(Candle {
                t: k,
                o: pt.p,
                h: pt.p,
                l: pt.p,
                c: pt.p,
            });
    }
    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_cache_recomputes_only_on_change() {
        let mut cache = ChartCache::default();
        let mut pts: Vec<PricePoint> = (0..30)
            .map(|i| PricePoint {
                t: 1_700_000_000 + i * 60,
                p: 0.40 + (i as f64) * 0.004,
            })
            .collect();
        let mut ind = IndicatorConfig {
            ema9: true,
            ..Default::default()
        };

        assert!(cache.ensure("tokA", "1d", &pts, ChartMode::Line, ind));
        assert!(!cache.ensure("tokA", "1d", &pts, ChartMode::Line, ind));

        // Update last point price -> triggers recompute once
        pts.last_mut().unwrap().p = 0.61;
        assert!(cache.ensure("tokA", "1d", &pts, ChartMode::Line, ind));
        assert!(!cache.ensure("tokA", "1d", &pts, ChartMode::Line, ind));

        // Toggle indicator -> triggers recompute once
        ind.bollinger = true;
        assert!(cache.ensure("tokA", "1d", &pts, ChartMode::Line, ind));
        assert!(!cache.ensure("tokA", "1d", &pts, ChartMode::Line, ind));

        // Outcome switch -> invalidates cache
        assert!(cache.ensure("tokB", "1d", &pts, ChartMode::Line, ind));
        assert!(!cache.ensure("tokB", "1d", &pts, ChartMode::Line, ind));

        // Interval switch -> invalidates cache
        assert!(cache.ensure("tokB", "6h", &pts, ChartMode::Line, ind));
        assert!(!cache.ensure("tokB", "6h", &pts, ChartMode::Line, ind));

        // Manual clear -> invalidates cache
        cache.clear();
        assert!(cache.ensure("tokB", "6h", &pts, ChartMode::Line, ind));
    }

    #[test]
    fn chart_cache_handles_zero_and_one_point_with_all_indicators() {
        let mut cache = ChartCache::default();
        let ind = IndicatorConfig {
            ema9: true,
            sma20: true,
            bollinger: true,
            vwap: true,
            rsi14: true,
        };

        // 0-point input
        for mode in [ChartMode::Line, ChartMode::Candles] {
            assert!(cache.ensure("tok0", "1d", &[], mode, ind));
            assert!(cache.base_min_p.is_finite());
            assert!(cache.base_max_p.is_finite());
            assert!(cache.bb.is_none());
            cache.clear();
        }

        // 1-point input with Bollinger enabled
        let one = [PricePoint { t: 1_700_000_000, p: 0.55 }];
        for mode in [ChartMode::Line, ChartMode::Candles] {
            assert!(cache.ensure("tok1", "1h", &one, mode, ind));
            assert!(cache.base_min_p.is_finite());
            assert!(cache.base_max_p.is_finite());
            let bb = cache.bb.as_ref().unwrap();
            assert_eq!(bb.len(), 1);
            assert_eq!(bb[0], (None, None));
            cache.clear();
        }
    }
}
