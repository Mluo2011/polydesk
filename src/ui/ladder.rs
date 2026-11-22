use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

use polydesk::automation::{AutomationRule, RuleKind};
use polydesk::book::BookView;
use polydesk::i18n::{Lang, tr};
use polydesk::odds::{OddsFormat, format_odds, format_size, format_usd};
use polydesk::paper::{PaperOrder, PaperPosition};

pub enum LadderAction {
    PlaceOrder { side: &'static str, price: f64, post_only_override: bool },
    CancelLevel { side: &'static str, price: f64 },
}

pub struct LadderState {
    pub step: f64,
    pub center_price: f64,
    pub auto_center: bool,
    pub initialized: bool,
}

impl Default for LadderState {
    fn default() -> Self {
        Self {
            step: 0.01,
            center_price: 0.50,
            auto_center: true,
            initialized: false,
        }
    }
}

impl LadderState {
    pub fn reset_for_market(&mut self, tick_size: f64) {
        self.step = tick_size.clamp(0.001, 0.05);
        self.auto_center = true;
        self.initialized = false;
    }

    pub fn recenter(&mut self, book: Option<&BookView>) {
        if let Some(mid) = book.and_then(|b| b.mid) {
            self.center_price = clamp_center(mid, self.step);
        }
    }
}

/// Modern, airy GPU-rendered Desk-style One-Click Price Ladder (DOM).
/// Fires instantly on primary pointer press (0ms release delay, 0µs IPC).
#[allow(clippy::too_many_arguments)]
pub fn draw_price_ladder(
    ui: &mut Ui,
    lang: Lang,
    state: &mut LadderState,
    odds_fmt: OddsFormat,
    book: Option<&BookView>,
    open_orders: &[PaperOrder],
    position: Option<&PaperPosition>,
    rules: &[AutomationRule],
) -> Option<LadderAction> {
    let step = state.step.max(0.001);
    let avail = ui.available_size();
    let row_h = 26.0_f32;
    let hdr_h = 26.0_f32;
    let visible_rows = (((avail.y - hdr_h - 4.0) / row_h).floor() as usize).clamp(9, 41);
    let visible_rows = if visible_rows % 2 == 0 { visible_rows - 1 } else { visible_rows };
    let half = (visible_rows / 2) as isize;

    let total_h = hdr_h + (visible_rows as f32) * row_h;
    let (rect, response) = ui.allocate_exact_size(vec2(avail.x.max(260.0), total_h), Sense::click_and_drag());

    // Never auto-recenter while the user's cursor is hovering the ladder (prevents rows jumping under your mouse!)
    if let Some(mid) = book.and_then(|b| b.mid) {
        if !state.initialized {
            state.center_price = clamp_center(mid, state.step);
            state.initialized = true;
        } else if !response.hovered() && state.auto_center && (mid - state.center_price).abs() >= state.step * 6.0 {
            state.center_price = clamp_center(mid, state.step);
        }
    }

    if response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 2.0 {
            state.auto_center = false;
            let dir = if scroll_y > 0.0 { 1.0 } else { -1.0 };
            state.center_price = clamp_center(state.center_price + dir * step * 2.0, step);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(8), Color32::from_rgb(11, 15, 23));
    painter.rect_stroke(
        rect,
        CornerRadius::same(8),
        Stroke::new(1.0, Color32::from_rgb(31, 41, 55)),
        egui::StrokeKind::Inside,
    );

    // Spacious column layout: [MyBid | BidDepth | Price | AskDepth | MyAsk | P&L]
    let w = rect.width();
    let c_my = 44.0_f32;
    let c_px = 82.0_f32;
    let c_pnl = 66.0_f32;
    let c_depth = ((w - c_my * 2.0 - c_px - c_pnl) * 0.5).max(52.0);

    let x0 = rect.left();
    let x1 = x0 + c_my;
    let x2 = x1 + c_depth;
    let x3 = x2 + c_px;
    let x4 = x3 + c_depth;
    let x5 = x4 + c_my;
    let x6 = rect.right();

    // Header
    let hdr_rect = Rect::from_min_max(pos2(x0 + 1.0, rect.top() + 1.0), pos2(x6 - 1.0, rect.top() + hdr_h));
    painter.rect_filled(
        hdr_rect,
        CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 },
        Color32::from_rgb(20, 27, 40),
    );
    let hdr_col = Color32::from_rgb(138, 154, 178);
    let hdr_font = FontId::proportional(11.0);
    let hdr_y = hdr_rect.center().y;
    painter.text(pos2((x0 + x1) * 0.5, hdr_y), Align2::CENTER_CENTER, tr(lang, "MyB", "买挂"), hdr_font.clone(), hdr_col);
    painter.text(pos2((x1 + x2) * 0.5, hdr_y), Align2::CENTER_CENTER, tr(lang, "Bid Size", "买盘深度"), hdr_font.clone(), hdr_col);
    painter.text(pos2((x2 + x3) * 0.5, hdr_y), Align2::CENTER_CENTER, tr(lang, "Price", "价格"), hdr_font.clone(), hdr_col);
    painter.text(pos2((x3 + x4) * 0.5, hdr_y), Align2::CENTER_CENTER, tr(lang, "Ask Size", "卖盘深度"), hdr_font.clone(), hdr_col);
    painter.text(pos2((x4 + x5) * 0.5, hdr_y), Align2::CENTER_CENTER, tr(lang, "MyA", "卖挂"), hdr_font.clone(), hdr_col);
    painter.text(pos2((x5 + x6) * 0.5, hdr_y), Align2::CENTER_CENTER, tr(lang, "P&L", "预估盈亏"), hdr_font, hdr_col);

    // Aggregate book & orders
    let mut bid_map: HashMap<isize, f64> = HashMap::new();
    let mut ask_map: HashMap<isize, f64> = HashMap::new();
    if let Some(b) = book {
        for &(p, s) in &b.bids {
            let k = (p / step + 1e-9).floor() as isize;
            *bid_map.entry(k).or_insert(0.0) += s;
        }
        for &(p, s) in &b.asks {
            let k = (p / step - 1e-9).ceil() as isize;
            *ask_map.entry(k).or_insert(0.0) += s;
        }
    }

    let mut my_bids: HashMap<isize, f64> = HashMap::new();
    let mut my_asks: HashMap<isize, f64> = HashMap::new();
    for o in open_orders {
        let k = (o.price / step).round() as isize;
        if o.side == "BUY" {
            *my_bids.entry(k).or_insert(0.0) += o.remaining_size;
        } else {
            *my_asks.entry(k).or_insert(0.0) += o.remaining_size;
        }
    }

    let mut rule_tags: HashMap<isize, (&'static str, Color32)> = HashMap::new();
    for r in rules {
        if !r.active { continue; }
        let k = (r.trigger_price / step).round() as isize;
        let pair = match r.kind {
            RuleKind::StopLoss => ("SL", Color32::from_rgb(244, 63, 94)),
            RuleKind::TakeProfit => ("TP", Color32::from_rgb(16, 185, 129)),
            RuleKind::TrailingStop => ("TRL", Color32::from_rgb(245, 158, 11)),
            RuleKind::DripFeed => ("DRP", Color32::from_rgb(56, 189, 248)),
        };
        rule_tags.insert(k, pair);
    }

    let center_k = (state.center_price / step).round() as isize;
    let best_bid_k = book.and_then(|b| b.best_bid).map(|p| (p / step + 1e-9).floor() as isize);
    let best_ask_k = book.and_then(|b| b.best_ask).map(|p| (p / step - 1e-9).ceil() as isize);
    let last_k = book.and_then(|b| b.last_trade_price).map(|p| (p / step).round() as isize);
    let avg_k = position.filter(|p| p.shares > 1e-6).map(|p| (p.avg_entry / step).round() as isize);

    let mut max_depth = 10.0_f64;
    for i in 0..visible_rows {
        let k = center_k + (half - i as isize);
        if let Some(&v) = bid_map.get(&k) { max_depth = max_depth.max(v); }
        if let Some(&v) = ask_map.get(&k) { max_depth = max_depth.max(v); }
    }

    let (hover_pos, primary_pressed, shift_held) = ui.input(|i| {
        (i.pointer.hover_pos(), i.pointer.primary_pressed(), i.modifiers.shift)
    });

    let mut action: Option<LadderAction> = None;
    let mono = FontId::monospace(11.5);
    let mono_sm = FontId::monospace(10.0);
    let row_sep = Stroke::new(0.5, Color32::from_rgba_unmultiplied(31, 41, 55, 110));
    let px_bg = Color32::from_rgb(18, 24, 36);
    let bid_bar = Color32::from_rgba_unmultiplied(16, 185, 129, 54);
    let ask_bar = Color32::from_rgba_unmultiplied(244, 63, 94, 54);

    for i in 0..visible_rows {
        let k = center_k + (half - i as isize);
        let p = ((k as f64) * step * 10_000.0).round() / 10_000.0;
        let valid = p >= 0.001 && p <= 0.999;

        let y_top = rect.top() + hdr_h + (i as f32) * row_h;
        let y_bot = y_top + row_h;
        let y_mid = (y_top + y_bot) * 0.5;
        let row_rect = Rect::from_min_max(pos2(x0 + 1.0, y_top), pos2(x6 - 1.0, y_bot));

        painter.line_segment([pos2(x0 + 1.0, y_bot), pos2(x6 - 1.0, y_bot)], row_sep);
        if !valid { continue; }

        if best_bid_k == Some(k) {
            painter.rect_filled(row_rect, 0.0, Color32::from_rgba_unmultiplied(16, 185, 129, 20));
        } else if best_ask_k == Some(k) {
            painter.rect_filled(row_rect, 0.0, Color32::from_rgba_unmultiplied(244, 63, 94, 20));
        }

        let r_my_bid = Rect::from_min_max(pos2(x0, y_top), pos2(x1, y_bot));
        let r_bid = Rect::from_min_max(pos2(x1, y_top), pos2(x2, y_bot));
        let r_px = Rect::from_min_max(pos2(x2, y_top), pos2(x3, y_bot));
        let r_ask = Rect::from_min_max(pos2(x3, y_top), pos2(x4, y_bot));
        let r_my_ask = Rect::from_min_max(pos2(x4, y_top), pos2(x5, y_bot));
        let r_pnl = Rect::from_min_max(pos2(x5, y_top), pos2(x6, y_bot));

        if let Some(hp) = hover_pos {
            if r_bid.contains(hp) {
                painter.rect_filled(r_bid.shrink(1.0), 4.0, Color32::from_rgba_unmultiplied(16, 185, 129, 64));
                if primary_pressed {
                    action = Some(LadderAction::PlaceOrder { side: "BUY", price: p, post_only_override: shift_held });
                }
            } else if r_ask.contains(hp) {
                painter.rect_filled(r_ask.shrink(1.0), 4.0, Color32::from_rgba_unmultiplied(244, 63, 94, 64));
                if primary_pressed {
                    action = Some(LadderAction::PlaceOrder { side: "SELL", price: p, post_only_override: shift_held });
                }
            } else if r_my_bid.contains(hp) && my_bids.contains_key(&k) {
                painter.rect_filled(r_my_bid.shrink(2.0), 4.0, Color32::from_rgba_unmultiplied(245, 158, 11, 100));
                if primary_pressed {
                    action = Some(LadderAction::CancelLevel { side: "BUY", price: p });
                }
            } else if r_my_ask.contains(hp) && my_asks.contains_key(&k) {
                painter.rect_filled(r_my_ask.shrink(2.0), 4.0, Color32::from_rgba_unmultiplied(245, 158, 11, 100));
                if primary_pressed {
                    action = Some(LadderAction::CancelLevel { side: "SELL", price: p });
                }
            }
        }

        if let Some(&bsz) = bid_map.get(&k) && bsz > 0.0 {
            let bw = ((bsz / max_depth).min(1.0) as f32) * (r_bid.width() - 4.0);
            painter.rect_filled(
                Rect::from_min_max(pos2(r_bid.right() - bw - 2.0, y_top + 3.0), pos2(r_bid.right() - 2.0, y_bot - 3.0)),
                3.0,
                bid_bar,
            );
            painter.text(pos2(r_bid.right() - 7.0, y_mid), Align2::RIGHT_CENTER, format_size(bsz), mono.clone(), Color32::from_rgb(167, 243, 208));
        }

        if let Some(&asz) = ask_map.get(&k) && asz > 0.0 {
            let aw = ((asz / max_depth).min(1.0) as f32) * (r_ask.width() - 4.0);
            painter.rect_filled(
                Rect::from_min_max(pos2(r_ask.left() + 2.0, y_top + 3.0), pos2(r_ask.left() + aw + 2.0, y_bot - 3.0)),
                3.0,
                ask_bar,
            );
            painter.text(pos2(r_ask.left() + 7.0, y_mid), Align2::LEFT_CENTER, format_size(asz), mono.clone(), Color32::from_rgb(254, 205, 211));
        }

        let px_fill = if avg_k == Some(k) { Color32::from_rgba_unmultiplied(56, 189, 248, 52) } else { px_bg };
        painter.rect_filled(r_px, 0.0, px_fill);
        if last_k == Some(k) {
            painter.rect_stroke(r_px.shrink(1.0), 3.0, Stroke::new(1.2, Color32::from_rgb(56, 189, 248)), egui::StrokeKind::Inside);
        }
        painter.text(r_px.center(), Align2::CENTER_CENTER, format_odds(Some(p), odds_fmt, step), mono.clone(), Color32::from_rgb(241, 245, 249));

        if let Some(&(tag, tcol)) = rule_tags.get(&k) {
            let b = Rect::from_min_max(pos2(r_px.right() - 25.0, y_top + 4.0), pos2(r_px.right() - 2.0, y_bot - 4.0));
            painter.rect_filled(b, 3.0, tcol);
            painter.text(b.center(), Align2::CENTER_CENTER, tag, FontId::monospace(8.5), Color32::from_rgb(10, 13, 20));
        }

        if let Some(&mb) = my_bids.get(&k) && mb > 0.0 {
            painter.rect_filled(r_my_bid.shrink(2.5), 4.0, Color32::from_rgba_unmultiplied(245, 158, 11, 52));
            painter.text(r_my_bid.center(), Align2::CENTER_CENTER, format!("×{}", format_size(mb)), mono_sm.clone(), Color32::from_rgb(251, 191, 36));
        }
        if let Some(&ma) = my_asks.get(&k) && ma > 0.0 {
            painter.rect_filled(r_my_ask.shrink(2.5), 4.0, Color32::from_rgba_unmultiplied(245, 158, 11, 52));
            painter.text(r_my_ask.center(), Align2::CENTER_CENTER, format!("×{}", format_size(ma)), mono_sm.clone(), Color32::from_rgb(251, 191, 36));
        }

        if let Some(pos) = position && pos.shares > 0.01 {
            let pnl = pos.shares * (p - pos.avg_entry);
            let pcol = if pnl >= 0.0 { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(244, 63, 94) };
            painter.text(pos2(r_pnl.right() - 6.0, y_mid), Align2::RIGHT_CENTER, format_usd(pnl, true), mono_sm.clone(), pcol);
        }
    }

    action
}

fn clamp_center(p: f64, step: f64) -> f64 {
    let k = (p / step).round();
    ((k * step).clamp(step * 5.0, 1.0 - step * 5.0) * 10_000.0).round() / 10_000.0
}
