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
                // Carryover (4-2 S-3): broker can fill 1-of-N then reject the
                // remainder; without this arc the order would strand in
                // PartialFill indefinitely. The terminal state is Rejected so
                // operators see "the rest got rejected" in the audit trail.
                | (PartialFill, Rejected)
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

/// Lifecycle state of an [`BracketOrder`].
///
/// Tracked per active position. Transitions are linear during normal entry submission
/// (`NoBracket -> EntryOnly -> EntryAndStop -> Full`) and any non-terminal state may
/// jump directly to `Flattening` when the flatten retry path engages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BracketState {
    /// No bracket has been registered yet for the underlying decision.
    NoBracket,
    /// Entry has filled; SL submission in flight (position is unprotected — narrow window).
    EntryOnly,
    /// Stop-loss is resting at exchange; TP submission in flight.
    EntryAndStop,
    /// Both legs resting at exchange — fully protected.
    Full,
    /// Flatten retry has engaged (entry filled but bracket submission failed).
    Flattening,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid bracket state transition: {from:?} -> {to:?}")]
pub struct BracketStateError {
    pub from: BracketState,
    pub to: BracketState,
}

impl BracketState {
    /// Validate the linear forward transition graph. The `Flattening` state is
    /// reachable from any non-terminal state since the flatten retry path may
    /// engage at any point between entry submission and full bracket protection.
    pub fn can_transition_to(&self, next: BracketState) -> bool {
        use BracketState::*;
        // Any -> Flattening is always allowed (escape hatch for the flatten path).
        if matches!(next, Flattening) {
            return !matches!(self, Flattening);
        }
        matches!(
            (self, next),
            (NoBracket, EntryOnly) | (EntryOnly, EntryAndStop) | (EntryAndStop, Full)
        )
    }
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

/// Logical bracket order tracked per position.
///
/// Carries the three-leg order shape (Market entry + Limit TP + Stop SL) plus the
/// per-bracket lifecycle state. The struct is `Copy` because every field is `Copy`,
/// but it is conceptually mutable: the [`BracketManager`] in
/// `engine::order_manager::bracket` keeps a mutable reference and walks the state
/// machine via [`BracketOrder::transition`].
///
/// Note (story 4.3 dev deviation): the spec calls for `entry: OrderEvent` etc., but
/// the architecture doc and the existing pre-routing-event API both used
/// [`OrderParams`]. Keeping `OrderParams` here means the `BracketOrder` describes
/// *what to submit* without prematurely allocating routing-event fields
/// (`order_id`, `timestamp`) — those are attached by the [`BracketManager`] when
/// each leg is actually pushed onto the [`OrderQueueProducer`] in the engine.
#[derive(Debug, Clone, Copy)]
pub struct BracketOrder {
    pub bracket_id: u64,
    pub decision_id: u64,
    pub entry: OrderParams,
    pub take_profit: OrderParams,
    pub stop_loss: OrderParams,
    pub state: BracketState,
}

impl BracketOrder {
    /// Construct from already-validated [`OrderParams`]. Used by the legacy bracket
    /// API (signal evaluators that build `OrderParams` directly) and by the more
    /// ergonomic `from_decision` helper below.
    pub fn new(
        bracket_id: u64,
        decision_id: u64,
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
            bracket_id,
            decision_id,
            entry,
            take_profit,
            stop_loss,
            state: BracketState::NoBracket,
        })
    }

    /// Convenience constructor from a trade decision.
    ///
    /// Builds the three legs (Market entry, Limit TP at `tp_price`, Stop SL at
    /// `sl_price`) with the exit side computed as the opposite of `side`. Initial
    /// state is `NoBracket` (per Task 1.3).
    pub fn from_decision(
        bracket_id: u64,
        decision_id: u64,
        symbol_id: u32,
        side: Side,
        quantity: u32,
        tp_price: FixedPrice,
        sl_price: FixedPrice,
    ) -> Result<Self, BracketOrderError> {
        let exit_side = match side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let entry = OrderParams {
            symbol_id,
            side,
            quantity,
            order_type: OrderKind::Market,
            price: None,
        };
        let take_profit = OrderParams {
            symbol_id,
            side: exit_side,
            quantity,
            order_type: OrderKind::Limit,
            price: Some(tp_price),
        };
        let stop_loss = OrderParams {
            symbol_id,
            side: exit_side,
            quantity,
            order_type: OrderKind::Stop,
            price: Some(sl_price),
        };
        Self::new(bracket_id, decision_id, entry, take_profit, stop_loss)
    }

    /// Transition the bracket to the next state, validated against the bracket
    /// state machine. Returns `Err` (with the offending arc) for illegal
    /// transitions; the bracket is left untouched.
    pub fn transition(&mut self, new_state: BracketState) -> Result<(), BracketStateError> {
        if !self.state.can_transition_to(new_state) {
            return Err(BracketStateError {
                from: self.state,
                to: new_state,
            });
        }
        self.state = new_state;
        Ok(())
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

    /// Carryover (4-2 S-3): PartialFill -> Rejected is a real-world arc
    /// (broker accepts entry, fills 1-of-N, then rejects remainder). Without
    /// this arc the order strands in PartialFill indefinitely.
    #[test]
    fn partial_fill_to_rejected_is_valid() {
        assert!(OrderState::PartialFill.can_transition_to(OrderState::Rejected));
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

        let bracket = BracketOrder::new(1, 7, entry, tp, sl).unwrap();
        assert_eq!(bracket.bracket_id, 1);
        assert_eq!(bracket.decision_id, 7);
        assert_eq!(bracket.state, BracketState::NoBracket);

        // Wrong entry type
        let bad_entry = OrderParams {
            order_type: OrderKind::Limit,
            price: Some(FixedPrice::new(100)),
            ..entry
        };
        assert!(matches!(
            BracketOrder::new(1, 7, bad_entry, tp, sl),
            Err(BracketOrderError::EntryNotMarket)
        ));

        // Wrong sides
        let bad_tp = OrderParams {
            side: Side::Buy,
            ..tp
        };
        assert!(matches!(
            BracketOrder::new(1, 7, entry, bad_tp, sl),
            Err(BracketOrderError::InconsistentSides)
        ));
    }

    /// Task 6.1: BracketOrder::from_decision constructs Market/Limit/Stop legs
    /// with opposite-side exits and the supplied prices.
    #[test]
    fn bracket_order_from_decision_constructs_three_legs() {
        let bracket = BracketOrder::from_decision(
            42,
            99,
            1,
            Side::Buy,
            3,
            FixedPrice::new(200),
            FixedPrice::new(50),
        )
        .unwrap();

        assert_eq!(bracket.bracket_id, 42);
        assert_eq!(bracket.decision_id, 99);
        assert_eq!(bracket.state, BracketState::NoBracket);

        // Entry: Market, Buy, qty 3, no price.
        assert_eq!(bracket.entry.order_type, OrderKind::Market);
        assert_eq!(bracket.entry.side, Side::Buy);
        assert_eq!(bracket.entry.quantity, 3);
        assert_eq!(bracket.entry.price, None);

        // TP: Limit, Sell (opposite), qty 3, price = tp_price.
        assert_eq!(bracket.take_profit.order_type, OrderKind::Limit);
        assert_eq!(bracket.take_profit.side, Side::Sell);
        assert_eq!(bracket.take_profit.price, Some(FixedPrice::new(200)));

        // SL: Stop, Sell (opposite), qty 3, price = sl_price.
        assert_eq!(bracket.stop_loss.order_type, OrderKind::Stop);
        assert_eq!(bracket.stop_loss.side, Side::Sell);
        assert_eq!(bracket.stop_loss.price, Some(FixedPrice::new(50)));

        // Sell-entry symmetry.
        let short = BracketOrder::from_decision(
            1,
            1,
            1,
            Side::Sell,
            1,
            FixedPrice::new(50),
            FixedPrice::new(200),
        )
        .unwrap();
        assert_eq!(short.take_profit.side, Side::Buy);
        assert_eq!(short.stop_loss.side, Side::Buy);
    }

    /// Task 6.2: BracketState transitions: valid ones succeed, invalid return
    /// an error with the offending arc preserved.
    #[test]
    fn bracket_state_transitions() {
        // Linear forward path
        let mut b = BracketOrder::from_decision(
            1,
            1,
            1,
            Side::Buy,
            1,
            FixedPrice::new(200),
            FixedPrice::new(50),
        )
        .unwrap();
        assert_eq!(b.state, BracketState::NoBracket);
        b.transition(BracketState::EntryOnly).unwrap();
        b.transition(BracketState::EntryAndStop).unwrap();
        b.transition(BracketState::Full).unwrap();
        // Skipping ahead is invalid
        let mut b2 = BracketOrder::from_decision(
            2,
            2,
            1,
            Side::Buy,
            1,
            FixedPrice::new(200),
            FixedPrice::new(50),
        )
        .unwrap();
        assert!(matches!(
            b2.transition(BracketState::Full),
            Err(BracketStateError {
                from: BracketState::NoBracket,
                to: BracketState::Full
            })
        ));
        assert_eq!(b2.state, BracketState::NoBracket, "state untouched on err");

        // Flattening reachable from any non-terminal state
        b2.transition(BracketState::EntryOnly).unwrap();
        b2.transition(BracketState::Flattening).unwrap();
        // Flattening -> Flattening rejected (no idempotent self-loop)
        assert!(b2.transition(BracketState::Flattening).is_err());
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
