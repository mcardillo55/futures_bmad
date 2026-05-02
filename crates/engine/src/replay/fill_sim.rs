//! Fill simulation for replay mode.
//!
//! The replay path needs to answer "what fill would the broker have produced
//! for this engine-submitted order?" without contacting an exchange. The V1
//! model is [`FillModel::ImmediateAtMarket`]: every order is filled in full,
//! immediately, at the current best bid (sells) or best ask (buys).
//!
//! Fidelity boundaries (per architecture doc, NFR15 / FR29 commentary):
//! replay CANNOT model queue position, partial fills, market impact, or
//! liquidity variation. Use replay to verify *logic correctness*, NOT P&L
//! prediction.

use futures_bmad_broker::FillQueueProducer;
use futures_bmad_core::{FillEvent, FillType, FixedPrice, OrderBook, OrderEvent, Side};
use tracing::{debug, warn};

/// Fill simulation model used by [`MockFillSimulator`].
///
/// V1 ships a single variant: every order fills immediately at the current
/// best bid/ask. Subsequent fidelity tiers (slippage, queue position,
/// partial fills) will be added as new variants without breaking the
/// existing wiring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillModel {
    /// Immediately fill the full order quantity at the current best
    /// bid (for Sell orders) or best ask (for Buy orders). Used by Story
    /// 7.1's V1 replay path.
    ImmediateAtMarket,
}

/// Fill simulator wired between the broker [`OrderQueueConsumer`] (engine →
/// broker) and the [`FillQueueProducer`] (broker → engine) during replay.
///
/// The simulator does NOT own either queue; the [`crate::replay::ReplayOrchestrator`]
/// owns them and calls [`MockFillSimulator::simulate`] for each order it pulls
/// off the order queue.
pub struct MockFillSimulator {
    model: FillModel,
    /// Monotonic fill counter — populates `FillEvent.order_id` is already set
    /// from the source order; this is internal accounting for the simulator's
    /// own fill_id sequence (Story 7.1 Task 5.3).
    next_fill_id: u64,
    /// Cumulative fills emitted so far.
    fills_emitted: u64,
    /// Orders dropped because the FillQueue producer was full (should never
    /// happen in replay, but counted for diagnostics).
    fills_dropped: u64,
}

impl MockFillSimulator {
    pub fn new(model: FillModel) -> Self {
        Self {
            model,
            next_fill_id: 1,
            fills_emitted: 0,
            fills_dropped: 0,
        }
    }

    pub fn fills_emitted(&self) -> u64 {
        self.fills_emitted
    }

    pub fn fills_dropped(&self) -> u64 {
        self.fills_dropped
    }

    pub fn next_fill_id(&self) -> u64 {
        self.next_fill_id
    }

    /// Advance the internal fill_id sequence and return the next id.
    pub fn allocate_fill_id(&mut self) -> u64 {
        let id = self.next_fill_id;
        self.next_fill_id = self.next_fill_id.saturating_add(1);
        id
    }

    /// Build the synthetic [`FillEvent`] that would result from executing
    /// `order` against `book` under the configured [`FillModel`]. Returns
    /// `None` when the book has no liquidity on the relevant side (no best
    /// bid for a sell, no best ask for a buy) — the caller is expected to
    /// drop the order and log.
    pub fn synth_fill(&mut self, order: &OrderEvent, book: &OrderBook) -> Option<FillEvent> {
        match self.model {
            FillModel::ImmediateAtMarket => {
                let fill_price = best_price_for(order, book)?;
                let _ = self.allocate_fill_id();
                Some(FillEvent {
                    order_id: order.order_id,
                    fill_price,
                    fill_size: order.quantity,
                    timestamp: order.timestamp,
                    side: order.side,
                    decision_id: order.decision_id,
                    fill_type: FillType::Full,
                })
            }
        }
    }

    /// Synthesize a fill for `order` and push it onto `producer`. Returns
    /// `true` if the fill made it onto the queue.
    pub fn simulate(
        &mut self,
        order: &OrderEvent,
        book: &OrderBook,
        producer: &mut FillQueueProducer,
    ) -> bool {
        let Some(fill) = self.synth_fill(order, book) else {
            warn!(
                target: "replay::fill_sim",
                order_id = order.order_id,
                decision_id = order.decision_id,
                ?order.side,
                "no liquidity in replay book — dropping simulated fill"
            );
            return false;
        };
        if producer.try_push(fill) {
            self.fills_emitted += 1;
            debug!(
                target: "replay::fill_sim",
                order_id = order.order_id,
                decision_id = order.decision_id,
                fill_price = fill.fill_price.raw(),
                fill_size = fill.fill_size,
                "simulated fill pushed"
            );
            true
        } else {
            self.fills_dropped += 1;
            warn!(
                target: "replay::fill_sim",
                order_id = order.order_id,
                decision_id = order.decision_id,
                fills_dropped = self.fills_dropped,
                "FillQueue full during replay — dropping simulated fill"
            );
            false
        }
    }
}

/// Resolve the price at which `order` would fill under [`FillModel::ImmediateAtMarket`].
///
/// Buys lift the best ask, sells hit the best bid. Limit and stop orders
/// degenerate into the same market-fill behavior under V1 — the limit/stop
/// price is ignored and the trigger is assumed to have fired immediately.
fn best_price_for(order: &OrderEvent, book: &OrderBook) -> Option<FixedPrice> {
    // For V1 every order_type collapses to "fill at the touch on the
    // opposite side". Future fill models can branch on `order.order_type`
    // here.
    let _ = order.order_type; // suppress unused-pattern lint if branches are added later
    match order.side {
        Side::Buy => book.best_ask().map(|lvl| lvl.price),
        Side::Sell => book.best_bid().map(|lvl| lvl.price),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{Level, MarketEvent, MarketEventType, OrderBook, OrderType, UnixNanos};

    fn book_with_quotes(bid: i64, ask: i64) -> OrderBook {
        let mut book = OrderBook::empty();
        book.update_bid(
            0,
            Level {
                price: FixedPrice::new(bid),
                size: 100,
                order_count: 0,
            },
        );
        book.update_ask(
            0,
            Level {
                price: FixedPrice::new(ask),
                size: 100,
                order_count: 0,
            },
        );
        book
    }

    fn buy_order(qty: u32) -> OrderEvent {
        OrderEvent {
            order_id: 1,
            symbol_id: 0,
            side: Side::Buy,
            quantity: qty,
            order_type: OrderType::Market,
            decision_id: 7,
            timestamp: UnixNanos::new(1_000_000),
        }
    }

    fn sell_order(qty: u32) -> OrderEvent {
        OrderEvent {
            order_id: 2,
            symbol_id: 0,
            side: Side::Sell,
            quantity: qty,
            order_type: OrderType::Market,
            decision_id: 8,
            timestamp: UnixNanos::new(1_000_001),
        }
    }

    /// 5.1 — Buy lifts the best ask; full quantity, immediate fill.
    #[test]
    fn buy_fills_at_best_ask() {
        let book = book_with_quotes(18_000, 18_004);
        let mut sim = MockFillSimulator::new(FillModel::ImmediateAtMarket);
        let fill = sim.synth_fill(&buy_order(3), &book).unwrap();
        assert_eq!(fill.fill_price.raw(), 18_004);
        assert_eq!(fill.fill_size, 3);
        assert_eq!(fill.side, Side::Buy);
        assert!(matches!(fill.fill_type, FillType::Full));
        assert_eq!(fill.decision_id, 7);
    }

    /// 5.1 — Sell hits the best bid.
    #[test]
    fn sell_fills_at_best_bid() {
        let book = book_with_quotes(18_000, 18_004);
        let mut sim = MockFillSimulator::new(FillModel::ImmediateAtMarket);
        let fill = sim.synth_fill(&sell_order(2), &book).unwrap();
        assert_eq!(fill.fill_price.raw(), 18_000);
        assert_eq!(fill.fill_size, 2);
        assert_eq!(fill.side, Side::Sell);
    }

    /// 5.3 — Each simulated fill consumes a unique fill_id from the
    /// simulator's monotonic counter.
    #[test]
    fn fill_ids_are_unique_and_monotonic() {
        let book = book_with_quotes(18_000, 18_004);
        let mut sim = MockFillSimulator::new(FillModel::ImmediateAtMarket);
        let id1 = sim.allocate_fill_id();
        let id2 = sim.allocate_fill_id();
        let id3 = sim.allocate_fill_id();
        assert_eq!((id1, id2, id3), (1, 2, 3));
        // synth_fill also bumps the counter.
        let _ = sim.synth_fill(&buy_order(1), &book).unwrap();
        assert_eq!(sim.next_fill_id(), 5);
    }

    /// 5.2 — When pushed through a FillQueueProducer, fills are countable
    /// and the simulator's accounting reflects success.
    #[test]
    fn simulate_pushes_fill_to_queue() {
        use futures_bmad_broker::create_order_fill_queues;
        let (_op, _oc, mut fp, mut fc) = create_order_fill_queues();
        let book = book_with_quotes(18_000, 18_004);
        let mut sim = MockFillSimulator::new(FillModel::ImmediateAtMarket);
        let pushed = sim.simulate(&buy_order(1), &book, &mut fp);
        assert!(pushed);
        assert_eq!(sim.fills_emitted(), 1);
        let fill = fc.try_pop().unwrap();
        assert_eq!(fill.fill_price.raw(), 18_004);
        // We didn't fabricate a MarketEvent for this; suppress unused-import lint.
        let _e = MarketEvent {
            timestamp: UnixNanos::new(0),
            symbol_id: 0,
            sequence: 0,
            event_type: MarketEventType::Trade,
            price: FixedPrice::new(0),
            size: 0,
            side: None,
        };
    }

    /// No-liquidity book ⇒ no synthetic fill, dropped count stays at zero
    /// (we only count drops at the FillQueue boundary, not no-liquidity
    /// drops which surface as a warn log).
    #[test]
    fn empty_book_yields_no_fill() {
        let book = OrderBook::empty();
        let mut sim = MockFillSimulator::new(FillModel::ImmediateAtMarket);
        assert!(sim.synth_fill(&buy_order(1), &book).is_none());
        assert!(sim.synth_fill(&sell_order(1), &book).is_none());
    }
}
