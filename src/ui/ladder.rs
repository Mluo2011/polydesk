use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

use polydesk::automation::{AutomationRule, RuleKind};
use polydesk::book::BookView;
use polydesk::i18n::{Lang, tr};
use polydesk::odds::{OddsFormat, format_odds, format_size, format_usd};
use polydesk::paper::{PaperOrder, PaperPosition};

use super::theme::ThemePalette;

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

    pub fn recenter_mid(&mut self, mid: Option<f64>) {
        if let Some(mid) = mid {
            self.center_price = clamp_center(mid, self.step);
        }
    }

    pub fn recenter(&mut self, book: Option<&BookView>) {
        self.recenter_mid(book.and_then(|b| b.mid));
    }
}

/// GPU-rendered Desk-style One-Click Price Ladder (DOM).
/// Fires instantly on primary pointer press (0ms release delay, 0µs IPC).
#[allow(clippy::too_many_arguments)]
pub fn draw_price_ladder(
    ui: &mut Ui,
    lang: Lang,
    pal: ThemePalette,
    state: &mut LadderState,
    odds_fmt: OddsFormat,
    active_asset_id: Option<&str>,
    book: Option<&BookView>,
    open_orders: &[PaperOrder],
    position: Option<&PaperPosition>,
    rules: &[AutomationRule],
    interact_enabled: bool,
) -> Option<LadderAction> {
    let step = state.step.max(0.001);
    let avail = ui.available_size();
    let imb_h = 20.0_f32;
    let row_h = 25.0_f32;
    let hdr_h = 24.0_f32;
    let visible_rows = (((avail.y - imb_h - hdr_h - 4.0) / row_h).floor() as usize).clamp(9, 41);
    let visible_rows = if visible_rows % 2 == 0 { visible_rows - 1 } else { visible_rows };
    let half = (visible_rows / 2) as isize;

    let total_h = imb_h + hdr_h + (visible_rows as f32) * row_h;
    let sense = if interact_enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(avail.x.max(260.0), total_h), sense);

    // Never auto-recenter while cursor hovers over the ladder
    if let Some(mid) = book.and_then(|b| b.mid) {
        if !state.initialized {
            state.center_price = clamp_center(mid, state.step);
            state.initialized = true;
        } else if !(interact_enabled && response.hovered())
            && state.auto_center
            && (mid - state.center_price).abs() >= state.step * 6.0
        {
            state.center_price = clamp_center(mid, state.step);
        }
    }

    if interact_enabled && response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 2.0 {
            state.auto_center = false;
            let dir = if scroll_y > 0.0 { 1.0 } else { -1.0 };
            state.center_price = clamp_center(state.center_price + dir * step * 2.0, step);
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(10), pal.bg_card);
    painter.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0, pal.border),
        egui::StrokeKind::Inside,
    );

    // Column layout: [MyBid | BidDepth | Price | AskDepth | MyAsk | P&L]
    let w = rect.width();
    let c_my = 44.0_f32;
    let c_px = 80.0_f32;
    let c_pnl = 64.0_f32;
    let c_depth = ((w - c_my * 2.0 - c_px - c_pnl) * 0.5).max(48.0);

    let x0 = rect.left();
    let x1 = x0 + c_my;
    let x2 = x1 + c_depth;
    let x3 = x2 + c_px;
    let x4 = x3 + c_depth;
    let x5 = x4 + c_my;
    let x6 = rect.right();

    // Header (sits beneath the 20px Bid/Ask pressure bar)
    let hdr_top = rect.top() + imb_h;
    let hdr_rect = Rect::from_min_max(pos2(x0 + 1.0, hdr_top), pos2(x6 - 1.0, hdr_top + hdr_h));
    painter.rect_filled(hdr_rect, CornerRadius::ZERO, pal.ladder_hdr_bg);
    painter.line_segment(
        [pos2(x0 + 1.0, hdr_top), pos2(x6 - 1.0, hdr_top)],
        Stroke::new(1.0, pal.border),
    );
    painter.line_segment(
        [pos2(x0 + 1.0, hdr_top + hdr_h), pos2(x6 - 1.0, hdr_top + hdr_h)],
        Stroke::new(1.0, pal.border),
    );
    let hdr_col = pal.text_muted;
    let hdr_font = FontId::proportional(10.5);
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
    let mut tot_bids = 0.0_f64;
    let mut tot_asks = 0.0_f64;
    if let Some(b) = book {
        for &(p, s) in &b.bids {
            let k = (p / step + 1e-9).floor() as isize;
            *bid_map.entry(k).or_insert(0.0) += s;
            tot_bids += s;
        }
        for &(p, s) in &b.asks {
            let k = (p / step - 1e-9).ceil() as isize;
            *ask_map.entry(k).or_insert(0.0) += s;
            tot_asks += s;
        }
    }

    // Bid / Ask pressure bar at top of ladder
    let imb_rect = Rect::from_min_max(pos2(x0 + 1.0, rect.top() + 1.0), pos2(x6 - 1.0, hdr_top));
    painter.rect_filled(imb_rect, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, pal.bg_sub);
    let bid_share = if tot_bids + tot_asks > 1e-6 {
        (tot_bids / (tot_bids + tot_asks)) as f32
    } else {
        book.map(|b| (b.imbalance * 0.5 + 0.5).clamp(0.05, 0.95) as f32)
            .unwrap_or(0.5)
    };
    let bid_pct = (bid_share * 100.0).round() as i32;
    let ask_pct = 100 - bid_pct;
    let bar_track = Rect::from_min_max(pos2(x0 + 78.0, imb_rect.center().y - 2.5), pos2(x6 - 78.0, imb_rect.center().y + 2.5));
    if bar_track.width() > 24.0 {
        let split_x = bar_track.left() + bar_track.width() * bid_share.clamp(0.04, 0.96);
        painter.rect_filled(bar_track, CornerRadius::same(2), ThemePalette::tint(pal.red, 95));
        painter.rect_filled(
            Rect::from_min_max(bar_track.left_top(), pos2(split_x, bar_track.bottom())),
            CornerRadius::same(2),
            pal.green,
        );
    }
    painter.text(
        pos2(x0 + 10.0, imb_rect.center().y),
        Align2::LEFT_CENTER,
        format!("{} {}%", tr(lang, "BID", "买盘"), bid_pct),
        FontId::monospace(9.5),
        pal.green_text,
    );
    painter.text(
        pos2(x6 - 10.0, imb_rect.center().y),
        Align2::RIGHT_CENTER,
        format!("{}% {}", ask_pct, tr(lang, "ASK", "卖盘")),
        FontId::monospace(9.5),
        pal.red_text,
    );

    let mut my_bids: HashMap<isize, f64> = HashMap::new();
    let mut my_asks: HashMap<isize, f64> = HashMap::new();
    for o in open_orders {
        if active_asset_id != Some(o.asset_id.as_str()) {
            continue;
        }
        let k = (o.price / step).round() as isize;
        if o.side == "BUY" {
            *my_bids.entry(k).or_insert(0.0) += o.remaining_size;
        } else {
            *my_asks.entry(k).or_insert(0.0) += o.remaining_size;
        }
    }

    let mut rule_tags: HashMap<isize, (&'static str, Color32)> = HashMap::new();
    for r in rules {
        if !r.active || active_asset_id != Some(r.asset_id.as_str()) {
            continue;
        }
        let k = (r.trigger_price / step).round() as isize;
        let pair = match r.kind {
            RuleKind::StopLoss => ("SL", pal.red),
            RuleKind::TakeProfit => ("TP", pal.green),
            RuleKind::TrailingStop => ("TRL", pal.amber),
            RuleKind::DripFeed => ("DRP", pal.blue),
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
    let allow_pointer = interact_enabled && response.hovered();

    let mut action: Option<LadderAction> = None;
    let mono = FontId::monospace(11.0);
    let mono_sm = FontId::monospace(10.0);
    let row_sep = Stroke::new(0.5, pal.grid_line);
    let px_bg = pal.px_col_bg;
    let bid_bar = ThemePalette::tint(pal.green, if pal.light { 34 } else { 44 });
    let ask_bar = ThemePalette::tint(pal.red, if pal.light { 30 } else { 44 });
    let best_bid_row = ThemePalette::tint(pal.green, 16);
    let best_ask_row = ThemePalette::tint(pal.red, 16);
    let my_order_fill = ThemePalette::tint(pal.amber, if pal.light { 34 } else { 42 });

    for i in 0..visible_rows {
        let k = center_k + (half - i as isize);
        let p = ((k as f64) * step * 10_000.0).round() / 10_000.0;
        let valid = (0.001..=0.999).contains(&p);

        let y_top = hdr_top + hdr_h + (i as f32) * row_h;
        let y_bot = y_top + row_h;
        let y_mid = (y_top + y_bot) * 0.5;
        let row_rect = Rect::from_min_max(pos2(x0 + 1.0, y_top), pos2(x6 - 1.0, y_bot));

        painter.line_segment([pos2(x0 + 1.0, y_bot), pos2(x6 - 1.0, y_bot)], row_sep);
        if !valid { continue; }

        if best_bid_k == Some(k) {
            painter.rect_filled(row_rect, 0.0, best_bid_row);
        } else if best_ask_k == Some(k) {
            painter.rect_filled(row_rect, 0.0, best_ask_row);
        }

        let r_my_bid = Rect::from_min_max(pos2(x0, y_top), pos2(x1, y_bot));
        let r_bid = Rect::from_min_max(pos2(x1, y_top), pos2(x2, y_bot));
        let r_px = Rect::from_min_max(pos2(x2, y_top), pos2(x3, y_bot));
        let r_ask = Rect::from_min_max(pos2(x3, y_top), pos2(x4, y_bot));
        let r_my_ask = Rect::from_min_max(pos2(x4, y_top), pos2(x5, y_bot));
        let r_pnl = Rect::from_min_max(pos2(x5, y_top), pos2(x6, y_bot));

        if allow_pointer && let Some(hp) = hover_pos {
            if r_bid.contains(hp) {
                painter.rect_filled(r_bid.shrink(1.0), 3.0, ThemePalette::tint(pal.green, 56));
                if primary_pressed {
                    action = Some(LadderAction::PlaceOrder { side: "BUY", price: p, post_only_override: shift_held });
                }
            } else if r_ask.contains(hp) {
                painter.rect_filled(r_ask.shrink(1.0), 3.0, ThemePalette::tint(pal.red, 56));
                if primary_pressed {
                    action = Some(LadderAction::PlaceOrder { side: "SELL", price: p, post_only_override: shift_held });
                }
            } else if r_my_bid.contains(hp) && my_bids.contains_key(&k) {
                painter.rect_filled(r_my_bid.shrink(2.0), 3.0, ThemePalette::tint(pal.amber, 86));
                if primary_pressed {
                    action = Some(LadderAction::CancelLevel { side: "BUY", price: p });
                }
            } else if r_my_ask.contains(hp) && my_asks.contains_key(&k) {
                painter.rect_filled(r_my_ask.shrink(2.0), 3.0, ThemePalette::tint(pal.amber, 86));
                if primary_pressed {
                    action = Some(LadderAction::CancelLevel { side: "SELL", price: p });
                }
            }
        }

        if let Some(&bsz) = bid_map.get(&k) && bsz > 0.0 {
            let bw = ((bsz / max_depth).min(1.0) as f32) * (r_bid.width() - 4.0);
            painter.rect_filled(
                Rect::from_min_max(pos2(r_bid.right() - bw - 2.0, y_top + 2.5), pos2(r_bid.right() - 2.0, y_bot - 2.5)),
                3.0,
                bid_bar,
            );
            painter.text(pos2(r_bid.right() - 6.0, y_mid), Align2::RIGHT_CENTER, format_size(bsz), mono.clone(), pal.green_text);
        }

        if let Some(&asz) = ask_map.get(&k) && asz > 0.0 {
            let aw = ((asz / max_depth).min(1.0) as f32) * (r_ask.width() - 4.0);
            painter.rect_filled(
                Rect::from_min_max(pos2(r_ask.left() + 2.0, y_top + 2.5), pos2(r_ask.left() + aw + 2.0, y_bot - 2.5)),
                3.0,
                ask_bar,
            );
            painter.text(pos2(r_ask.left() + 6.0, y_mid), Align2::LEFT_CENTER, format_size(asz), mono.clone(), pal.red_text);
        }

        let px_fill = if avg_k == Some(k) {
            ThemePalette::tint(pal.blue, if pal.light { 34 } else { 44 })
        } else {
            px_bg
        };
        painter.rect_filled(r_px, 0.0, px_fill);
        if last_k == Some(k) {
            painter.rect_stroke(
                r_px.shrink(1.0),
                CornerRadius::same(2),
                Stroke::new(1.2, pal.blue),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(r_px.center(), Align2::CENTER_CENTER, format_odds(Some(p), odds_fmt, step), mono.clone(), pal.text_primary);

        if let Some(&(tag, tcol)) = rule_tags.get(&k) {
            let b = Rect::from_min_max(pos2(r_px.right() - 25.0, y_top + 4.0), pos2(r_px.right() - 2.0, y_bot - 4.0));
            painter.rect_filled(b, CornerRadius::same(2), tcol);
            painter.text(
                b.center(),
                Align2::CENTER_CENTER,
                tag,
                FontId::monospace(8.5),
                ThemePalette::contrast_on(tcol),
            );
        }

        if let Some(&mb) = my_bids.get(&k) && mb > 0.0 {
            painter.rect_filled(r_my_bid.shrink(2.0), 3.0, my_order_fill);
            painter.text(r_my_bid.center(), Align2::CENTER_CENTER, format!("×{}", format_size(mb)), mono_sm.clone(), pal.amber);
        }
        if let Some(&ma) = my_asks.get(&k) && ma > 0.0 {
            painter.rect_filled(r_my_ask.shrink(2.0), 3.0, my_order_fill);
            painter.text(r_my_ask.center(), Align2::CENTER_CENTER, format!("×{}", format_size(ma)), mono_sm.clone(), pal.amber);
        }

        if let Some(pos) = position && pos.shares > 0.01 {
            let pnl = pos.shares * (p - pos.avg_entry);
            let pcol = if pnl >= 0.0 { pal.green } else { pal.red };
            painter.text(pos2(r_pnl.right() - 6.0, y_mid), Align2::RIGHT_CENTER, format_usd(pnl, true), mono_sm.clone(), pcol);
        }
    }

    action
}

fn clamp_center(p: f64, step: f64) -> f64 {
    let k = (p / step).round();
    ((k * step).clamp(step * 5.0, 1.0 - step * 5.0) * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_ladder_blocks_clicks_when_interact_disabled() {
        let pal = ThemePalette::dark();
        let click_pos = pos2(80.0, 140.0);

        let run_click_case = |interact_enabled: bool| -> Option<LadderAction> {
            let ctx = egui::Context::default();
            let mut state = LadderState::default();
            let make_raw = |events: Vec<egui::Event>| egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 400.0))),
                events,
                ..Default::default()
            };
            let mut step = |events: Vec<egui::Event>| -> Option<LadderAction> {
                let mut captured = None;
                let mut out = ctx.run_ui(make_raw(events), |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        captured = draw_price_ladder(
                            ui,
                            Lang::En,
                            pal,
                            &mut state,
                            OddsFormat::Cents,
                            Some("tok1"),
                            None,
                            &[],
                            None,
                            &[],
                            interact_enabled,
                        );
                    });
                });
                out.textures_delta.clear();
                captured
            };

            // Warm-up frame so egui records widget rect and hover state.
            let _ = step(vec![egui::Event::PointerMoved(click_pos)]);
            // Click frame while pointer hovers the Bid cell.
            step(vec![
                egui::Event::PointerMoved(click_pos),
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ])
        };

        assert!(matches!(
            run_click_case(true),
            Some(LadderAction::PlaceOrder { side: "BUY", .. })
        ));
        assert!(run_click_case(false).is_none());
    }
}
