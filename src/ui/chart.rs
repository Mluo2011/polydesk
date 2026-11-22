use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};

use polydesk::automation::{AutomationRule, RuleKind};
use polydesk::book::BookView;
use polydesk::i18n::{Lang, tr};
use polydesk::market::PricePoint;
use polydesk::odds::{OddsFormat, format_odds};
use polydesk::paper::PaperPosition;

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
    o: f64,
    h: f64,
    l: f64,
    c: f64,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_price_chart(
    ui: &mut Ui,
    lang: Lang,
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

    painter.rect_filled(rect, CornerRadius::same(10), Color32::from_rgb(13, 18, 28));
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, Color32::from_rgb(31, 41, 55)),
        egui::StrokeKind::Inside,
    );

    let pad_l = 10.0;
    let pad_r = 66.0;
    let pad_t = 12.0;
    let pad_b = 20.0;
    let rsi_h = if indicators.rsi14 && height >= 210.0 { 58.0 } else { 0.0 };
    let rsi_gap = if rsi_h > 0.0 { 6.0 } else { 0.0 };

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
            tr(lang, "Waiting for live Polymarket ticks...", "正在等待 Polymarket 实时行情..."),
            FontId::proportional(12.0),
            Color32::from_rgb(100, 116, 139),
        );
        return;
    }

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
    if let Some(b) = book {
        if let Some(bb) = b.best_bid { min_p = min_p.min(bb); }
        if let Some(ba) = b.best_ask { max_p = max_p.max(ba); }
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

    let span_pad = ((max_p - min_p) * 0.12).max(0.015);
    min_p = (min_p - span_pad).max(0.001);
    max_p = (max_p + span_pad).min(0.999);
    let range_p = (max_p - min_p).max(0.005);

    let y_of = |p: f64| -> f32 {
        plot.top() + (((max_p - p) / range_p) as f32) * plot.height()
    };
    let x_of_idx = |i: usize, total: usize| -> f32 {
        if total <= 1 {
            plot.right()
        } else {
            plot.left() + ((i as f32) / ((total - 1) as f32)) * plot.width()
        }
    };

    // Gridlines & right Y-axis
    let grid_col = Color32::from_rgba_unmultiplied(148, 163, 184, 22);
    let label_col = Color32::from_rgb(123, 139, 163);
    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let y = plot.top() + (frac as f32) * plot.height();
        let p = max_p - frac * range_p;
        painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, grid_col));
        painter.text(
            pos2(plot.right() + 6.0, y),
            egui::Align2::LEFT_CENTER,
            format_odds(Some(p), odds_fmt, tick_size),
            FontId::monospace(10.0),
            label_col,
        );
    }

    // Bid/Ask spread band
    if let Some(b) = book
        && let (Some(bb), Some(ba)) = (b.best_bid, b.best_ask)
    {
        let ya = y_of(ba);
        let yb = y_of(bb);
        let band = Rect::from_min_max(
            pos2(plot.left(), ya.min(yb)),
            pos2(plot.right(), ya.max(yb).max(ya.min(yb) + 1.5)),
        );
        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(56, 189, 248, 20));
    }

    // Bollinger Bands (20, 2σ) shaded envelope
    if let Some(ref bb) = bb_vals {
        let mut upper_pts = Vec::new();
        let mut lower_pts = Vec::new();
        for (i, &(lo, hi)) in bb.iter().enumerate() {
            if let (Some(l), Some(h)) = (lo, hi) {
                let x = x_of_idx(i, bb.len());
                upper_pts.push(pos2(x, y_of(h)));
                lower_pts.push(pos2(x, y_of(l)));
            }
        }
        let bb_fill = Color32::from_rgba_unmultiplied(99, 102, 241, 22);
        let bb_stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(129, 140, 248, 120));
        for w in 0..upper_pts.len().saturating_sub(1) {
            let quad = vec![upper_pts[w], upper_pts[w + 1], lower_pts[w + 1], lower_pts[w]];
            painter.add(egui::Shape::convex_polygon(quad, bb_fill, Stroke::NONE));
        }
        if upper_pts.len() >= 2 {
            painter.add(egui::Shape::line(upper_pts, bb_stroke));
            painter.add(egui::Shape::line(lower_pts, bb_stroke));
        }
    }

    let up_col = Color32::from_rgb(0, 211, 149);
    let dn_col = Color32::from_rgb(255, 77, 106);
    let first_p = prices[0];
    let last_p = *prices.last().unwrap_or(&0.5);
    let trend_col = if last_p >= first_p { up_col } else { dn_col };

    match mode {
        ChartMode::Line => {
            let poly: Vec<Pos2> = prices
                .iter()
                .enumerate()
                .map(|(i, &p)| pos2(x_of_idx(i, prices.len()), y_of(p)))
                .collect();
            let fill_col = if last_p >= first_p {
                Color32::from_rgba_unmultiplied(0, 211, 149, 22)
            } else {
                Color32::from_rgba_unmultiplied(255, 77, 106, 22)
            };
            for seg in poly.windows(2) {
                let quad = vec![seg[0], seg[1], pos2(seg[1].x, plot.bottom()), pos2(seg[0].x, plot.bottom())];
                painter.add(egui::Shape::convex_polygon(quad, fill_col, Stroke::NONE));
            }
            painter.add(egui::Shape::line(poly, Stroke::new(1.8, trend_col)));
        }
        ChartMode::Candles => {
            let candles = build_candles(points);
            let n = candles.len().max(1);
            let bar_w = ((plot.width() / n as f32) * 0.66).clamp(3.0, 14.0);
            for (i, c) in candles.iter().enumerate() {
                let x = plot.left() + (((i as f32) + 0.5) / n as f32) * plot.width();
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

    // Indicator overlay curves
    if let Some(ref ema) = ema9_vals {
        let pts: Vec<Pos2> = ema
            .iter()
            .enumerate()
            .map(|(i, &v)| pos2(x_of_idx(i, ema.len()), y_of(v)))
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.3, Color32::from_rgb(56, 189, 248))));
    }
    if indicators.sma20
        && let Some(ref sma) = sma20_vals
    {
        let pts: Vec<Pos2> = sma
            .iter()
            .enumerate()
            .filter_map(|(i, &o)| o.map(|v| pos2(x_of_idx(i, sma.len()), y_of(v))))
            .collect();
        if pts.len() >= 2 {
            painter.add(egui::Shape::line(pts, Stroke::new(1.3, Color32::from_rgb(245, 166, 35))));
        }
    }
    if let Some(ref vw) = vwap_vals {
        let pts: Vec<Pos2> = vw
            .iter()
            .enumerate()
            .map(|(i, &v)| pos2(x_of_idx(i, vw.len()), y_of(v)))
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.2, Color32::from_rgb(192, 132, 252))));
    }

    // Indicator legend top-left
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

    // Tagged horizontal overlays (Avg Entry, SL, TP, Trailing, Live price)
    let draw_tag_line = |p: f64, col: Color32, tag: String| {
        let y = y_of(p);
        if y < plot.top() - 4.0 || y > plot.bottom() + 4.0 {
            return;
        }
        painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.1, col));
        let badge = Rect::from_min_max(pos2(plot.right() + 2.0, y - 8.0), pos2(rect.right() - 2.0, y + 8.0));
        painter.rect_filled(badge, 2.0, col);
        painter.text(badge.center(), egui::Align2::CENTER_CENTER, tag, FontId::monospace(9.5), Color32::from_rgb(8, 11, 16));
    };

    if let Some(pos) = position
        && pos.shares > 1e-6
    {
        draw_tag_line(pos.avg_entry, Color32::from_rgb(56, 189, 248), format!("AVG {:.1}", pos.avg_entry * 100.0));
    }
    for r in rules {
        if !r.active { continue; }
        match r.kind {
            RuleKind::StopLoss => draw_tag_line(r.trigger_price, dn_col, format!("SL {:.1}", r.trigger_price * 100.0)),
            RuleKind::TakeProfit => draw_tag_line(r.trigger_price, up_col, format!("TP {:.1}", r.trigger_price * 100.0)),
            RuleKind::TrailingStop => draw_tag_line(r.trigger_price, Color32::from_rgb(245, 166, 35), format!("TRL {:.1}", r.trigger_price * 100.0)),
            RuleKind::DripFeed => {}
        }
    }
    draw_tag_line(last_p, trend_col, format_odds(Some(last_p), odds_fmt, tick_size));

    // RSI(14) Sub-Pane
    if let Some(ref rsi) = rsi_vals {
        let r_rect = Rect::from_min_max(
            pos2(plot.left(), plot.bottom() + rsi_gap),
            pos2(plot.right(), rect.bottom() - pad_b),
        );
        painter.rect_filled(r_rect, 2.0, Color32::from_rgb(8, 12, 18));
        painter.rect_stroke(r_rect, 2.0, Stroke::new(0.8, Color32::from_rgb(30, 41, 59)), egui::StrokeKind::Inside);

        let y_rsi = |v: f64| r_rect.top() + (((100.0 - v.clamp(0.0, 100.0)) / 100.0) as f32) * r_rect.height();
        let y70 = y_rsi(70.0);
        let y30 = y_rsi(30.0);
        painter.line_segment([pos2(r_rect.left(), y70), pos2(r_rect.right(), y70)], Stroke::new(0.8, Color32::from_rgba_unmultiplied(255, 77, 106, 90)));
        painter.line_segment([pos2(r_rect.left(), y30), pos2(r_rect.right(), y30)], Stroke::new(0.8, Color32::from_rgba_unmultiplied(0, 211, 149, 90)));
        painter.text(pos2(r_rect.right() + 6.0, y70), egui::Align2::LEFT_CENTER, "70", FontId::monospace(9.0), Color32::from_rgb(255, 77, 106));
        painter.text(pos2(r_rect.right() + 6.0, y30), egui::Align2::LEFT_CENTER, "30", FontId::monospace(9.0), Color32::from_rgb(0, 211, 149));

        let r_pts: Vec<Pos2> = rsi
            .iter()
            .enumerate()
            .filter_map(|(i, &o)| o.map(|v| pos2(x_of_idx(i, rsi.len()), y_rsi(v))))
            .collect();
        if r_pts.len() >= 2 {
            painter.add(egui::Shape::line(r_pts, Stroke::new(1.3, Color32::from_rgb(245, 166, 35))));
        }
        if let Some(Some(last_r)) = rsi.last() {
            let rc = if *last_r >= 70.0 { dn_col } else if *last_r <= 30.0 { up_col } else { Color32::from_rgb(245, 166, 35) };
            painter.text(
                pos2(r_rect.left() + 4.0, r_rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                format!("RSI(14) {:.1}", last_r),
                FontId::monospace(9.5),
                rc,
            );
        }
    }

    // Time axis
    for i in 0..=4 {
        let idx = ((i as f64 / 4.0) * ((points.len() - 1) as f64)).round() as usize;
        let idx = idx.min(points.len() - 1);
        let dt = chrono::DateTime::from_timestamp(points[idx].t, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        let x = x_of_idx(idx, points.len()).clamp(plot.left() + 18.0, plot.right() - 18.0);
        painter.text(pos2(x, rect.bottom() - 8.0), egui::Align2::CENTER_CENTER, dt, FontId::monospace(9.5), Color32::from_rgb(100, 116, 139));
    }

    // Hover crosshair
    if let Some(hover) = response.hover_pos()
        && plot.contains(hover)
    {
        let frac = ((hover.x - plot.left()) / plot.width()).clamp(0.0, 1.0);
        let idx = (frac * ((points.len() - 1) as f32)).round() as usize;
        let pt = &points[idx.min(points.len() - 1)];
        let cx = x_of_idx(idx, points.len());
        let cy = y_of(pt.p);

        let cross_col = Color32::from_rgba_unmultiplied(226, 232, 240, 75);
        painter.line_segment([pos2(cx, plot.top()), pos2(cx, rect.bottom() - pad_b)], Stroke::new(1.0, cross_col));
        painter.line_segment([pos2(plot.left(), cy), pos2(plot.right(), cy)], Stroke::new(1.0, cross_col));
        painter.circle_filled(pos2(cx, cy), 3.5, trend_col);

        let tm = chrono::DateTime::from_timestamp(pt.t, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%m/%d %H:%M").to_string())
            .unwrap_or_default();
        let tip = format!("{}  {}", format_odds(Some(pt.p), odds_fmt, tick_size), tm);
        let bx = (cx + 8.0).min(plot.right() - 122.0).max(plot.left() + 4.0);
        let by = (cy - 24.0).max(plot.top() + 4.0);
        let tip_rect = Rect::from_min_size(pos2(bx, by), vec2(120.0, 18.0));
        painter.rect_filled(tip_rect, 3.0, Color32::from_rgba_unmultiplied(15, 23, 42, 235));
        painter.rect_stroke(tip_rect, 3.0, Stroke::new(1.0, Color32::from_rgb(51, 65, 85)), egui::StrokeKind::Inside);
        painter.text(tip_rect.center(), egui::Align2::CENTER_CENTER, tip, FontId::monospace(10.0), Color32::from_rgb(226, 232, 240));
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
            .or_insert(Candle { o: pt.p, h: pt.p, l: pt.p, c: pt.p });
    }
    map.into_values().collect()
}
