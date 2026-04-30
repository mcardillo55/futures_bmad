use super::{FillEvent, FillType, FixedPrice, Side};

/// Tracks the current position for a symbol.
///
/// Story 4-5 added `realized_pnl` (cumulative quarter-ticks, signed) so that
/// the engine can compute a session-level P&L without consulting the journal.
/// `unrealized_pnl` and `realized_pnl` are both stored as raw quarter-tick
/// integers — convert to dollars at the display layer only via
/// `pnl_quarter_ticks * tick_value / 4` (for ES, tick_value = $12.50, so a
/// quarter-tick is $3.125).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub symbol_id: u32,
    pub side: Option<Side>,
    pub quantity: u32,
    pub avg_entry_price: FixedPrice,
    /// Unrealized P&L in quarter-ticks. Updated by `update_unrealized_pnl`.
    pub unrealized_pnl: i64,
    /// Cumulative realized P&L in quarter-ticks. Updated on every closing /
    /// reducing / flipping fill. Never reset within a session.
    pub realized_pnl: i64,
}

impl Position {
    pub fn flat(symbol_id: u32) -> Self {
        Self {
            symbol_id,
            side: None,
            quantity: 0,
            avg_entry_price: FixedPrice::default(),
            unrealized_pnl: 0,
            realized_pnl: 0,
        }
    }

    pub fn is_flat(&self) -> bool {
        self.quantity == 0
    }

    pub fn is_long(&self) -> bool {
        self.side == Some(Side::Buy) && self.quantity > 0
    }

    pub fn is_short(&self) -> bool {
        self.side == Some(Side::Sell) && self.quantity > 0
    }

    /// Apply a fill to this position. On reducing / closing / flipping fills,
    /// the realized P&L for the closed-out portion is accumulated into
    /// `realized_pnl`.
    ///
    /// Realized P&L for a closed portion = `(exit - entry) * closed_qty * direction`
    /// where `direction = +1` for an originally-long position, `-1` for short.
    /// All quantities are quarter-ticks (FixedPrice raw units).
    pub fn apply_fill(&mut self, fill: &FillEvent) {
        // Rejections never alter position arithmetic.
        if matches!(fill.fill_type, FillType::Rejected { .. }) {
            return;
        }
        if fill.fill_size == 0 {
            return;
        }

        if self.quantity == 0 {
            // Opening a new position — no realized P&L.
            self.side = Some(fill.side);
            self.quantity = fill.fill_size;
            self.avg_entry_price = fill.fill_price;
            return;
        }

        if self.side == Some(fill.side) {
            // Same side: increase position with weighted average entry price.
            // No realized P&L on additions.
            let total_cost = self
                .avg_entry_price
                .saturating_mul(self.quantity as i64)
                .saturating_add(fill.fill_price.saturating_mul(fill.fill_size as i64));
            let new_qty = self.quantity.saturating_add(fill.fill_size);
            self.avg_entry_price = FixedPrice::new(total_cost.raw() / new_qty as i64);
            self.quantity = new_qty;
        } else {
            // Opposite side: reduce, close, or flip.
            // Realized P&L for the closed-out portion is computed against
            // the current `avg_entry_price` (weighted-average cost basis).
            // The remaining portion (after a flip) starts a fresh leg with
            // `avg_entry_price = fill.fill_price`.
            let direction: i64 = match self.side {
                Some(Side::Buy) => 1,
                Some(Side::Sell) => -1,
                None => 0,
            };
            let closed_qty = fill.fill_size.min(self.quantity);
            let leg_pnl = fill
                .fill_price
                .saturating_sub(self.avg_entry_price)
                .saturating_mul(closed_qty as i64)
                .saturating_mul(direction)
                .raw();
            self.realized_pnl = self.realized_pnl.saturating_add(leg_pnl);

            if fill.fill_size < self.quantity {
                // Partial close: avg_entry_price stays unchanged for the
                // remaining position.
                self.quantity -= fill.fill_size;
            } else if fill.fill_size == self.quantity {
                // Exact close: position goes flat. realized_pnl preserved.
                let preserved_pnl = self.realized_pnl;
                *self = Position::flat(self.symbol_id);
                self.realized_pnl = preserved_pnl;
            } else {
                // Flip: previous leg fully closed, new leg opens in the
                // opposite direction at `fill.fill_price`. realized_pnl
                // already accumulated for the closed leg (above).
                self.quantity = fill.fill_size - self.quantity;
                self.side = Some(fill.side);
                self.avg_entry_price = fill.fill_price;
                // unrealized_pnl is recomputed by the next mid-price update.
                self.unrealized_pnl = 0;
            }
        }
    }

    /// Update unrealized P&L (in quarter-ticks) using the supplied current
    /// market price. Call this each time the order book mid-price advances.
    ///
    /// `(current - avg_entry) * quantity * direction`
    pub fn update_unrealized_pnl(&mut self, current_price: FixedPrice) {
        if self.quantity == 0 {
            self.unrealized_pnl = 0;
            return;
        }

        let diff = current_price.saturating_sub(self.avg_entry_price);
        let direction = match self.side {
            Some(Side::Buy) => 1i64,
            Some(Side::Sell) => -1i64,
            None => 0i64,
        };
        self.unrealized_pnl = diff
            .saturating_mul(self.quantity as i64)
            .saturating_mul(direction)
            .raw();
    }
}

/// Broker-reported view of a position used during reconciliation.
///
/// Distinct type from [`Position`] so the engine can compare its locally
/// derived state against what the broker reports without having to fabricate
/// `unrealized_pnl` / `realized_pnl` for the broker side. Story 4-5 introduced
/// this type alongside `BrokerAdapter::query_positions()` -> `Vec<BrokerPosition>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrokerPosition {
    pub symbol_id: u32,
    /// `None` when the broker explicitly reports flat for the symbol.
    pub side: Option<Side>,
    pub quantity: u32,
    pub avg_entry_price: FixedPrice,
}

impl BrokerPosition {
    pub fn flat(symbol_id: u32) -> Self {
        Self {
            symbol_id,
            side: None,
            quantity: 0,
            avg_entry_price: FixedPrice::default(),
        }
    }

    pub fn is_flat(&self) -> bool {
        self.quantity == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FillType, UnixNanos};

    fn make_fill(side: Side, price_raw: i64, size: u32) -> FillEvent {
        FillEvent {
            order_id: 1,
            fill_price: FixedPrice::new(price_raw),
            fill_size: size,
            timestamp: UnixNanos::default(),
            side,
            decision_id: 0,
            fill_type: FillType::Full,
        }
    }

    #[test]
    fn buy_fill_creates_long() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 2));
        assert!(pos.is_long());
        assert_eq!(pos.quantity, 2);
        assert_eq!(pos.avg_entry_price.raw(), 100);
    }

    #[test]
    fn sell_fill_reduces_long() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 5));
        pos.apply_fill(&make_fill(Side::Sell, 110, 3));
        assert!(pos.is_long());
        assert_eq!(pos.quantity, 2);
    }

    #[test]
    fn opposite_fill_flips_position() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 2));
        pos.apply_fill(&make_fill(Side::Sell, 110, 5));
        assert!(pos.is_short());
        assert_eq!(pos.quantity, 3);
        assert_eq!(pos.avg_entry_price.raw(), 110);
    }

    #[test]
    fn exact_close_gives_flat() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 3));
        pos.apply_fill(&make_fill(Side::Sell, 110, 3));
        assert!(pos.is_flat());
    }

    #[test]
    fn unrealized_pnl_long() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 2));
        pos.update_unrealized_pnl(FixedPrice::new(110));
        // (110 - 100) * 2 * 1 = 20
        assert_eq!(pos.unrealized_pnl, 20);
    }

    #[test]
    fn unrealized_pnl_short() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Sell, 100, 2));
        pos.update_unrealized_pnl(FixedPrice::new(90));
        // (90 - 100) * 2 * -1 = 20
        assert_eq!(pos.unrealized_pnl, 20);
    }

    #[test]
    fn pnl_uses_integer_arithmetic() {
        let mut pos = Position::flat(1);
        // Use quarter-tick values directly
        pos.apply_fill(&make_fill(Side::Buy, 17929, 1)); // 4482.25
        pos.update_unrealized_pnl(FixedPrice::new(17930)); // 4482.50
        // (17930 - 17929) * 1 * 1 = 1 quarter-tick
        assert_eq!(pos.unrealized_pnl, 1);
    }

    #[test]
    fn weighted_avg_entry_price() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 2)); // avg = 100
        pos.apply_fill(&make_fill(Side::Buy, 200, 2)); // avg = (100*2 + 200*2) / 4 = 150
        assert_eq!(pos.avg_entry_price.raw(), 150);
    }

    /// Story 4-5 Task 9.2: closing fill computes realized P&L correctly.
    #[test]
    fn close_fill_accumulates_realized_pnl() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 3)); // long 3 @ 100
        pos.apply_fill(&make_fill(Side::Sell, 110, 3)); // close @ 110
        assert!(pos.is_flat());
        // (110 - 100) * 3 * 1 = +30 quarter-ticks
        assert_eq!(pos.realized_pnl, 30);
    }

    /// Story 4-5 Task 9.3: partial close: realized P&L for closed portion,
    /// avg_entry unchanged for remainder.
    #[test]
    fn partial_close_computes_realized_pnl_for_closed_portion() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 5)); // long 5 @ 100
        pos.apply_fill(&make_fill(Side::Sell, 120, 2)); // close 2 @ 120
        assert!(pos.is_long());
        assert_eq!(pos.quantity, 3);
        // avg_entry unchanged for remaining position
        assert_eq!(pos.avg_entry_price.raw(), 100);
        // (120 - 100) * 2 * 1 = 40
        assert_eq!(pos.realized_pnl, 40);
    }

    /// Story 4-5: short partial close also accumulates realized P&L correctly.
    #[test]
    fn short_partial_close_accumulates_realized_pnl() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Sell, 200, 4)); // short 4 @ 200
        pos.apply_fill(&make_fill(Side::Buy, 180, 1)); // close 1 @ 180
        assert!(pos.is_short());
        assert_eq!(pos.quantity, 3);
        // (180 - 200) * 1 * -1 = +20 (short profits when price drops)
        assert_eq!(pos.realized_pnl, 20);
    }

    /// Story 4-5: flip position — full close of leg + new leg with fresh avg.
    #[test]
    fn flip_position_realizes_pnl_on_closed_leg() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 2)); // long 2 @ 100
        pos.apply_fill(&make_fill(Side::Sell, 110, 5)); // close 2, flip short 3 @ 110
        assert!(pos.is_short());
        assert_eq!(pos.quantity, 3);
        assert_eq!(pos.avg_entry_price.raw(), 110);
        // Closed-leg P&L: (110 - 100) * 2 * 1 = +20
        assert_eq!(pos.realized_pnl, 20);
        // Unrealized P&L is reset post-flip until next mid update.
        assert_eq!(pos.unrealized_pnl, 0);
    }

    /// Story 4-5: realized P&L accumulates across multiple closes.
    #[test]
    fn realized_pnl_accumulates_across_multiple_closes() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 5)); // long 5 @ 100
        pos.apply_fill(&make_fill(Side::Sell, 110, 2)); // +20
        pos.apply_fill(&make_fill(Side::Sell, 120, 1)); // +20
        pos.apply_fill(&make_fill(Side::Sell, 90, 2)); //  -20
        assert!(pos.is_flat());
        assert_eq!(pos.realized_pnl, 20);
    }

    /// Story 4-5: rejection fills don't disturb position state.
    #[test]
    fn rejected_fill_is_noop() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 2));
        let mut reject = make_fill(Side::Sell, 0, 0);
        reject.fill_type = FillType::Rejected {
            reason: crate::types::RejectReason::InsufficientMargin,
        };
        pos.apply_fill(&reject);
        assert_eq!(pos.quantity, 2);
        assert!(pos.is_long());
        assert_eq!(pos.realized_pnl, 0);
    }

    /// Story 4-5: BrokerPosition flat constructor.
    #[test]
    fn broker_position_flat_constructor() {
        let bp = BrokerPosition::flat(7);
        assert_eq!(bp.symbol_id, 7);
        assert!(bp.is_flat());
        assert_eq!(bp.side, None);
        assert_eq!(bp.quantity, 0);
    }
}
