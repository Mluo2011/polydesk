use egui::{Color32, CornerRadius, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

use polydesk::automation::{AutomationRule, RuleKind};
use polydesk::book::BookView;
use polydesk::i18n::{Lang, tr};
use polydesk::market::PricePoint;
use polydesk::odds::{OddsFormat, format_odds};
use polydesk::paper::PaperPosition;

use super::theme::ThemePalette;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartMode {
    Line,
    Candles,
}

#[derive(Debug, Clone, Copy)]
pub struct IndicatorConfig {
    pub ema9: bool,
    pub sma20: bool,
    pub bollinger: bool,
    pub vwap: bool,
    pub rsi14: bool,
}

impl Default for IndicatorConfig {
    fn default() -> Self {
        Self {
            ema9: false,
            sma20: false,
            bollinger: false,
            vwap: false,
            rsi14: false,
        }
    }
}

struct Candle {
    t: i64,
    o: f64,
    h: f64,
    l: f64,
    c: f64,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_price_chart(
    ui: &mut Ui,
    lang: Lang,
    pal: ThemePalette,
    points: &[PricePoint],
    mode: ChartMode,
    indicators: IndicatorConfig,
    odds_fmt: OddsFormat,
    tick_size: f64,
    book: Option<&BookView>,
    position: Option<&PaperPosition>,
    rules: &[AutomationRule],
    height: f32,
) {
    let desired = vec2(ui.available_width().max(120.0), height.max(160.0));
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);

    // Modern adaptive card surface (Light or Dark!)
    painter.rect_filled(rect, CornerRadius::same(12), pal.bg_card);
    painter.rect_stroke(
        rect,
        CornerRadius::same(12),
        Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    let pad_l = 14.0;
    let pad_r = 68.0;
    let pad_t = 14.0;
    let pad_b = 24.0;
    let rsi_h = if indicators.rsi14 && height >= 210.0 { 58.0 } else { 0.0 };
    let rsi_gap = if rsi_h > 0.0 { 8.0 } else { 0.0 };

    let plot = Rect::from_min_max(
        pos2(rect.left() + pad_l, rect.top() + pad_t),
        pos2(
            (rect.right() - pad_r).max(rect.left() + pad_l + 20.0),
            (rect.bottom() - pad_b - rsi_h - rsi_gap).max(rect.top() + pad_t + 30.0),
        ),
    );

    if points.len() < 2 {
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            tr(lang, "Loading live Polymarket chart...", "正在加载 Polymarket 实时走势..."),
            FontId::proportional(13.0),
            Color32::from_rgb(100, 116, 139),
        );
        return;
    }

    // TRUE timestamp-based X-axis (prevents live ticks from stretching into a weird flat tail!)
    let min_t = points.iter().map(|pt| pt.t).min().unwrap_or(0);
    let max_t = points.iter().map(|pt| pt.t).max().unwrap_or(min_t + 1);
    let span_t = (max_t - min_t).max(1) as f64;

    let prices: Vec<f64> = points.iter().map(|pt| pt.p).collect();
    let ema9_vals = indicators.ema9.then(|| compute_ema(&prices, 9));
    let sma20_vals = (indicators.sma20 || indicators.bollinger).then(|| compute_sma(&prices, 20));
    let bb_vals = indicators.bollinger.then(|| compute_bollinger(&prices, 20, 2.0));
    let vwap_vals = indicators.vwap.then(|| compute_running_mean(&prices));
    let rsi_vals = (rsi_h > 0.0).then(|| compute_rsi(&prices, 14));

    let mut min_p = f64::INFINITY;
    let mut max_p = f64::NEG_INFINITY;
    for &p in &prices {
        min_p = min_p.min(p);
        max_p = max_p.max(p);
    }
    if let Some(ref bb) = bb_vals {
        for &(lo, hi) in bb {
            if let (Some(l), Some(h)) = (lo, hi) {
                min_p = min_p.min(l);
                max_p = max_p.max(h);
            }
        }
    }
    if let Some(pos) = position
        && pos.shares > 1e-6
    {
        min_p = min_p.min(pos.avg_entry);
        max_p = max_p.max(pos.avg_entry);
    }
    for r in rules {
        if r.active {
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
    let grid_col = pal.grid_line;
    let label_col = pal.text_muted;
    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let y = plot.top() + (frac as f32) * plot.height();
        let p = max_p - frac * range_p;
        painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, grid_col));
        painter.text(
            pos2(plot.right() + 8.0, y),
            egui::Align2::LEFT_CENTER,
            format_odds(Some(p), odds_fmt, tick_size),
            FontId::monospace(10.0),
            label_col,
        );
    }

    // Subtle Bid/Ask spread band
    if let Some(b) = book
        && let (Some(bb), Some(ba)) = (b.best_bid, b.best_ask)
        && bb >= min_p
        && ba <= max_p
    {
        let ya = y_of(ba);
        let yb = y_of(bb);
        let band = Rect::from_min_max(
            pos2(plot.left(), ya.min(yb)),
            pos2(plot.right(), ya.max(yb).max(ya.min(yb) + 1.5)),
        );
        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(56, 189, 248, 12));
    }

    // Bollinger Bands (20, 2σ) shaded envelope (only when enabled)
    if let Some(ref bb) = bb_vals {
        let mut upper_pts = Vec::new();
        let mut lower_pts = Vec::new();
        for (i, &(lo, hi)) in bb.iter().enumerate() {
            if let (Some(l), Some(h)) = (lo, hi) {
                let x = x_of_t(points[i].t);
                upper_pts.push(pos2(x, y_of(h)));
                lower_pts.push(pos2(x, y_of(l)));
            }
        }
        let bb_fill = Color32::from_rgba_unmultiplied(99, 102, 241, 20);
        let bb_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(129, 140, 248, 120));
        for w in 0..upper_pts.len().saturating_sub(1) {
            let quad = vec![upper_pts[w], upper_pts[w + 1], lower_pts[w + 1], lower_pts[w]];
            painter.add(Shape::convex_polygon(quad, bb_fill, Stroke::NONE));
        }
        if upper_pts.len() >= 2 {
            painter.add(Shape::line(upper_pts, bb_stroke));
            painter.add(Shape::line(lower_pts, bb_stroke));
        }
    }

    let up_col = pal.green;
    let dn_col = pal.red;
    let first_p = prices[0];
    let last_p = *prices.last().unwrap_or(&0.5);
    let is_up = last_p >= first_p;
    let trend_col = if is_up { up_col } else { dn_col };

    match mode {
        ChartMode::Line => {
            let poly: Vec<Pos2> = points
                .iter()
                .map(|pt| pos2(x_of_t(pt.t), y_of(pt.p)))
                .collect();

            // Smooth GPU hardware-interpolated vertical gradient fill under the line!
            if poly.len() >= 2 {
                let top_alpha = if pal.light { 50 } else { 44 };
                let top_fill = Color32::from_rgba_unmultiplied(trend_col.r(), trend_col.g(), trend_col.b(), top_alpha);
                let bot_fill = Color32::from_rgba_unmultiplied(trend_col.r(), trend_col.g(), trend_col.b(), 0);

                let mut mesh = Mesh::default();
                for (i, &pt) in poly.iter().enumerate() {
                    let top_idx = (i * 2) as u32;
                    let bot_idx = top_idx + 1;
                    mesh.colored_vertex(pt, top_fill);
                    mesh.colored_vertex(pos2(pt.x, plot.bottom()), bot_fill);
                    if i > 0 {
                        let prev_top = top_idx - 2;
                        let prev_bot = top_idx - 1;
                        mesh.add_triangle(prev_top, top_idx, bot_idx);
                        mesh.add_triangle(prev_top, bot_idx, prev_bot);
                    }
                }
                painter.add(Shape::mesh(mesh));

                // Soft subtle glow stroke underneath main line
                let glow_col = Color32::from_rgba_unmultiplied(trend_col.r(), trend_col.g(), trend_col.b(), if pal.light { 36 } else { 45 });
                painter.add(Shape::line(poly.clone(), Stroke::new(4.5, glow_col)));
                painter.add(Shape::line(poly.clone(), Stroke::new(2.0, trend_col)));

                // Glowing live endpoint dot (modern Polymarket/Robinhood look!)
                if let Some(&last_pt) = poly.last() {
                    painter.circle_filled(last_pt, 7.5, glow_col);
                    painter.circle_filled(last_pt, 3.8, trend_col);
                    painter.circle_stroke(last_pt, 3.8, Stroke::new(1.4, pal.bg_card));
                }
            }
        }
        ChartMode::Candles => {
            let candles = build_candles(points);
            let n = candles.len().max(1);
            let bar_w = ((plot.width() / n as f32) * 0.62).clamp(3.0, 14.0);
            for c in &candles {
                let x = x_of_t(c.t).clamp(plot.left() + bar_w * 0.5, plot.right() - bar_w * 0.5);
                let col = if c.c >= c.o { up_col } else { dn_col };
                painter.line_segment([pos2(x, y_of(c.h)), pos2(x, y_of(c.l))], Stroke::new(1.1, col));
                let top = y_of(c.o.max(c.c));
                let bot = y_of(c.o.min(c.c)).max(top + 1.5);
                painter.rect_filled(
                    Rect::from_min_max(pos2(x - bar_w * 0.5, top), pos2(x + bar_w * 0.5, bot)),
                    2.0,
                    col,
                );
            }
        }
    }

    // Optional indicator overlay curves (OFF by default)
    if let Some(ref ema) = ema9_vals {
        let pts: Vec<Pos2> = ema
            .iter()
            .enumerate()
            .map(|(i, &v)| pos2(x_of_t(points[i].t), y_of(v)))
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.4, Color32::from_rgb(56, 189, 248))));
    }
    if indicators.sma20
        && let Some(ref sma) = sma20_vals
    {
        let pts: Vec<Pos2> = sma
            .iter()
            .enumerate()
            .filter_map(|(i, &o)| o.map(|v| pos2(x_of_t(points[i].t), y_of(v))))
            .collect();
        if pts.len() >= 2 {
            painter.add(Shape::line(pts, Stroke::new(1.4, Color32::from_rgb(245, 166, 35))));
        }
    }
    if let Some(ref vw) = vwap_vals {
        let pts: Vec<Pos2> = vw
            .iter()
            .enumerate()
            .map(|(i, &v)| pos2(x_of_t(points[i].t), y_of(v)))
            .collect();
        painter.add(Shape::line(pts, Stroke::new(1.3, Color32::from_rgb(192, 132, 252))));
    }

    // Indicator legend top-left (only shown if user toggled at least one indicator on)
    let mut legend = Vec::new();
    if let Some(ref e) = ema9_vals && let Some(&v) = e.last() {
        legend.push((format!("EMA9 {:.1}¢", v * 100.0), Color32::from_rgb(56, 189, 248)));
    }
    if indicators.sma20 && let Some(ref s) = sma20_vals && let Some(Some(v)) = s.last() {
        legend.push((format!("SMA20 {:.1}¢", v * 100.0), Color32::from_rgb(245, 166, 35)));
    }
    if indicators.bollinger {
        legend.push(("BB(20,2)".into(), Color32::from_rgb(129, 140, 248)));
    }
    if let Some(ref vw) = vwap_vals && let Some(&v) = vw.last() {
        legend.push((format!("VWAP {:.1}¢", v * 100.0), Color32::from_rgb(192, 132, 252)));
    }
    let mut lx = plot.left() + 4.0;
    for (txt, col) in legend {
        painter.text(pos2(lx, plot.top() + 2.0), egui::Align2::LEFT_TOP, &txt, FontId::monospace(9.5), col);
        lx += (txt.len() as f32) * 6.0 + 10.0;
    }

    // Subtle dashed horizontal lines for Avg Entry / SL / TP / Trail + Live price pill
    let draw_dashed_tag = |p: f64, col: Color32, line_alpha: u8, tag: String| {
        let y = y_of(p);
        if y < plot.top() - 4.0 || y > plot.bottom() + 4.0 {
            return;
        }
        let dash_col = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), line_alpha);
        let mut x = plot.left();
        while x < plot.right() {
            let x_end = (x + 5.0).min(plot.right());
            painter.line_segment([pos2(x, y), pos2(x_end, y)], Stroke::new(1.0, dash_col));
            x += 9.0;
        }
        let badge = Rect::from_min_max(pos2(plot.right() + 4.0, y - 9.0), pos2(rect.right() - 4.0, y + 9.0));
        painter.rect_filled(badge, CornerRadius::same(5), col);
        let pill_txt = if pal.light { Color32::WHITE } else { Color32::from_rgb(8, 11, 16) };
        painter.text(badge.center(), egui::Align2::CENTER_CENTER, tag, FontId::monospace(9.5), pill_txt);
    };

    if let Some(pos) = position
        && pos.shares > 1e-6
    {
        draw_dashed_tag(pos.avg_entry, Color32::from_rgb(56, 189, 248), 160, format!("AVG {:.1}", pos.avg_entry * 100.0));
    }
    for r in rules {
        if !r.active { continue; }
        match r.kind {
            RuleKind::StopLoss => draw_dashed_tag(r.trigger_price, dn_col, 160, format!("SL {:.1}", r.trigger_price * 100.0)),
            RuleKind::TakeProfit => draw_dashed_tag(r.trigger_price, up_col, 160, format!("TP {:.1}", r.trigger_price * 100.0)),
            RuleKind::TrailingStop => draw_dashed_tag(r.trigger_price, Color32::from_rgb(245, 166, 35), 160, format!("TRL {:.1}", r.trigger_price * 100.0)),
            RuleKind::DripFeed => {}
        }
    }
    draw_dashed_tag(last_p, trend_col, 55, format_odds(Some(last_p), odds_fmt, tick_size));

    // RSI(14) Sub-Pane (only when toggled on)
    if let Some(ref rsi) = rsi_vals {
        let r_rect = Rect::from_min_max(
            pos2(plot.left(), plot.bottom() + rsi_gap),
            pos2(plot.right(), rect.bottom() - pad_b),
        );
        painter.rect_filled(r_rect, 6.0, pal.bg_sub);
        painter.rect_stroke(r_rect, 6.0, Stroke::new(0.8, pal.border), egui::StrokeKind::Inside);

        let y_rsi = |v: f64| r_rect.top() + (((100.0 - v.clamp(0.0, 100.0)) / 100.0) as f32) * r_rect.height();
        let y70 = y_rsi(70.0);
        let y30 = y_rsi(30.0);
        painter.line_segment([pos2(r_rect.left(), y70), pos2(r_rect.right(), y70)], Stroke::new(0.8, Color32::from_rgba_unmultiplied(255, 77, 106, 70)));
        painter.line_segment([pos2(r_rect.left(), y30), pos2(r_rect.right(), y30)], Stroke::new(0.8, Color32::from_rgba_unmultiplied(0, 211, 149, 70)));
        painter.text(pos2(r_rect.right() + 8.0, y70), egui::Align2::LEFT_CENTER, "70", FontId::monospace(9.0), Color32::from_rgb(255, 77, 106));
        painter.text(pos2(r_rect.right() + 8.0, y30), egui::Align2::LEFT_CENTER, "30", FontId::monospace(9.0), Color32::from_rgb(0, 211, 149));

        let r_pts: Vec<Pos2> = rsi
            .iter()
            .enumerate()
            .filter_map(|(i, &o)| o.map(|v| pos2(x_of_t(points[i].t), y_rsi(v))))
            .collect();
        if r_pts.len() >= 2 {
            painter.add(Shape::line(r_pts, Stroke::new(1.4, Color32::from_rgb(245, 166, 35))));
        }
        if let Some(Some(last_r)) = rsi.last() {
            let rc = if *last_r >= 70.0 { dn_col } else if *last_r <= 30.0 { up_col } else { Color32::from_rgb(245, 166, 35) };
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
        painter.text(pos2(x, rect.bottom() - 10.0), egui::Align2::CENTER_CENTER, dt, FontId::monospace(9.5), Color32::from_rgb(100, 116, 139));
    }

    // Hover crosshair (snaps cleanly to nearest timestamp point!)
    if let Some(hover) = response.hover_pos()
        && plot.contains(hover)
    {
        let target_t = min_t + ((((hover.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64) * span_t).round() as i64;
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

        let cross_col = if pal.light {
            Color32::from_rgba_unmultiplied(15, 23, 42, 68)
        } else {
            Color32::from_rgba_unmultiplied(226, 232, 240, 60)
        };
        painter.line_segment([pos2(cx, plot.top()), pos2(cx, rect.bottom() - pad_b)], Stroke::new(1.0, cross_col));
        painter.line_segment([pos2(plot.left(), cy), pos2(plot.right(), cy)], Stroke::new(1.0, cross_col));
        painter.circle_filled(pos2(cx, cy), 4.2, trend_col);
        painter.circle_stroke(pos2(cx, cy), 4.2, Stroke::new(1.5, pal.bg_card));

        let tm = chrono::DateTime::from_timestamp(pt.t, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%m/%d %H:%M").to_string())
            .unwrap_or_default();
        let tip = format!("{}  ·  {}", format_odds(Some(pt.p), odds_fmt, tick_size), tm);
        let bx = (cx + 10.0).min(plot.right() - 144.0).max(plot.left() + 6.0);
        let by = (cy - 28.0).max(plot.top() + 6.0);
        let tip_rect = Rect::from_min_size(pos2(bx, by), vec2(138.0, 22.0));
        painter.rect_filled(tip_rect, CornerRadius::same(6), pal.tooltip_bg);
        painter.rect_stroke(tip_rect, CornerRadius::same(6), Stroke::new(1.0, pal.border_strong), egui::StrokeKind::Inside);
        painter.text(tip_rect.center(), egui::Align2::CENTER_CENTER, tip, FontId::monospace(10.0), pal.tooltip_fg);
    }
}

fn compute_ema(vals: &[f64], period: usize) -> Vec<f64> {
    if vals.is_empty() { return Vec::new(); }
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
        if i >= period { sum -= vals[i - period]; }
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
        out.push((Some((mean - mult * sd).max(0.001)), Some((mean + mult * sd).min(0.999))));
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
    if vals.len() <= period { return out; }
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;
    for i in 1..=period {
        let diff = vals[i] - vals[i - 1];
        if diff >= 0.0 { avg_gain += diff; } else { avg_loss -= diff; }
    }
    avg_gain /= period as f64;
    avg_loss /= period as f64;
    let calc = |g: f64, l: f64| if l < 1e-12 { 100.0 } else { 100.0 - (100.0 / (1.0 + g / l)) };
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
    if pts.is_empty() { return Vec::new(); }
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
            .or_insert(Candle { t: k, o: pt.p, h: pt.p, l: pt.p, c: pt.p });
    }
    map.into_values().collect()
}
