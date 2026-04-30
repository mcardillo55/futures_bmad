use super::{FixedPrice, Side, UnixNanos};

// --- Order State Machine ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderState {
    Idle,
    Submitted,
    Confirmed,
    PartialFill,
    Filled,
    Rejected,
    Cancelled,
    PendingCancel,
    Uncertain,
    PendingRecon,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid order state transition: {from:?} -> {to:?}")]
pub struct OrderStateError {
    pub from: OrderState,
    pub to: OrderState,
}

impl OrderState {
    pub fn can_transition_to(&self, next: OrderState) -> bool {
        use OrderState::*;
        matches!(
            (self, next),
            (Idle, Submitted)
                | (Submitted, Confirmed)
                | (Submitted, Rejected)
                | (Submitted, Uncertain)
                | (Confirmed, PartialFill)
                | (Confirmed, Filled)
                | (Confirmed, PendingCancel)
                | (Confirmed, Uncertain)
                | (PartialFill, PartialFill)
                | (PartialFill, Filled)
                | (PartialFill, PendingCancel)
                | (PartialFill, Uncertain)
                | (PendingCancel, Cancelled)
                | (PendingCancel, Filled)
                | (PendingCancel, Uncertain)
                | (Uncertain, PendingRecon)
                | (PendingRecon, Resolved)
        )
    }

    pub fn try_transition(&self, next: OrderState) -> Result<OrderState, OrderStateError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(OrderStateError {
                from: *self,
                to: next,
            })
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderState::Filled
                | OrderState::Rejected
                | OrderState::Cancelled
                | OrderState::Resolved
        )
    }
}

// --- Order Kind & Params ---

/// Coarse order classification used by `OrderParams`/`BracketOrder` validation.
///
/// This is a simple unit-variant enum kept for the order params/bracket validation
/// path — it intentionally does NOT embed prices, because `OrderParams` carries
/// `price: Option<FixedPrice>` separately. For the SPSC routing event (`OrderEvent`)
/// the richer [`OrderType`] enum below is used instead, where the price/trigger is
/// carried inside the variant for `Copy` efficiency on the hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Market,
    Limit,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OrderParamsError {
    #[error("Limit/Stop orders require a price")]
    MissingPrice,
    #[error("Market orders must not have a price")]
    UnexpectedPrice,
    #[error("quantity must be > 0")]
    ZeroQuantity,
}

#[derive(Debug, Clone, Copy)]
pub struct OrderParams {
    pub symbol_id: u32,
    pub side: Side,
    pub quantity: u32,
    pub order_type: OrderKind,
    pub price: Option<FixedPrice>,
}

impl OrderParams {
    pub fn validate(&self) -> Result<(), OrderParamsError> {
        if self.quantity == 0 {
            return Err(OrderParamsError::ZeroQuantity);
        }
        match self.order_type {
            OrderKind::Market => {
                if self.price.is_some() {
                    return Err(OrderParamsError::UnexpectedPrice);
                }
            }
            OrderKind::Limit | OrderKind::Stop => {
                if self.price.is_none() {
                    return Err(OrderParamsError::MissingPrice);
                }
            }
        }
        Ok(())
    }
}

// --- Bracket Order ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketState {
    NoBracket,
    EntryOnly,
    EntryAndStop,
    Full,
    Flattening,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BracketOrderError {
    #[error("entry must be a Market order")]
    EntryNotMarket,
    #[error("take_profit must be a Limit order")]
    TakeProfitNotLimit,
    #[error("stop_loss must be a Stop order")]
    StopLossNotStop,
    #[error("take_profit and stop_loss must be opposite side to entry")]
    InconsistentSides,
    #[error("invalid order params: {0}")]
    InvalidParams(#[from] OrderParamsError),
}

#[derive(Debug, Clone, Copy)]
pub struct BracketOrder {
    pub entry: OrderParams,
    pub take_profit: OrderParams,
    pub stop_loss: OrderParams,
}

impl BracketOrder {
    pub fn new(
        entry: OrderParams,
        take_profit: OrderParams,
        stop_loss: OrderParams,
    ) -> Result<Self, BracketOrderError> {
        entry.validate()?;
        take_profit.validate()?;
        stop_loss.validate()?;

        if entry.order_type != OrderKind::Market {
            return Err(BracketOrderError::EntryNotMarket);
        }
        if take_profit.order_type != OrderKind::Limit {
            return Err(BracketOrderError::TakeProfitNotLimit);
        }
        if stop_loss.order_type != OrderKind::Stop {
            return Err(BracketOrderError::StopLossNotStop);
        }

        let exit_side = match entry.side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        if take_profit.side != exit_side || stop_loss.side != exit_side {
            return Err(BracketOrderError::InconsistentSides);
        }

        Ok(Self {
            entry,
            take_profit,
            stop_loss,
        })
    }
}

// --- Order Routing Event Types (Story 4.2) ---
//
// These types travel across the SPSC ring-buffer queues that connect the engine's
// order_manager to the broker's order_routing loop. They MUST be `Copy` so they can
// flow through `rtrb` without heap pointers and without `Drop` glue. `RejectReason`
// is therefore an enum (not a `String`) so the entire `OrderEvent` / `FillEvent`
// pipeline stays plain-old-data on the hot path.

/// Order type as carried on the routing queue. Unlike [`OrderKind`] the price (or
/// stop trigger) is embedded inside the variant so the slim [`OrderEvent`] does not
/// need a separate `price: Option<FixedPrice>` field — keeps the struct `Copy`-able
/// and pinned to a single source of truth for the limit/stop level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit { price: FixedPrice },
    Stop { trigger: FixedPrice },
}

impl OrderType {
    /// Coarse classification; useful for matching parity with the older [`OrderKind`].
    pub fn kind(&self) -> OrderKind {
        match self {
            OrderType::Market => OrderKind::Market,
            OrderType::Limit { .. } => OrderKind::Limit,
            OrderType::Stop { .. } => OrderKind::Stop,
        }
    }

    /// Returns the limit price or stop trigger, if any (Market returns `None`).
    pub fn price(&self) -> Option<FixedPrice> {
        match *self {
            OrderType::Market => None,
            OrderType::Limit { price } => Some(price),
            OrderType::Stop { trigger } => Some(trigger),
        }
    }
}

/// Common rejection codes carried in [`FillType::Rejected`].
///
/// Encoded as an enum (not a `String`) so the entire `FillEvent` stays `Copy` and
/// can flow through the SPSC queue without heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Account margin exhausted at the exchange.
    InsufficientMargin,
    /// Symbol not recognized by the broker.
    InvalidSymbol,
    /// Generic exchange-side reject (rate limit, halted instrument, etc).
    ExchangeReject,
    /// Connection to the broker lost mid-submission or before ack.
    ConnectionLost,
    /// Catch-all for codes we have not yet mapped.
    Unknown,
}

/// Outcome attached to a [`FillEvent`].
///
/// `Partial { remaining }` carries the un-filled quantity (post-fill remaining qty,
/// not the cumulative fill size — the consumer subtracts to derive cumulative if
/// needed). `Rejected { reason }` always uses [`RejectReason`] (no `String`) so the
/// fill event remains `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillType {
    Full,
    Partial { remaining: u32 },
    Rejected { reason: RejectReason },
}

/// Engine -> broker order submission event.
///
/// `Copy` and pointer-free so it can sit in an `rtrb` ring buffer without heap
/// indirection. The `decision_id` flows from signal evaluation through to the
/// downstream `FillEvent` for end-to-end causality tracing (NFR17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderEvent {
    pub order_id: u64,
    pub symbol_id: u32,
    pub side: Side,
    pub quantity: u32,
    pub order_type: OrderType,
    pub decision_id: u64,
    pub timestamp: UnixNanos,
}

/// Broker -> engine fill / rejection event.
///
/// `Copy` for the same SPSC reasons as [`OrderEvent`]. `decision_id` is propagated
/// from the originating `OrderEvent` so journal entries chain back to the signal
/// that produced the trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillEvent {
    pub order_id: u64,
    pub fill_price: FixedPrice,
    pub fill_size: u32,
    pub timestamp: UnixNanos,
    pub side: Side,
    pub decision_id: u64,
    pub fill_type: FillType,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_state_transitions() {
        assert!(OrderState::Idle.can_transition_to(OrderState::Submitted));
        assert!(OrderState::Submitted.can_transition_to(OrderState::Confirmed));
        assert!(OrderState::Confirmed.can_transition_to(OrderState::Filled));
        assert!(OrderState::PartialFill.can_transition_to(OrderState::PartialFill));
    }

    #[test]
    fn invalid_state_transitions() {
        assert!(!OrderState::Idle.can_transition_to(OrderState::Filled));
        assert!(!OrderState::Filled.can_transition_to(OrderState::Idle));
        assert!(!OrderState::Rejected.can_transition_to(OrderState::Submitted));
    }

    #[test]
    fn terminal_states() {
        assert!(OrderState::Filled.is_terminal());
        assert!(OrderState::Rejected.is_terminal());
        assert!(OrderState::Cancelled.is_terminal());
        assert!(OrderState::Resolved.is_terminal());
        assert!(!OrderState::Idle.is_terminal());
    }

    #[test]
    fn order_params_validation() {
        let market = OrderParams {
            symbol_id: 1,
            side: Side::Buy,
            quantity: 1,
            order_type: OrderKind::Market,
            price: None,
        };
        assert!(market.validate().is_ok());

        let bad_market = OrderParams {
            price: Some(FixedPrice::new(100)),
            ..market
        };
        assert!(bad_market.validate().is_err());

        let limit = OrderParams {
            order_type: OrderKind::Limit,
            price: Some(FixedPrice::new(100)),
            ..market
        };
        assert!(limit.validate().is_ok());

        let bad_limit = OrderParams {
            order_type: OrderKind::Limit,
            price: None,
            ..market
        };
        assert!(bad_limit.validate().is_err());
    }

    #[test]
    fn bracket_order_validation() {
        let entry = OrderParams {
            symbol_id: 1,
            side: Side::Buy,
            quantity: 1,
            order_type: OrderKind::Market,
            price: None,
        };
        let tp = OrderParams {
            symbol_id: 1,
            side: Side::Sell,
            quantity: 1,
            order_type: OrderKind::Limit,
            price: Some(FixedPrice::new(200)),
        };
        let sl = OrderParams {
            symbol_id: 1,
            side: Side::Sell,
            quantity: 1,
            order_type: OrderKind::Stop,
            price: Some(FixedPrice::new(50)),
        };

        assert!(BracketOrder::new(entry, tp, sl).is_ok());

        // Wrong entry type
        let bad_entry = OrderParams {
            order_type: OrderKind::Limit,
            price: Some(FixedPrice::new(100)),
            ..entry
        };
        assert!(matches!(
            BracketOrder::new(bad_entry, tp, sl),
            Err(BracketOrderError::EntryNotMarket)
        ));

        // Wrong sides
        let bad_tp = OrderParams {
            side: Side::Buy,
            ..tp
        };
        assert!(matches!(
            BracketOrder::new(entry, bad_tp, sl),
            Err(BracketOrderError::InconsistentSides)
        ));
    }

    // --- Story 4.2 routing-event tests ---

    /// Task 6.1: OrderEvent and FillEvent are `Copy`.
    #[test]
    fn order_and_fill_events_are_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<OrderEvent>();
        assert_copy::<FillEvent>();
        assert_copy::<OrderType>();
        assert_copy::<FillType>();
        assert_copy::<RejectReason>();
    }

    #[test]
    fn order_type_kind_and_price_extraction() {
        let market = OrderType::Market;
        assert_eq!(market.kind(), OrderKind::Market);
        assert_eq!(market.price(), None);

        let limit = OrderType::Limit {
            price: FixedPrice::new(17_929),
        };
        assert_eq!(limit.kind(), OrderKind::Limit);
        assert_eq!(limit.price(), Some(FixedPrice::new(17_929)));

        let stop = OrderType::Stop {
            trigger: FixedPrice::new(17_900),
        };
        assert_eq!(stop.kind(), OrderKind::Stop);
        assert_eq!(stop.price(), Some(FixedPrice::new(17_900)));
    }
}
