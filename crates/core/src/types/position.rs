use super::{FixedPrice, Side};
use crate::events::FillEvent;

/// Tracks the current position for a symbol.
#[derive(Debug, Clone, Copy)]
pub struct Position {
    pub symbol_id: u32,
    pub side: Option<Side>,
    pub quantity: u32,
    pub avg_entry_price: FixedPrice,
    pub unrealized_pnl: FixedPrice,
}

impl Position {
    pub fn flat(symbol_id: u32) -> Self {
        Self {
            symbol_id,
            side: None,
            quantity: 0,
            avg_entry_price: FixedPrice::default(),
            unrealized_pnl: FixedPrice::default(),
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

    pub fn apply_fill(&mut self, fill: &FillEvent) {
        if self.quantity == 0 {
            // Opening a new position
            self.side = Some(fill.side);
            self.quantity = fill.fill_size;
            self.avg_entry_price = fill.fill_price;
            return;
        }

        if self.side == Some(fill.side) {
            // Same side: increase position with weighted average entry price
            let total_cost = self
                .avg_entry_price
                .saturating_mul(self.quantity as i64)
                .saturating_add(fill.fill_price.saturating_mul(fill.fill_size as i64));
            let new_qty = self.quantity + fill.fill_size;
            self.avg_entry_price = FixedPrice::new(total_cost.raw() / new_qty as i64);
            self.quantity = new_qty;
        } else {
            // Opposite side: reduce or flip position
            if fill.fill_size < self.quantity {
                self.quantity -= fill.fill_size;
            } else if fill.fill_size == self.quantity {
                *self = Position::flat(self.symbol_id);
            } else {
                // Flip
                self.quantity = fill.fill_size - self.quantity;
                self.side = Some(fill.side);
                self.avg_entry_price = fill.fill_price;
            }
        }
    }

    pub fn update_unrealized_pnl(&mut self, current_price: FixedPrice) {
        if self.quantity == 0 {
            self.unrealized_pnl = FixedPrice::default();
            return;
        }

        let diff = current_price.saturating_sub(self.avg_entry_price);
        let direction = match self.side {
            Some(Side::Buy) => 1i64,
            Some(Side::Sell) => -1i64,
            None => 0i64,
        };
        self.unrealized_pnl = diff.saturating_mul(self.quantity as i64).saturating_mul(direction);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UnixNanos;

    fn make_fill(side: Side, price_raw: i64, size: u32) -> FillEvent {
        FillEvent {
            order_id: 1,
            fill_price: FixedPrice::new(price_raw),
            fill_size: size,
            timestamp: UnixNanos::default(),
            side,
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
        assert_eq!(pos.unrealized_pnl.raw(), 20);
    }

    #[test]
    fn unrealized_pnl_short() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Sell, 100, 2));
        pos.update_unrealized_pnl(FixedPrice::new(90));
        // (90 - 100) * 2 * -1 = 20
        assert_eq!(pos.unrealized_pnl.raw(), 20);
    }

    #[test]
    fn pnl_uses_integer_arithmetic() {
        let mut pos = Position::flat(1);
        // Use quarter-tick values directly
        pos.apply_fill(&make_fill(Side::Buy, 17929, 1)); // 4482.25
        pos.update_unrealized_pnl(FixedPrice::new(17930)); // 4482.50
        // (17930 - 17929) * 1 * 1 = 1 quarter-tick
        assert_eq!(pos.unrealized_pnl.raw(), 1);
    }

    #[test]
    fn weighted_avg_entry_price() {
        let mut pos = Position::flat(1);
        pos.apply_fill(&make_fill(Side::Buy, 100, 2)); // avg = 100
        pos.apply_fill(&make_fill(Side::Buy, 200, 2)); // avg = (100*2 + 200*2) / 4 = 150
        assert_eq!(pos.avg_entry_price.raw(), 150);
    }
}
