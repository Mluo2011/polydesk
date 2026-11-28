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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShortChartView {
    #[default]
    Both,
    Overlay,
    Spot,
    Shares,
}

pub struct ShareOverlayData<'a> {
    pub label: &'a str,
    pub samples_ms: &'a [(i64, f64)],
    pub latest_price: f64,
    pub odds_fmt: OddsFormat,
    pub tick_size: f64,
}

pub struct OracleChartData<'a> {
    pub symbol: &'a str,
    pub samples: &'a [(i64, f64)],
    pub fast_samples: &'a [(i64, f64)],
    pub fast_spot: Option<f64>,
    pub fast_wire_lag_ms: Option<i64>,
    pub now_ms: i64,
    pub window_ms: i64,
    pub primary_live_lead: bool,
    pub twap_60s: Option<f64>,
    pub price_to_beat: Option<f64>,
    pub stale: bool,
    pub last_timestamp_ms: i64,
    pub show_twap: bool,
    pub show_fast_lead: bool,
    pub show_cl_ref: bool,
    pub show_lag: bool,
    pub share_overlay: Option<ShareOverlayData<'a>>,
}

#[derive(Debug, Default)]
pub struct OracleChartSmoothState {
    pub initialized: bool,
    pub symbol: String,
    pub last_frame: Option<std::time::Instant>,
    pub disp_min_p: f64,
    pub disp_max_p: f64,
    pub disp_spot: f64,
    pub disp_fast_spot: Option<f64>,
    pub disp_spot_pill_y: f32,
    pub pill_y_initialized: bool,
    pub disp_green_blend: f32,
    pub share_y_initialized: bool,
    pub disp_share_min: f64,
    pub disp_share_max: f64,
    pub disp_share_pill_y: f32,
    pub share_pill_y_init: bool,
    /// TWAP overlay memoization: recomputing `compute_twap_series_60s` every frame is
    /// O(n·window) (~2.4 M float ops at 125 fps) purely because the frame loop asks
    /// for it again. Keyed on the (len, last-ts) fingerprint of the input series.
    pub twap_cache_key: u64,
    pub twap_cache: Vec<(i64, f64)>,
}

impl OracleChartSmoothState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn exp_smooth(cur: f64, target: f64, dt: f64, tau: f64) -> f64 {
    let alpha = 1.0 - (-dt / tau.max(1e-4)).exp();
    cur + (target - cur) * alpha
}

fn exp_smooth_f32(cur: f32, target: f32, dt: f32, tau: f32) -> f32 {
    let alpha = 1.0 - (-dt / tau.max(1e-4)).exp();
    cur + (target - cur) * alpha
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |x: u8, y: u8| -> u8 {
        ((x as f32) + ((y as f32) - (x as f32)) * t).round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()), mix(a.a(), b.a()))
}

fn compute_twap_series_60s(samples: &[(i64, f64)]) -> Vec<(i64, f64)> {
    let mut rolling: VecDeque<OracleSample> = VecDeque::with_capacity(64);
    let mut out = Vec::with_capacity(samples.len());
    for &(t_ms, p) in samples {
        rolling.push_back(OracleSample {
            timestamp_ms: t_ms,
            price: p,
        });
        while rolling.get(1).is_some_and(|s2| s2.timestamp_ms <= t_ms - 60_000) {
            rolling.pop_front();
        }
        if let Some(tw) = compute_twap_60s(&rolling, t_ms) {
            out.push((t_ms, tw));
        }
    }
    out
}

/// Monotonic fingerprint of a sample series: length + last timestamp is enough to
/// detect any append/replace that reaches the chart.
fn twap_cache_key(samples: &[(i64, f64)]) -> u64 {
    let last_ts = samples.last().map(|s| s.0).unwrap_or(0);
    ((samples.len() as u64) << 40) ^ (last_ts as u64)
}

#[cfg(test)]
pub(crate) static TWAP_COMPUTE_COUNT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Return the TWAP overlay series for `samples`, recomputing it only when the input
/// actually changed. `compute_twap_series_60s` is O(n·window) (~2.4 M float ops at
/// 125 fps for a 12 k-sample canvas), so recomputing every frame is not acceptable.
fn twap_series_cached(
    state: &mut OracleChartSmoothState,
    samples: &[(i64, f64)],
    show: bool,
) -> Vec<(i64, f64)> {
    if !show {
        return Vec::new();
    }
    let key = twap_cache_key(samples);
    if state.twap_cache_key == key && state.twap_cache.len() == samples.len() {
        return state.twap_cache.clone();
    }
    #[cfg(test)]
    TWAP_COMPUTE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let series = compute_twap_series_60s(samples);
    state.twap_cache_key = key;
    state.twap_cache = series.clone();
    series
}

pub fn format_oracle_usd(p: f64) -> String {
    let sign = if p < 0.0 { "-" } else { "" };
    let abs = p.abs();
    if abs >= 100.0 {
        let formatted = format!("{abs:.2}");
        let (int_part, dec_part) = formatted.split_once('.').unwrap_or((&formatted, "00"));
        let mut with_commas = String::with_capacity(int_part.len() + int_part.len() / 3);
        for (i, ch) in int_part.chars().rev().enumerate() {
            if i > 0 && i % 3 == 0 {
                with_commas.push(',');
            }
            with_commas.push(ch);
        }
        let int_commas: String = with_commas.chars().rev().collect();
        format!("{sign}${int_commas}.{dec_part}")
    } else if abs >= 1.0 {
        format!("{sign}${abs:.3}")
    } else {
        format!("{sign}${abs:.4}")
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
    compact_label: Option<&str>,
    height: f32,
) {
    let desired = vec2(ui.available_width().max(120.0), height.max(92.0));
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, CornerRadius::same(10), pal.bg_card);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
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

    // Subtle hairline horizontal & vertical gridlines + clean right Y-axis
    let last_p_preview = points.last().map(|pt| pt.p).unwrap_or(0.5);
    let live_y_badge = y_of(last_p_preview).clamp(plot.top() + 9.0, plot.bottom() - 9.0);
    let grid_steps = if plot.height() < 105.0 { 2 } else { 4 };
    for i in 0..=grid_steps {
        let frac = i as f64 / grid_steps as f64;
        let y = plot.top() + (frac as f32) * plot.height();
        let p = max_p - frac * range_p;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0, pal.grid_line),
        );
        if (y - live_y_badge).abs() >= 12.0 {
            painter.text(
                pos2(plot.right() + 8.0, y),
                egui::Align2::LEFT_CENTER,
                format_odds(Some(p), odds_fmt, tick_size),
                FontId::monospace(10.0),
                pal.text_muted,
            );
        }
        if i > 0 && i < grid_steps {
            let xv = plot.left() + (frac as f32) * plot.width();
            painter.line_segment(
                [pos2(xv, plot.top()), pos2(xv, plot.bottom())],
                Stroke::new(0.8, pal.grid_line),
            );
        }
    }

    if let Some(lbl) = compact_label {
        let galley = painter.layout_no_wrap(lbl.to_string(), FontId::monospace(10.0), pal.text_primary);
        let pill = Rect::from_min_size(
            pos2(plot.left() + 6.0, plot.top() + 4.0),
            vec2(galley.size().x + 14.0, 18.0),
        );
        painter.rect_filled(pill, CornerRadius::same(5), ThemePalette::tint(pal.bg_sub, 225));
        painter.rect_stroke(pill, CornerRadius::same(5), Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
        painter.galley(pos2(pill.left() + 7.0, pill.center().y - galley.size().y * 0.5), galley, pal.text_primary);
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

            let top_alpha = if pal.light { 28 } else { 26 };
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
                painter.add(Shape::line(poly, Stroke::new(2.0, trend_col)));
                if let Some(pt) = last_pt {
                    painter.circle_filled(pt, 5.5, ThemePalette::tint(trend_col, 48));
                    painter.circle_filled(pt, 3.2, trend_col);
                    painter.circle_stroke(pt, 3.2, Stroke::new(1.3, pal.bg_card));
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

        let px_txt = format_odds(Some(pt.p), odds_fmt, tick_size);
        let tm = chrono::DateTime::from_timestamp(pt.t, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%m/%d %H:%M").to_string())
            .unwrap_or_default();

        // TradingView-style axis pills
        let y_pill = Rect::from_min_max(pos2(plot.right() + 4.0, cy - 9.0), pos2(rect.right() - 4.0, cy + 9.0));
        painter.rect_filled(y_pill, CornerRadius::same(4), pal.bg_chip);
        painter.rect_stroke(y_pill, CornerRadius::same(4), Stroke::new(1.0, pal.border_strong), egui::StrokeKind::Inside);
        painter.text(y_pill.center(), egui::Align2::CENTER_CENTER, &px_txt, FontId::monospace(9.5), pal.text_primary);

        let x_pill = Rect::from_center_size(pos2(cx.clamp(plot.left() + 42.0, plot.right() - 42.0), rect.bottom() - 10.0), vec2(80.0, 16.0));
        painter.rect_filled(x_pill, CornerRadius::same(4), pal.bg_chip);
        painter.rect_stroke(x_pill, CornerRadius::same(4), Stroke::new(1.0, pal.border_strong), egui::StrokeKind::Inside);
        painter.text(x_pill.center(), egui::Align2::CENTER_CENTER, &tm, FontId::monospace(9.0), pal.text_primary);

        let tip = format!("{px_txt}  ·  {tm}");
        let bx = (cx + 10.0).min(plot.right() - 144.0).max(plot.left() + 6.0);
        let by = (cy - 28.0).max(plot.top() + 6.0);
        let tip_rect = Rect::from_min_size(pos2(bx, by), vec2(138.0, 22.0));
        painter.rect_filled(tip_rect, CornerRadius::same(5), pal.tooltip_bg);
        painter.rect_stroke(
            tip_rect,
            CornerRadius::same(5),
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

const LIVE_SHARE_BRIDGE_MS: i64 = 15_000;
const HISTORY_SHARE_BRIDGE_MS: i64 = 150_000;
const MIN_SHARE_SPAN: f64 = 0.18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShareSegKind {
    Live,
    HistoryStep,
}

#[derive(Debug, Clone, PartialEq)]
struct ShareSegment {
    kind: ShareSegKind,
    pts: Vec<(i64, f64)>,
}

/// Stabilize Share Y bounds around the visible data midpoint with at least [`MIN_SHARE_SPAN`]
/// (18 percentage points) so 1–2¢ spread jiggles do not blow up vertically.
fn stabilized_share_bounds(s_min: f64, s_max: f64) -> (f64, f64) {
    let mid = ((s_min + s_max) * 0.5).clamp(0.01, 0.99);
    let half = (((s_max - s_min) * 0.5) * 1.18).max(MIN_SHARE_SPAN * 0.5);
    let mut lo = mid - half;
    let mut hi = mid + half;
    if lo < 0.01 {
        hi = (hi + (0.01 - lo)).min(0.99);
        lo = 0.01;
    }
    if hi > 0.99 {
        lo = (lo - (hi - 0.99)).max(0.01);
        hi = 0.99;
    }
    if hi - lo < 0.02 {
        lo = (mid - 0.09).max(0.01);
        hi = (mid + 0.09).min(0.99);
    }
    (lo, hi)
}

/// Deterministically separate vertical pill center coordinates by at least `min_sep`
/// within `[min_y, max_y]`, preserving each pill's original index ordering.
fn resolve_pill_stack(ys: &mut [f32], min_y: f32, max_y: f32, min_sep: f32) {
    let n = ys.len();
    if n <= 1 || max_y <= min_y {
        for y in ys.iter_mut() {
            *y = y.clamp(min_y, max_y.max(min_y));
        }
        return;
    }
    let eff_sep = min_sep.min((max_y - min_y) / ((n - 1) as f32).max(1.0)).max(0.0);
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| ys[a].total_cmp(&ys[b]).then_with(|| a.cmp(&b)));

    let mut sorted: Vec<f32> = order.iter().map(|&i| ys[i].clamp(min_y, max_y)).collect();
    for i in 1..n {
        if sorted[i] < sorted[i - 1] + eff_sep {
            sorted[i] = sorted[i - 1] + eff_sep;
        }
    }
    if sorted[n - 1] > max_y {
        sorted[n - 1] = max_y;
        for i in (0..(n - 1)).rev() {
            if sorted[i] > sorted[i + 1] - eff_sep {
                sorted[i] = sorted[i + 1] - eff_sep;
            }
        }
        if sorted[0] < min_y {
            sorted[0] = min_y;
        }
    }
    for (rank, &orig_idx) in order.iter().enumerate() {
        ys[orig_idx] = sorted[rank];
    }
}

/// Build visual Share segments from mixed REST intra-slot points and high-rate live WS ticks:
/// - consecutive samples `<= 15s` apart become smooth `Live` polylines,
/// - consecutive samples `15s < dt <= 150s` apart become horizontal step-after `HistoryStep` holds
///   so sparse REST history populates the slot cleanly without fake diagonal slashes,
/// - dead gaps `> 150s` stay cleanly disconnected.
fn build_share_segments(
    samples_ms: &[(i64, f64)],
    latest_price: f64,
    view_min_t: i64,
    view_max_t: i64,
) -> Vec<ShareSegment> {
    let mut raw_seed: Option<(i64, f64)> = None;
    let mut interior: Vec<(i64, f64)> = Vec::new();
    for &(t_ms, p) in samples_ms {
        if !p.is_finite() || p <= 0.0 || p >= 1.0 {
            continue;
        }
        if t_ms <= view_min_t {
            raw_seed = Some((t_ms, p));
        } else if t_ms <= view_max_t {
            if interior.last().is_some_and(|&(pt, _)| t_ms == pt) {
                if let Some(last) = interior.last_mut() {
                    last.1 = p;
                }
            } else if interior.last().is_none_or(|&(pt, _)| t_ms > pt) {
                interior.push((t_ms, p));
            }
        }
    }

    let mut ordered: Vec<(i64, i64, f64)> = Vec::with_capacity(interior.len() + 2);
    if let Some((real_t, p)) = raw_seed
        && let Some(&(first_t, _)) = interior.first()
        && first_t - real_t <= HISTORY_SHARE_BRIDGE_MS
    {
        ordered.push((real_t, view_min_t, p));
    }
    for &(t_ms, p) in &interior {
        ordered.push((t_ms, t_ms, p));
    }
    if latest_price.is_finite() && latest_price > 0.0 && latest_price < 1.0 {
        let should_append = match ordered.last() {
            Some(&(last_real_t, _, _)) => view_max_t - last_real_t <= HISTORY_SHARE_BRIDGE_MS,
            None => true,
        };
        if should_append {
            if ordered.last().is_some_and(|&(_, disp_t, _)| view_max_t - disp_t < 400) {
                if let Some(last) = ordered.last_mut() {
                    last.0 = view_max_t;
                    last.1 = view_max_t;
                    last.2 = latest_price;
                }
            } else {
                ordered.push((view_max_t, view_max_t, latest_price));
            }
        } else {
            ordered.push((view_max_t, view_max_t, latest_price));
        }
    }

    let mut out: Vec<ShareSegment> = Vec::new();
    let Some(&(mut prev_real_t, mut prev_disp_t, mut prev_p)) = ordered.first() else {
        return out;
    };

    for &(real_t, disp_t, p) in &ordered[1..] {
        let dt = real_t - prev_real_t;
        if dt <= LIVE_SHARE_BRIDGE_MS {
            if let Some(last) = out.last_mut().filter(|s| s.kind == ShareSegKind::Live)
                && last.pts.last().is_some_and(|&(t, _)| t == prev_disp_t)
            {
                last.pts.push((disp_t, p));
            } else {
                out.push(ShareSegment {
                    kind: ShareSegKind::Live,
                    pts: vec![(prev_disp_t, prev_p), (disp_t, p)],
                });
            }
        } else if dt <= HISTORY_SHARE_BRIDGE_MS {
            let need_new = out
                .last()
                .is_none_or(|s| s.kind != ShareSegKind::HistoryStep || s.pts.last().is_none_or(|&(t, _)| t != prev_disp_t));
            if need_new {
                out.push(ShareSegment {
                    kind: ShareSegKind::HistoryStep,
                    pts: vec![(prev_disp_t, prev_p), (disp_t, prev_p), (disp_t, p)],
                });
            } else if let Some(last) = out.last_mut() {
                last.pts.push((disp_t, prev_p));
                last.pts.push((disp_t, p));
            }
        }
        prev_real_t = real_t;
        prev_disp_t = disp_t;
        prev_p = p;
    }

    out
}

pub fn draw_oracle_chart(
    ui: &mut Ui,
    lang: Lang,
    pal: ThemePalette,
    data: Option<&OracleChartData<'_>>,
    state: &mut OracleChartSmoothState,
    height: f32,
) {
    let desired = vec2(ui.available_width().max(120.0), height.max(120.0));
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, CornerRadius::same(10), pal.bg_card);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    let has_overlay = data.is_some_and(|d| d.share_overlay.is_some());
    let pad_l = if has_overlay { 42.0 } else { 12.0 };
    let pad_r = if has_overlay { 104.0 } else { 96.0 };
    let pad_t = 28.0;
    let pad_b = 22.0;
    let plot = Rect::from_min_max(
        pos2(rect.left() + pad_l, rect.top() + pad_t),
        pos2(
            (rect.right() - pad_r).max(rect.left() + pad_l + 20.0),
            (rect.bottom() - pad_b).max(rect.top() + pad_t + 30.0),
        ),
    );

    let Some(d) = data else {
        state.reset();
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            tr(
                lang,
                "Select a 5m / 15m crypto Up/Down market for Chainlink price chart",
                "请选择 5m / 15m 加密货币涨跌市场查看 Chainlink 实时币价图",
            ),
            FontId::proportional(13.0),
            pal.text_muted,
        );
        return;
    };

    let use_live_primary = d.primary_live_lead
        && d.show_fast_lead
        && d.fast_spot.is_some()
        && !d.fast_samples.is_empty();

    if d.samples.is_empty() && !use_live_primary {
        state.reset();
        let wait_msg = match d.price_to_beat {
            Some(beat) => format!(
                "{} · {} {}",
                tr(lang, "Waiting for live Chainlink RTDS ticks...", "正在等待 Chainlink 实时报价..."),
                tr(lang, "Price to Beat", "目标基准价"),
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

    let render_twap = d.show_twap && !has_overlay;
    let render_cl_ref = d.show_cl_ref && !has_overlay;
    let twap_pts: Vec<(i64, f64)> = twap_series_cached(state, d.samples, render_twap);
    let max_cl_t = d.samples.last().map(|s| s.0).unwrap_or(i64::MIN);
    let max_fast_t = d.fast_samples.last().map(|s| s.0).unwrap_or(i64::MIN);
    let max_sample_t = max_cl_t.max(max_fast_t);
    let view_max_t = if d.now_ms > 0 {
        d.now_ms.max(max_sample_t)
    } else {
        max_sample_t
    };
    let win_ms = d.window_ms.max(15_000);
    let view_min_t = view_max_t - win_ms;
    let span_t = (view_max_t - view_min_t).max(1) as f64;

    let mut raw_min_p = f64::INFINITY;
    let mut raw_max_p = f64::NEG_INFINITY;

    let mut collect_win = |src: &[(i64, f64)]| -> (Option<(i64, f64)>, Vec<(i64, f64)>) {
        let mut seed: Option<(i64, f64)> = None;
        let mut pts: Vec<(i64, f64)> = Vec::new();
        for &(t_ms, p) in src {
            if t_ms <= view_min_t {
                seed = Some((view_min_t, p));
            } else if t_ms <= view_max_t {
                raw_min_p = raw_min_p.min(p);
                raw_max_p = raw_max_p.max(p);
                pts.push((t_ms, p));
            }
        }
        if let Some((_, sp)) = seed {
            raw_min_p = raw_min_p.min(sp);
            raw_max_p = raw_max_p.max(sp);
        }
        (seed, pts)
    };

    let (cl_seed, cl_win) = if !has_overlay || !use_live_primary {
        collect_win(d.samples)
    } else {
        (None, Vec::new())
    };
    let (fast_seed, fast_win) = if d.show_fast_lead {
        collect_win(d.fast_samples)
    } else {
        (None, Vec::new())
    };
    if render_twap {
        for &(t_ms, tw) in &twap_pts {
            if t_ms >= view_min_t && t_ms <= view_max_t {
                raw_min_p = raw_min_p.min(tw);
                raw_max_p = raw_max_p.max(tw);
            }
        }
    }
    if let Some(fs) = d.fast_spot
        && d.show_fast_lead
    {
        raw_min_p = raw_min_p.min(fs);
        raw_max_p = raw_max_p.max(fs);
    }
    if let Some(&(_, p)) = d.samples.last() {
        raw_min_p = raw_min_p.min(p);
        raw_max_p = raw_max_p.max(p);
    }
    if let Some(beat) = d.price_to_beat {
        raw_min_p = raw_min_p.min(beat);
        raw_max_p = raw_max_p.max(beat);
    }
    let cl_last_price = d.samples.last().map(|s| s.1);
    let raw_spot = if use_live_primary {
        d.fast_spot.or(cl_last_price).unwrap_or(0.0)
    } else {
        cl_last_price.or(d.fast_spot).unwrap_or(0.0)
    };
    if !raw_min_p.is_finite() || !raw_max_p.is_finite() {
        raw_min_p = raw_spot - 1.0;
        raw_max_p = raw_spot + 1.0;
    }

    let target_raw_span = (raw_max_p - raw_min_p).max(1e-6);
    let min_pad = (raw_max_p.abs() * 0.0005).max(0.0005);
    let span_pad = ((raw_max_p - raw_min_p) * 0.18).max(min_pad);
    let target_min_p = raw_min_p - span_pad;
    let target_max_p = raw_max_p + span_pad;

    let target_green = match d.price_to_beat {
        Some(b) if raw_spot >= b => 1.0_f32,
        _ => 0.0_f32,
    };

    let now = std::time::Instant::now();
    let dt = state
        .last_frame
        .map(|t0| now.saturating_duration_since(t0).as_secs_f64().clamp(0.001, 0.25))
        .unwrap_or(0.016);
    state.last_frame = Some(now);

    if !state.initialized || state.symbol != d.symbol {
        state.initialized = true;
        state.symbol = d.symbol.to_string();
        state.disp_min_p = target_min_p;
        state.disp_max_p = target_max_p;
        state.disp_spot = raw_spot;
        state.disp_fast_spot = d.fast_spot;
        state.pill_y_initialized = false;
        state.disp_green_blend = target_green;
        state.share_y_initialized = false;
        state.share_pill_y_init = false;
    } else {
        // Track live price instantaneously with ZERO artificial smoothing delay.
        state.disp_spot = raw_spot;
        state.disp_fast_spot = d.fast_spot;
        let cur_span = (state.disp_max_p - state.disp_min_p).max(target_raw_span);
        if target_min_p < state.disp_min_p {
            state.disp_min_p = exp_smooth(state.disp_min_p, target_min_p, dt, 0.12);
        } else if (target_min_p - state.disp_min_p) > cur_span * 0.30 {
            state.disp_min_p = exp_smooth(state.disp_min_p, target_min_p, dt, 0.45);
        }
        if target_max_p > state.disp_max_p {
            state.disp_max_p = exp_smooth(state.disp_max_p, target_max_p, dt, 0.12);
        } else if (state.disp_max_p - target_max_p) > cur_span * 0.30 {
            state.disp_max_p = exp_smooth(state.disp_max_p, target_max_p, dt, 0.45);
        }
        state.disp_green_blend = exp_smooth_f32(state.disp_green_blend, target_green, dt as f32, 0.18);
    }

    let min_p = state.disp_min_p;
    let max_p = state.disp_max_p;
    let range_p = (max_p - min_p).max(1e-6);

    let y_of = |p: f64| -> f32 {
        plot.top() + (((max_p - p) / range_p) as f32) * plot.height()
    };
    let x_of_t = |t_ms: i64| -> f32 {
        let frac = ((t_ms - view_min_t) as f64 / span_t).clamp(0.0, 1.0) as f32;
        plot.left() + frac * plot.width()
    };

    let spot = state.disp_spot;
    let spot_col = if d.stale && !use_live_primary {
        pal.text_muted
    } else if d.price_to_beat.is_some() {
        lerp_color(pal.red, pal.green, state.disp_green_blend)
    } else {
        pal.blue
    };

    // Pre-compute Share overlay scale & targets so BTC / BEAT / SHARE right-edge pills
    // never overlap each other or right-axis tick labels.
    struct PreparedShareOverlay<'a> {
        label: &'a str,
        latest_price: f64,
        odds_fmt: OddsFormat,
        tick_size: f64,
        lo: f64,
        hi: f64,
        s_rng: f64,
        raw_s_lo: f64,
        raw_s_hi: f64,
        segments: Vec<ShareSegment>,
        target_pill_y: f32,
        dot_y: f32,
    }

    let min_pill_y = plot.top() + 9.0;
    let max_pill_y = (plot.bottom() - 9.0).max(min_pill_y);

    let mut prep_share = d.share_overlay.as_ref().map(|ov| {
        let share_segments = build_share_segments(
            ov.samples_ms,
            ov.latest_price,
            view_min_t,
            view_max_t,
        );
        let mut s_min = ov.latest_price;
        let mut s_max = ov.latest_price;
        for seg in &share_segments {
            for &(_, p) in &seg.pts {
                s_min = s_min.min(p);
                s_max = s_max.max(p);
            }
        }
        let (raw_s_lo, raw_s_hi) = stabilized_share_bounds(s_min, s_max);
        let raw_s_rng = (raw_s_hi - raw_s_lo).max(0.02);

        if !state.share_y_initialized {
            state.share_y_initialized = true;
            state.disp_share_min = raw_s_lo;
            state.disp_share_max = raw_s_hi;
        } else {
            let cur_s_rng = (state.disp_share_max - state.disp_share_min).max(raw_s_rng);
            if raw_s_lo < state.disp_share_min {
                state.disp_share_min = exp_smooth(state.disp_share_min, raw_s_lo, dt, 0.12);
            } else if (raw_s_lo - state.disp_share_min) > cur_s_rng * 0.25 {
                state.disp_share_min = exp_smooth(state.disp_share_min, raw_s_lo, dt, 0.45);
            }
            if raw_s_hi > state.disp_share_max {
                state.disp_share_max = exp_smooth(state.disp_share_max, raw_s_hi, dt, 0.12);
            } else if (state.disp_share_max - raw_s_hi) > cur_s_rng * 0.25 {
                state.disp_share_max = exp_smooth(state.disp_share_max, raw_s_hi, dt, 0.45);
            }
        }
        let lo = state.disp_share_min;
        let hi = state.disp_share_max;
        let s_rng = (hi - lo).max(0.02);
        let dot_y = (plot.top() + (((hi - ov.latest_price) / s_rng) as f32) * plot.height())
            .clamp(plot.top() + 2.0, plot.bottom() - 2.0);
        PreparedShareOverlay {
            label: ov.label,
            latest_price: ov.latest_price,
            odds_fmt: ov.odds_fmt,
            tick_size: ov.tick_size,
            lo,
            hi,
            s_rng,
            raw_s_lo,
            raw_s_hi,
            segments: share_segments,
            target_pill_y: dot_y.clamp(min_pill_y, max_pill_y),
            dot_y,
        }
    });
    if prep_share.is_none() {
        state.share_y_initialized = false;
        state.share_pill_y_init = false;
    }

    let raw_spot_y = y_of(spot).clamp(min_pill_y, max_pill_y);
    let raw_beat_y = d.price_to_beat.map(|b| y_of(b).clamp(min_pill_y, max_pill_y));
    let mut stack_ys: Vec<f32> = Vec::with_capacity(3);
    stack_ys.push(raw_spot_y); // idx 0: BTC spot
    let beat_stack_idx = if let Some(by) = raw_beat_y {
        stack_ys.push(by);
        Some(stack_ys.len() - 1)
    } else {
        None
    };
    let share_stack_idx = if let Some(ref ps) = prep_share {
        stack_ys.push(ps.target_pill_y);
        Some(stack_ys.len() - 1)
    } else {
        None
    };
    resolve_pill_stack(&mut stack_ys, min_pill_y, max_pill_y, 20.0);

    let target_spot_pill_y = stack_ys[0];
    let beat_pill_y = beat_stack_idx.map(|i| stack_ys[i]);
    if !state.pill_y_initialized {
        state.disp_spot_pill_y = target_spot_pill_y;
        state.pill_y_initialized = true;
    } else {
        state.disp_spot_pill_y = exp_smooth_f32(state.disp_spot_pill_y, target_spot_pill_y, dt as f32, 0.10);
    }
    let spot_pill_y = state.disp_spot_pill_y;

    let mut share_pill_y_val: Option<f32> = None;
    if let (Some(idx), Some(ps)) = (share_stack_idx, prep_share.as_mut()) {
        ps.target_pill_y = stack_ys[idx];
        if !state.share_pill_y_init {
            state.disp_share_pill_y = ps.target_pill_y;
            state.share_pill_y_init = true;
        } else {
            state.disp_share_pill_y = exp_smooth_f32(state.disp_share_pill_y, ps.target_pill_y, dt as f32, 0.10);
        }
        share_pill_y_val = Some(state.disp_share_pill_y);
    }

    let grid_rows = if plot.height() < 115.0 { 2 } else { 4 };
    painter.line_segment(
        [pos2(plot.right(), plot.top()), pos2(plot.right(), plot.bottom())],
        Stroke::new(1.0, pal.grid_line),
    );
    if has_overlay {
        painter.line_segment(
            [pos2(plot.left(), plot.top()), pos2(plot.left(), plot.bottom())],
            Stroke::new(1.0, pal.grid_line),
        );
    }
    for i in 0..=grid_rows {
        let frac = i as f64 / grid_rows as f64;
        let y = plot.top() + (frac as f32) * plot.height();
        let p = max_p - frac * range_p;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(0.8, pal.grid_line),
        );
        let skip_right = beat_pill_y.is_some_and(|by| (y - by).abs() < 16.5)
            || share_pill_y_val.is_some_and(|sy| (y - sy).abs() < 16.5)
            || (y - spot_pill_y).abs() < 16.5;
        if !skip_right {
            painter.text(
                pos2(plot.right() + 8.0, y),
                egui::Align2::LEFT_CENTER,
                format_oracle_usd(p),
                FontId::monospace(10.0),
                pal.text_muted,
            );
        }
        if i > 0 && i < grid_rows {
            let xv = plot.left() + (frac as f32) * plot.width();
            painter.line_segment(
                [pos2(xv, plot.top()), pos2(xv, plot.bottom())],
                Stroke::new(0.6, pal.grid_line),
            );
        }
    }

    if let Some(beat) = d.price_to_beat {
        let y = y_of(beat).clamp(plot.top() + 2.0, plot.bottom() - 2.0);
        let beat_col = pal.amber;
        let dash_col = ThemePalette::tint(beat_col, if has_overlay { 165 } else { 200 });
        let mut x = plot.left();
        while x < plot.right() {
            let x_end = (x + 6.0).min(plot.right());
            painter.line_segment([pos2(x, y), pos2(x_end, y)], Stroke::new(1.3, dash_col));
            x += 11.0;
        }

        let tag_txt = tr(lang, "BEAT", "基准价");
        let tag_above = y - plot.top() > 16.0;
        let tag_y = if tag_above { y - 7.0 } else { y + 8.0 };
        painter.text(
            pos2(plot.left() + 6.0, tag_y),
            egui::Align2::LEFT_CENTER,
            tag_txt,
            FontId::monospace(8.5),
            ThemePalette::tint(beat_col, 210),
        );

        let by = beat_pill_y.unwrap_or(y);
        if (by - y).abs() > 2.5 {
            painter.line_segment(
                [pos2(plot.right(), y), pos2(plot.right() + 4.0, by)],
                Stroke::new(1.0, ThemePalette::tint(beat_col, 140)),
            );
        }
        let badge = Rect::from_min_max(
            pos2(plot.right() + 4.0, by - 8.5),
            pos2(rect.right() - 4.0, by + 8.5),
        );
        painter.rect_filled(badge, CornerRadius::same(5), beat_col);
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            format!("BEAT {}", format_oracle_usd(beat)),
            FontId::monospace(8.8),
            ThemePalette::contrast_on(beat_col),
        );
    }

    let build_screen_pts = |seed: Option<(i64, f64)>, win: &[(i64, f64)], flush_spot: Option<f64>| -> Vec<Pos2> {
        let mut pts = Vec::with_capacity(win.len() + 2);
        if let Some((t0, p0)) = seed {
            pts.push(pos2(x_of_t(t0), y_of(p0)));
        }
        for &(t_ms, p) in win {
            pts.push(pos2(x_of_t(t_ms), y_of(p)));
        }
        if let Some(sp) = flush_spot {
            let end_pt = pos2(x_of_t(view_max_t), y_of(sp));
            if pts.is_empty() {
                pts.push(pos2(plot.left(), end_pt.y));
            }
            if pts.last().is_some_and(|prev| (end_pt.x - prev.x).abs() < 0.75) {
                if let Some(last) = pts.last_mut() {
                    *last = end_pt;
                }
            } else {
                pts.push(end_pt);
            }
        }
        pts
    };

    let primary_pts = if use_live_primary {
        build_screen_pts(fast_seed, &fast_win, Some(spot))
    } else {
        build_screen_pts(cl_seed, &cl_win, Some(spot))
    };

    if primary_pts.len() >= 2 {
        let mut mesh = Mesh::default();
        mesh.vertices.reserve(primary_pts.len() * 2);
        mesh.indices.reserve((primary_pts.len() - 1) * 6);
        let top_alpha = if has_overlay {
            11
        } else if pal.light {
            20
        } else {
            22
        };
        let top_fill = ThemePalette::tint(spot_col, top_alpha);
        let bot_fill = ThemePalette::tint(spot_col, 0);
        for (i, &scr) in primary_pts.iter().enumerate() {
            let top_idx = (i * 2) as u32;
            let bot_idx = top_idx + 1;
            mesh.colored_vertex(scr, top_fill);
            mesh.colored_vertex(pos2(scr.x, plot.bottom()), bot_fill);
            if i > 0 {
                let prev_top = top_idx - 2;
                let prev_l = top_idx - 1;
                mesh.add_triangle(prev_top, top_idx, bot_idx);
                mesh.add_triangle(prev_top, bot_idx, prev_l);
            }
        }
        painter.add(Shape::mesh(mesh));
        painter.add(Shape::line(primary_pts.clone(), Stroke::new(2.0, spot_col)));
    } else if let Some(&pt) = primary_pts.first() {
        painter.line_segment(
            [pos2(plot.left(), pt.y), pos2(plot.right(), pt.y)],
            Stroke::new(1.4, ThemePalette::tint(spot_col, 150)),
        );
    }

    let twap_scr: Vec<Pos2> = twap_pts
        .iter()
        .filter(|&&(t, _)| t >= view_min_t && t <= view_max_t)
        .map(|&(t, p)| pos2(x_of_t(t), y_of(p)))
        .collect();
    if render_twap && twap_scr.len() >= 2 {
        painter.add(Shape::line(twap_scr, Stroke::new(1.3, pal.ind_sma)));
    }

    if use_live_primary {
        if render_cl_ref {
            let cl_ref_pts = build_screen_pts(cl_seed, &cl_win, None);
            if cl_ref_pts.len() >= 2 {
                let cl_col = ThemePalette::tint(pal.blue, 200);
                painter.add(Shape::line(cl_ref_pts.clone(), Stroke::new(1.3, cl_col)));
                if let Some(&ct) = cl_ref_pts.last() {
                    painter.circle_filled(ct, 3.8, ThemePalette::tint(pal.blue, 50));
                    painter.circle_filled(ct, 2.2, cl_col);
                }
            }
        }
    } else if render_cl_ref && d.show_fast_lead {
        let fast_ref_pts = build_screen_pts(fast_seed, &fast_win, state.disp_fast_spot);
        if fast_ref_pts.len() >= 2 {
            let fast_col = ThemePalette::tint(pal.blue, 215);
            painter.add(Shape::line(fast_ref_pts.clone(), Stroke::new(1.3, fast_col)));
            if let Some(&ft) = fast_ref_pts.last() {
                painter.circle_filled(ft, 4.2, ThemePalette::tint(pal.blue, 55));
                painter.circle_filled(ft, 2.4, fast_col);
            }
        }
    }

    // Dual-axis Share Price overlay: calm left ¢ scale ticks + step-hold history + live WS curve
    // + unified right-gutter pill stacked cleanly alongside BTC and Beat.
    let mut share_y_mapper: Option<(f64, f64, OddsFormat, f64)> = None;
    let mut target_share_range: Option<(f64, f64, f32)> = None;
    if let Some(ref ps) = prep_share {
        let hi = ps.hi;
        let lo = ps.lo;
        let s_rng = ps.s_rng;
        share_y_mapper = Some((lo, s_rng, ps.odds_fmt, ps.tick_size));
        target_share_range = Some((ps.raw_s_lo, ps.raw_s_hi, ps.target_pill_y));

        let y_share = |p: f64| -> f32 {
            plot.top() + (((hi - p) / s_rng) as f32) * plot.height()
        };

        painter.text(
            pos2(plot.left() - 6.0, rect.top() + 12.0),
            egui::Align2::RIGHT_CENTER,
            tr(lang, "SHARES", "份额"),
            FontId::monospace(8.5),
            ThemePalette::tint(pal.cyan, 195),
        );
        painter.text(
            pos2(plot.right() + 8.0, rect.top() + 12.0),
            egui::Align2::LEFT_CENTER,
            d.symbol.to_uppercase(),
            FontId::monospace(8.5),
            pal.text_muted,
        );

        let axis_col = ThemePalette::tint(pal.cyan, 165);
        for i in 0..=grid_rows {
            let frac = i as f64 / grid_rows as f64;
            let y = plot.top() + (frac as f32) * plot.height();
            let p = hi - frac * s_rng;
            painter.text(
                pos2(plot.left() - 6.0, y),
                egui::Align2::RIGHT_CENTER,
                format_odds(Some(p), ps.odds_fmt, ps.tick_size),
                FontId::monospace(9.5),
                axis_col,
            );
        }

        // Subtle 50¢ fair-odds parity guide when inside visible Share scale
        if 0.50 > lo && 0.50 < hi {
            let y50 = y_share(0.50);
            let mut xd = plot.left();
            let dcol = ThemePalette::tint(pal.cyan, 36);
            while xd < plot.right() {
                let xe = (xd + 3.0).min(plot.right());
                painter.line_segment([pos2(xd, y50), pos2(xe, y50)], Stroke::new(1.0, dcol));
                xd += 10.0;
            }
        }

        let total_share_span_ms = ps
            .segments
            .iter()
            .filter_map(|seg| Some((seg.pts.first()?.0, seg.pts.last()?.0)))
            .fold((i64::MAX, i64::MIN), |(mn, mx), (a, b)| (mn.min(a), mx.max(b)));
        let short_stub = total_share_span_ms.1.saturating_sub(total_share_span_ms.0) < 4_000;

        if ps.segments.is_empty() || short_stub {
            // Brand-new slot (<4s of history): draw calm horizontal dotted parity guide instead of a 10px stub
            let y_cur = ps.dot_y;
            let mut xd = plot.left();
            let dcol = ThemePalette::tint(pal.cyan, 115);
            while xd < plot.right() {
                let xe = (xd + 4.0).min(plot.right());
                painter.line_segment([pos2(xd, y_cur), pos2(xe, y_cur)], Stroke::new(1.4, dcol));
                xd += 9.0;
            }
        } else {
            for seg in &ps.segments {
                let poly: Vec<Pos2> = seg
                    .pts
                    .iter()
                    .map(|&(tm, pm)| pos2(x_of_t(tm), y_share(pm)))
                    .collect();
                if poly.len() >= 2 {
                    match seg.kind {
                        ShareSegKind::HistoryStep => {
                            painter.add(Shape::line(
                                poly,
                                Stroke::new(1.5, ThemePalette::tint(pal.cyan, 145)),
                            ));
                        }
                        ShareSegKind::Live => {
                            painter.add(Shape::line(
                                poly.clone(),
                                Stroke::new(3.6, ThemePalette::tint(pal.bg_card, 185)),
                            ));
                            painter.add(Shape::line(poly, Stroke::new(2.0, pal.cyan)));
                        }
                    }
                }
            }
        }

        let end_pt = pos2(x_of_t(view_max_t), ps.dot_y);
        painter.circle_filled(end_pt, 5.2, ThemePalette::tint(pal.cyan, 55));
        painter.circle_filled(end_pt, 3.0, pal.cyan);
        painter.circle_stroke(end_pt, 3.0, Stroke::new(1.2, pal.bg_card));

        let sp_y = share_pill_y_val.unwrap_or(ps.target_pill_y);
        if (sp_y - ps.dot_y).abs() > 2.5 {
            painter.line_segment(
                [pos2(plot.right(), ps.dot_y), pos2(plot.right() + 4.0, sp_y)],
                Stroke::new(1.0, ThemePalette::tint(pal.cyan, 150)),
            );
        }
        let max_tag = if polydesk::translate::contains_cjk(ps.label) { 2 } else { 4 };
        let short_tag: String = ps.label.chars().take(max_tag).collect::<String>().to_uppercase();
        let pill_txt = format!(
            "{short_tag} {}",
            format_odds(Some(ps.latest_price), ps.odds_fmt, ps.tick_size)
        );
        let sh_badge = Rect::from_min_max(
            pos2(plot.right() + 4.0, sp_y - 8.5),
            pos2(rect.right() - 4.0, sp_y + 8.5),
        );
        painter.rect_filled(sh_badge, CornerRadius::same(5), pal.cyan);
        painter.text(
            sh_badge.center(),
            egui::Align2::CENTER_CENTER,
            pill_txt,
            FontId::monospace(8.9),
            ThemePalette::contrast_on(pal.cyan),
        );
    }

    if let Some(&last_pt) = primary_pts.last() {
        painter.circle_filled(last_pt, 5.5, ThemePalette::tint(spot_col, 50));
        painter.circle_filled(last_pt, 3.2, spot_col);
        painter.circle_stroke(last_pt, 3.2, Stroke::new(1.2, pal.bg_card));

        if (spot_pill_y - last_pt.y).abs() > 2.5 {
            painter.line_segment(
                [pos2(plot.right(), last_pt.y), pos2(plot.right() + 4.0, spot_pill_y)],
                Stroke::new(1.0, ThemePalette::tint(spot_col, 145)),
            );
        }
        let s_badge = Rect::from_min_max(
            pos2(plot.right() + 4.0, spot_pill_y - 8.5),
            pos2(rect.right() - 4.0, spot_pill_y + 8.5),
        );
        painter.rect_filled(s_badge, CornerRadius::same(5), spot_col);
        painter.text(
            s_badge.center(),
            egui::Align2::CENTER_CENTER,
            format_oracle_usd(raw_spot),
            FontId::monospace(9.1),
            ThemePalette::contrast_on(spot_col),
        );
    }

    let mut legend_parts: Vec<(String, Color32, bool)> = Vec::new();
    let lag_suffix = if use_live_primary {
        d.fast_wire_lag_ms
            .map(|ms| format!(" ({ms}ms)"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    if has_overlay {
        legend_parts.push((
            format!("{} {}{}", d.symbol.to_uppercase(), format_oracle_usd(raw_spot), lag_suffix),
            spot_col,
            true,
        ));
        if let Some(ref ov) = d.share_overlay {
            legend_parts.push((
                format!("{} {}", ov.label, format_odds(Some(ov.latest_price), ov.odds_fmt, ov.tick_size)),
                pal.cyan,
                true,
            ));
        }
        if let Some(beat) = d.price_to_beat {
            legend_parts.push((format!("BEAT {}", format_oracle_usd(beat)), pal.amber, false));
        }
    } else {
        if use_live_primary {
            legend_parts.push((
                format!("LIVE {} {}{}", d.symbol.to_uppercase(), format_oracle_usd(raw_spot), lag_suffix),
                spot_col,
                true,
            ));
            if render_cl_ref
                && let Some(cl_p) = cl_last_price
            {
                legend_parts.push((format!("CL {}", format_oracle_usd(cl_p)), pal.blue, true));
            }
        } else {
            legend_parts.push((
                format!("CL {} {}", d.symbol.to_uppercase(), format_oracle_usd(raw_spot)),
                spot_col,
                true,
            ));
            if d.show_fast_lead
                && let Some(fs) = d.fast_spot
            {
                legend_parts.push((format!("FAST {}", format_oracle_usd(fs)), pal.blue, true));
            }
        }
        if let Some(beat) = d.price_to_beat {
            legend_parts.push((format!("BEAT {}", format_oracle_usd(beat)), pal.amber, false));
        }
        if render_twap
            && let Some(tw) = d.twap_60s
        {
            legend_parts.push((format!("TWAP60 {}", format_oracle_usd(tw)), pal.ind_sma, true));
        }
        if d.stale {
            legend_parts.push(("STALE".into(), pal.red_text, false));
        } else if d.show_lag && d.last_timestamp_ms > 0 {
            let ref_now = if d.now_ms > 0 {
                d.now_ms
            } else {
                chrono::Utc::now().timestamp_millis()
            };
            let age_ms = (ref_now - d.last_timestamp_ms).max(0);
            let lag_s = (age_ms / 100) as f64 * 0.1;
            let lag_prefix = if use_live_primary { "CL LAG" } else { "LAG" };
            legend_parts.push((format!("{lag_prefix} {lag_s:.1}s"), pal.text_muted, false));
        }
    }

    let mut lx = plot.left() + 6.0;
    let legend_y = rect.top() + 7.0;
    for (txt, col, show_dot) in legend_parts {
        let dot_w = if show_dot { 9.0 } else { 0.0 };
        let galley = painter.layout_no_wrap(txt, FontId::monospace(9.5), col);
        let w = dot_w + galley.size().x;
        if lx + w > plot.right() - 4.0 {
            break;
        }
        if show_dot {
            painter.circle_filled(pos2(lx + 3.0, legend_y + 5.0), 2.8, col);
        }
        painter.galley(pos2(lx + dot_w, legend_y), galley, col);
        lx += w + 14.0;
    }

    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let t_lbl = view_min_t + (frac * span_t).round() as i64;
        let dt_lbl = chrono::DateTime::from_timestamp_millis(t_lbl)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        let x = (plot.left() + (frac as f32) * plot.width())
            .clamp(plot.left() + 24.0, plot.right() - 24.0);
        painter.text(
            pos2(x, rect.bottom() - 10.0),
            egui::Align2::CENTER_CENTER,
            dt_lbl,
            FontId::monospace(9.5),
            pal.text_muted,
        );
    }

    let hover_src = if use_live_primary && !d.fast_samples.is_empty() {
        d.fast_samples
    } else {
        d.samples
    };
    if let Some(hover) = response.hover_pos()
        && plot.contains(hover)
        && !hover_src.is_empty()
    {
        let target_t =
            view_min_t + ((((hover.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64) * span_t).round() as i64;
        let idx = hover_src
            .binary_search_by_key(&target_t, |pt| pt.0)
            .unwrap_or_else(|ins| {
                if ins == 0 {
                    0
                } else if ins >= hover_src.len() {
                    hover_src.len() - 1
                } else if (hover_src[ins].0 - target_t).abs() < (target_t - hover_src[ins - 1].0).abs() {
                    ins
                } else {
                    ins - 1
                }
            });
        let (st, sp) = hover_src[idx];
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
        painter.circle_filled(pos2(cx, cy), 3.5, spot_col);

        let mut tip = format!("{} · ", format_oracle_usd(sp));
        let y_pill = Rect::from_min_max(
            pos2(plot.right() + 4.0, cy.clamp(plot.top() + 9.0, plot.bottom() - 9.0) - 9.0),
            pos2(rect.right() - 4.0, cy.clamp(plot.top() + 9.0, plot.bottom() - 9.0) + 9.0),
        );
        painter.rect_filled(y_pill, CornerRadius::same(4), pal.bg_chip);
        painter.rect_stroke(y_pill, CornerRadius::same(4), Stroke::new(1.0, pal.border_strong), egui::StrokeKind::Inside);
        painter.text(y_pill.center(), egui::Align2::CENTER_CENTER, format_oracle_usd(sp), FontId::monospace(9.0), pal.text_primary);

        if let Some(ref ov) = d.share_overlay
            && let Some((lo, s_rng, s_fmt, s_tick)) = share_y_mapper
        {
            let sh_p = ov
                .samples_ms
                .iter()
                .rfind(|&&(ts, _)| ts <= st)
                .map(|&(_, p)| p)
                .unwrap_or(ov.latest_price);
            let sy_h = plot.top() + ((((lo + s_rng) - sh_p) / s_rng) as f32) * plot.height();
            painter.circle_filled(pos2(cx, sy_h.clamp(plot.top(), plot.bottom())), 3.5, pal.cyan);
            let sh_txt = format_odds(Some(sh_p), s_fmt, s_tick);
            tip.push_str(&format!("{} {} · ", ov.label, sh_txt));
        }

        let tm = chrono::DateTime::from_timestamp_millis(st)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        tip.push_str(&tm);
        let tip_galley = painter.layout_no_wrap(tip, FontId::monospace(10.0), pal.tooltip_fg);
        let tw = (tip_galley.size().x + 18.0).max(148.0);
        let bx = (cx + 10.0).min((plot.right() - tw - 4.0).max(plot.left() + 6.0)).max(plot.left() + 6.0);
        let by = (cy - 28.0).max(plot.top() + 6.0);
        let tip_rect = Rect::from_min_size(pos2(bx, by), vec2(tw, 24.0));
        painter.rect_filled(tip_rect, CornerRadius::same(8), pal.tooltip_bg);
        painter.rect_stroke(
            tip_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, pal.border_strong),
            egui::StrokeKind::Inside,
        );
        painter.galley(
            pos2(tip_rect.left() + 9.0, tip_rect.center().y - tip_galley.size().y * 0.5),
            tip_galley,
            pal.tooltip_fg,
        );
    }

    let eps_p = (range_p * 0.0008).max(1e-6);
    let fast_diff = match (state.disp_fast_spot, d.fast_spot) {
        (Some(a), Some(b)) => (a - b).abs() > eps_p,
        _ => false,
    };
    let cur_span_check = (state.disp_max_p - state.disp_min_p).max(target_raw_span);
    let share_animating = target_share_range.is_some_and(|(raw_s_lo, raw_s_hi, target_s_pill_y)| {
        let s_span_check = (state.disp_share_max - state.disp_share_min).max(raw_s_hi - raw_s_lo);
        (state.disp_share_pill_y - target_s_pill_y).abs() > 0.4
            || (raw_s_lo < state.disp_share_min && (state.disp_share_min - raw_s_lo) > 0.0005)
            || (raw_s_hi > state.disp_share_max && (raw_s_hi - state.disp_share_max) > 0.0005)
            || (raw_s_lo - state.disp_share_min) > s_span_check * 0.30
            || (state.disp_share_max - raw_s_hi) > s_span_check * 0.30
    });
    let animating = (state.disp_spot - raw_spot).abs() > eps_p
        || fast_diff
        || share_animating
        || (state.disp_spot_pill_y - target_spot_pill_y).abs() > 0.4
        || (state.disp_green_blend - target_green).abs() > 0.02
        || (target_min_p < state.disp_min_p && (state.disp_min_p - target_min_p) > eps_p)
        || (target_max_p > state.disp_max_p && (target_max_p - state.disp_max_p) > eps_p)
        || (target_min_p - state.disp_min_p) > cur_span_check * 0.30
        || (state.disp_max_p - target_max_p) > cur_span_check * 0.30;
    if animating {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    } else if d.now_ms > 0 {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Time-synchronized share price subplot for `Both` mode: uses identical horizontal
/// margins (`pad_l = 12`, `pad_r = 96`) and `[view_min_t, view_max_t]` as `draw_oracle_chart`
/// so BTC price moves and Polymarket share moves align pixel-for-pixel vertically.
#[allow(clippy::too_many_arguments)]
pub fn draw_synced_share_chart(
    ui: &mut Ui,
    lang: Lang,
    pal: ThemePalette,
    badge_label: Option<&str>,
    samples_ms: &[(i64, f64)],
    latest_price: f64,
    view_min_t: i64,
    view_max_t: i64,
    odds_fmt: OddsFormat,
    tick_size: f64,
    position: Option<&PaperPosition>,
    all_rules: &[AutomationRule],
    asset_id: &str,
    height: f32,
) {
    let desired = vec2(ui.available_width().max(120.0), height.max(88.0));
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, CornerRadius::same(10), pal.bg_card);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    let pad_l = 12.0;
    let pad_r = 96.0;
    let pad_t = 14.0;
    let pad_b = 20.0;
    let plot = Rect::from_min_max(
        pos2(rect.left() + pad_l, rect.top() + pad_t),
        pos2(
            (rect.right() - pad_r).max(rect.left() + pad_l + 20.0),
            (rect.bottom() - pad_b).max(rect.top() + pad_t + 24.0),
        ),
    );

    let span_t = (view_max_t - view_min_t).max(1) as f64;
    let x_of_t = |t_ms: i64| -> f32 {
        let frac = ((t_ms - view_min_t) as f64 / span_t).clamp(0.0, 1.0) as f32;
        plot.left() + frac * plot.width()
    };

    let share_segments = build_share_segments(samples_ms, latest_price, view_min_t, view_max_t);
    let mut min_p = latest_price;
    let mut max_p = latest_price;
    for seg in &share_segments {
        for &(_, p) in &seg.pts {
            min_p = min_p.min(p);
            max_p = max_p.max(p);
        }
    }
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

    let (min_p, max_p) = stabilized_share_bounds(min_p, max_p);
    let range_p = (max_p - min_p).max(0.02);

    let y_of = |p: f64| -> f32 {
        plot.top() + (((max_p - p) / range_p) as f32) * plot.height()
    };

    let first_p = share_segments
        .first()
        .and_then(|s| s.pts.first())
        .map(|s| s.1)
        .unwrap_or(latest_price);
    let trend_col = if latest_price >= first_p { pal.green } else { pal.red };

    let live_y = y_of(latest_price).clamp(plot.top() + 8.5, plot.bottom() - 8.5);
    let grid_rows = if plot.height() < 95.0 { 2 } else { 3 };
    painter.line_segment(
        [pos2(plot.right(), plot.top()), pos2(plot.right(), plot.bottom())],
        Stroke::new(1.0, pal.grid_line),
    );
    for i in 0..=grid_rows {
        let frac = i as f64 / grid_rows as f64;
        let y = plot.top() + (frac as f32) * plot.height();
        let p = max_p - frac * range_p;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(0.8, pal.grid_line),
        );
        if (y - live_y).abs() >= 16.5 {
            painter.text(
                pos2(plot.right() + 8.0, y),
                egui::Align2::LEFT_CENTER,
                format_odds(Some(p), odds_fmt, tick_size),
                FontId::monospace(9.5),
                pal.text_muted,
            );
        }
        if i > 0 && i < grid_rows {
            let xv = plot.left() + (frac as f32) * plot.width();
            painter.line_segment(
                [pos2(xv, plot.top()), pos2(xv, plot.bottom())],
                Stroke::new(0.6, pal.grid_line),
            );
        }
    }

    let total_span_ms = share_segments
        .iter()
        .filter_map(|seg| Some((seg.pts.first()?.0, seg.pts.last()?.0)))
        .fold((i64::MAX, i64::MIN), |(mn, mx), (a, b)| (mn.min(a), mx.max(b)));
    let short_stub = total_span_ms.1.saturating_sub(total_span_ms.0) < 4_000;

    let top_fill = ThemePalette::tint(trend_col, if pal.light { 20 } else { 22 });
    let bot_fill = ThemePalette::tint(trend_col, 0);
    if share_segments.is_empty() || short_stub {
        let y_cur = y_of(latest_price).clamp(plot.top() + 2.0, plot.bottom() - 2.0);
        let mut xd = plot.left();
        let dcol = ThemePalette::tint(trend_col, 115);
        while xd < plot.right() {
            let xe = (xd + 4.0).min(plot.right());
            painter.line_segment([pos2(xd, y_cur), pos2(xe, y_cur)], Stroke::new(1.4, dcol));
            xd += 9.0;
        }
    } else {
        for seg in &share_segments {
            let poly: Vec<Pos2> = seg
                .pts
                .iter()
                .map(|&(tm, pm)| pos2(x_of_t(tm), y_of(pm)))
                .collect();
            if poly.len() >= 2 {
                match seg.kind {
                    ShareSegKind::HistoryStep => {
                        painter.add(Shape::line(
                            poly,
                            Stroke::new(1.5, ThemePalette::tint(trend_col, 145)),
                        ));
                    }
                    ShareSegKind::Live => {
                        let mut mesh = Mesh::default();
                        mesh.vertices.reserve(poly.len() * 2);
                        mesh.indices.reserve((poly.len() - 1) * 6);
                        for (i, &scr) in poly.iter().enumerate() {
                            let top_idx = (i * 2) as u32;
                            let bot_idx = top_idx + 1;
                            mesh.colored_vertex(scr, top_fill);
                            mesh.colored_vertex(pos2(scr.x, plot.bottom()), bot_fill);
                            if i > 0 {
                                let prev_top = top_idx - 2;
                                let prev_l = top_idx - 1;
                                mesh.add_triangle(prev_top, top_idx, bot_idx);
                                mesh.add_triangle(prev_top, bot_idx, prev_l);
                            }
                        }
                        painter.add(Shape::mesh(mesh));
                        painter.add(Shape::line(poly, Stroke::new(2.0, trend_col)));
                    }
                }
            }
        }
    }
    let end_pt = pos2(x_of_t(view_max_t), y_of(latest_price));

    let draw_dashed = |p: f64, col: Color32| {
        let y = y_of(p);
        if y < plot.top() || y > plot.bottom() {
            return;
        }
        let dcol = ThemePalette::tint(col, 150);
        let mut x = plot.left();
        while x < plot.right() {
            let xe = (x + 5.0).min(plot.right());
            painter.line_segment([pos2(x, y), pos2(xe, y)], Stroke::new(1.0, dcol));
            x += 9.0;
        }
    };
    if let Some(pos) = position
        && pos.shares > 1e-6
    {
        draw_dashed(pos.avg_entry, pal.blue);
    }
    for r in all_rules {
        if r.active && r.asset_id == asset_id {
            let col = match r.kind {
                RuleKind::StopLoss => pal.red,
                RuleKind::TakeProfit => pal.green,
                _ => pal.amber,
            };
            draw_dashed(r.trigger_price, col);
        }
    }

    painter.circle_filled(end_pt, 5.2, ThemePalette::tint(trend_col, 50));
    painter.circle_filled(end_pt, 3.0, trend_col);
    painter.circle_stroke(end_pt, 3.0, Stroke::new(1.2, pal.bg_card));

    let badge = Rect::from_min_max(
        pos2(plot.right() + 4.0, live_y - 8.5),
        pos2(rect.right() - 4.0, live_y + 8.5),
    );
    painter.rect_filled(badge, CornerRadius::same(5), trend_col);
    painter.text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        format_odds(Some(latest_price), odds_fmt, tick_size),
        FontId::monospace(9.1),
        ThemePalette::contrast_on(trend_col),
    );

    if let Some(lbl) = badge_label {
        let galley = painter.layout_no_wrap(lbl.to_string(), FontId::monospace(9.5), pal.text_secondary);
        let pill = Rect::from_min_size(
            pos2(plot.left() + 6.0, plot.top() + 2.0),
            vec2(galley.size().x + 12.0, 17.0),
        );
        painter.rect_filled(pill, CornerRadius::same(5), ThemePalette::tint(pal.bg_sub, 200));
        painter.galley(pos2(pill.left() + 6.0, pill.center().y - galley.size().y * 0.5), galley, pal.text_secondary);
    }

    for i in 0..=4 {
        let frac = i as f64 / 4.0;
        let t_lbl = view_min_t + (frac * span_t).round() as i64;
        let dt_lbl = chrono::DateTime::from_timestamp_millis(t_lbl)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        let x = (plot.left() + (frac as f32) * plot.width())
            .clamp(plot.left() + 24.0, plot.right() - 24.0);
        painter.text(
            pos2(x, rect.bottom() - 9.0),
            egui::Align2::CENTER_CENTER,
            dt_lbl,
            FontId::monospace(9.5),
            pal.text_muted,
        );
    }

    if let Some(hover) = response.hover_pos()
        && plot.contains(hover)
    {
        let target_t =
            view_min_t + ((((hover.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64) * span_t).round() as i64;
        let hp = samples_ms
            .iter()
            .rfind(|&&(ts, _)| ts <= target_t)
            .map(|&(_, p)| p)
            .unwrap_or(latest_price);
        let cx = hover.x.clamp(plot.left(), plot.right());
        let cy = y_of(hp).clamp(plot.top(), plot.bottom());
        painter.line_segment([pos2(cx, plot.top()), pos2(cx, plot.bottom())], Stroke::new(1.0, pal.crosshair));
        painter.line_segment([pos2(plot.left(), cy), pos2(plot.right(), cy)], Stroke::new(1.0, pal.crosshair));
        painter.circle_filled(pos2(cx, cy), 3.2, trend_col);

        let tm = chrono::DateTime::from_timestamp_millis(target_t)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
            .unwrap_or_default();
        let tip = format!("{} · {} · {tm}", tr(lang, "Shares", "份额"), format_odds(Some(hp), odds_fmt, tick_size));
        let galley = painter.layout_no_wrap(tip, FontId::monospace(10.0), pal.tooltip_fg);
        let tw = (galley.size().x + 16.0).max(128.0);
        let bx = (cx + 10.0).min((plot.right() - tw - 4.0).max(plot.left() + 6.0)).max(plot.left() + 6.0);
        let by = (cy - 26.0).max(plot.top() + 4.0);
        let tip_rect = Rect::from_min_size(pos2(bx, by), vec2(tw, 22.0));
        painter.rect_filled(tip_rect, CornerRadius::same(8), pal.tooltip_bg);
        painter.rect_stroke(tip_rect, CornerRadius::same(8), Stroke::new(1.0, pal.border_strong), egui::StrokeKind::Inside);
        painter.galley(pos2(tip_rect.left() + 8.0, tip_rect.center().y - galley.size().y * 0.5), galley, pal.tooltip_fg);
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

    #[test]
    fn format_oracle_usd_formats_commas_and_precision() {
        assert_eq!(format_oracle_usd(61_420.35), "$61,420.35");
        assert_eq!(format_oracle_usd(1_234_567.89), "$1,234,567.89");
        assert_eq!(format_oracle_usd(2.451), "$2.451");
        assert_eq!(format_oracle_usd(0.4512), "$0.4512");
        assert_eq!(format_oracle_usd(-12.5), "-$12.500");
    }

    #[test]
    fn short_chart_view_defaults_to_both() {
        assert_eq!(ShortChartView::default(), ShortChartView::Both);
    }

    #[test]
    fn compute_twap_series_60s_matches_scalar_twap() {
        let base_ms = 1_700_000_000_000_i64;
        let samples: Vec<(i64, f64)> = (0..180)
            .map(|i| (base_ms + i * 500, 61_500.0 + ((i as f64) * 0.2).sin() * 25.0))
            .collect();
        let series = compute_twap_series_60s(&samples);
        assert_eq!(series.len(), samples.len());
        let mut full_q = VecDeque::new();
        for (i, &(t, p)) in samples.iter().enumerate() {
            full_q.push_back(OracleSample { timestamp_ms: t, price: p });
            let expected = compute_twap_60s(&full_q, t).unwrap();
            assert!((series[i].1 - expected).abs() < 1e-9, "mismatch at {i}: {} vs {expected}", series[i].1);
        }
    }

    #[test]
    fn twap_series_is_recomputed_only_when_the_input_changes() {
        use std::sync::atomic::Ordering;

        let base_ms = 1_700_000_000_000_i64;
        let mut samples: Vec<(i64, f64)> = (0..240)
            .map(|i| (base_ms + i * 50, 61_500.0 + ((i as f64) * 0.1).sin() * 25.0))
            .collect();
        let mut state = OracleChartSmoothState::default();

        TWAP_COMPUTE_COUNT.store(0, Ordering::Relaxed);
        let first = twap_series_cached(&mut state, &samples, true);
        assert_eq!(TWAP_COMPUTE_COUNT.load(Ordering::Relaxed), 1);
        assert_eq!(first.len(), samples.len());

        // 125 repaints in the same tick must not recompute anything.
        for _ in 0..125 {
            let again = twap_series_cached(&mut state, &samples, true);
            assert_eq!(again.len(), first.len());
        }
        assert_eq!(
            TWAP_COMPUTE_COUNT.load(Ordering::Relaxed),
            1,
            "TWAP overlay must be memoized across repaints"
        );

        // A new sample invalidates it exactly once.
        samples.push((base_ms + 240 * 50, 61_600.0));
        let _ = twap_series_cached(&mut state, &samples, true);
        assert_eq!(TWAP_COMPUTE_COUNT.load(Ordering::Relaxed), 2);

        // Toggling the layer off must not compute, and must not poison the cache.
        assert!(twap_series_cached(&mut state, &samples, false).is_empty());
        assert_eq!(TWAP_COMPUTE_COUNT.load(Ordering::Relaxed), 2);
        let _ = twap_series_cached(&mut state, &samples, true);
        assert_eq!(TWAP_COMPUTE_COUNT.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn synced_window_x_mapping_aligns_oracle_and_share_charts() {
        let view_max_t = 1_700_000_180_000_i64;
        let window_ms = 180_000_i64;
        let view_min_t = view_max_t - window_ms;
        assert_eq!(view_max_t - view_min_t, window_ms);
        let plot_left = 12.0_f32;
        let plot_width = 800.0_f32;
        let map_x = |t: i64| -> f32 {
            let frac = ((t - view_min_t) as f64 / (view_max_t - view_min_t) as f64).clamp(0.0, 1.0) as f32;
            plot_left + frac * plot_width
        };
        assert!((map_x(view_max_t) - (plot_left + plot_width)).abs() < 1e-4);
        assert!((map_x(view_min_t) - plot_left).abs() < 1e-4);
    }

    #[test]
    fn oracle_chart_smooth_state_reset_clears_coin_and_share_state() {
        let mut st = OracleChartSmoothState {
            initialized: true,
            symbol: "btc/usd".into(),
            last_frame: Some(std::time::Instant::now()),
            disp_min_p: 60_000.0,
            disp_max_p: 62_000.0,
            disp_spot: 61_000.0,
            disp_fast_spot: Some(61_002.0),
            disp_spot_pill_y: 120.0,
            pill_y_initialized: true,
            disp_green_blend: 0.75,
            share_y_initialized: true,
            disp_share_min: 0.35,
            disp_share_max: 0.65,
            disp_share_pill_y: 140.0,
            share_pill_y_init: true,
            twap_cache_key: 42,
            twap_cache: vec![(1_700_000_000_000, 61_000.0)],
        };
        st.reset();
        assert!(!st.initialized);
        assert!(!st.share_y_initialized);
        assert!(!st.share_pill_y_init);
        assert!(st.symbol.is_empty());
        assert!(st.twap_cache.is_empty());
        assert_eq!(st.twap_cache_key, 0);
    }

    #[test]
    fn share_segments_handle_live_step_history_and_dead_gaps() {
        let view_max_t = 1_700_000_180_000_i64;
        let view_min_t = view_max_t - 180_000;

        // 1. Dead gap (>150s) from stale seed to live tip must NOT bridge diagonally.
        let sparse = vec![(view_min_t - 5_000, 0.62), (view_max_t - 2_000, 0.50)];
        let segs_sparse = build_share_segments(&sparse, 0.50, view_min_t, view_max_t);
        assert_eq!(segs_sparse.len(), 1);
        assert_eq!(segs_sparse[0].kind, ShareSegKind::Live);
        assert_eq!(segs_sparse[0].pts.len(), 2);

        // 2. Intra-slot 60s REST points form HistoryStep holds, followed by dense Live curve.
        let mixed = vec![
            (view_min_t + 10_000, 0.55),
            (view_min_t + 70_000, 0.52),
            (view_min_t + 130_000, 0.58),
            (view_max_t - 5_000, 0.60),
            (view_max_t - 2_000, 0.61),
        ];
        let segs_mixed = build_share_segments(&mixed, 0.61, view_min_t, view_max_t);
        assert_eq!(segs_mixed.len(), 2);
        assert_eq!(segs_mixed[0].kind, ShareSegKind::HistoryStep);
        assert_eq!(segs_mixed[1].kind, ShareSegKind::Live);
    }

    #[test]
    fn resolve_pill_stack_separates_three_colliding_pills() {
        let mut ys = [120.0_f32, 122.0, 118.0];
        resolve_pill_stack(&mut ys, 30.0, 220.0, 20.0);
        let mut sorted = ys.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        assert!(sorted[1] - sorted[0] >= 19.9, "gap01={}", sorted[1] - sorted[0]);
        assert!(sorted[2] - sorted[1] >= 19.9, "gap12={}", sorted[2] - sorted[1]);
        assert!(sorted[0] >= 30.0 && sorted[2] <= 220.0);
    }

    #[test]
    fn stabilized_share_bounds_enforces_min_span() {
        let (lo, hi) = stabilized_share_bounds(0.49, 0.50);
        assert!((hi - lo) >= MIN_SHARE_SPAN - 1e-6);
        assert!((lo + hi) * 0.5 - 0.495 < 0.01);
    }
}
