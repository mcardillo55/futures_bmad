use super::{FixedPrice, Side};

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
            Err(OrderStateError { from: *self, to: next })
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, OrderState::Filled | OrderState::Rejected | OrderState::Cancelled | OrderState::Resolved)
    }
}

// --- Order Type & Params ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
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
}

#[derive(Debug, Clone, Copy)]
pub struct OrderParams {
    pub symbol_id: u32,
    pub side: Side,
    pub quantity: u32,
    pub order_type: OrderType,
    pub price: Option<FixedPrice>,
}

impl OrderParams {
    pub fn validate(&self) -> Result<(), OrderParamsError> {
        match self.order_type {
            OrderType::Market => {
                if self.price.is_some() {
                    return Err(OrderParamsError::UnexpectedPrice);
                }
            }
            OrderType::Limit | OrderType::Stop => {
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
        if entry.order_type != OrderType::Market {
            return Err(BracketOrderError::EntryNotMarket);
        }
        if take_profit.order_type != OrderType::Limit {
            return Err(BracketOrderError::TakeProfitNotLimit);
        }
        if stop_loss.order_type != OrderType::Stop {
            return Err(BracketOrderError::StopLossNotStop);
        }

        let exit_side = match entry.side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        if take_profit.side != exit_side || stop_loss.side != exit_side {
            return Err(BracketOrderError::InconsistentSides);
        }

        Ok(Self { entry, take_profit, stop_loss })
    }
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
            order_type: OrderType::Market,
            price: None,
        };
        assert!(market.validate().is_ok());

        let bad_market = OrderParams { price: Some(FixedPrice::new(100)), ..market };
        assert!(bad_market.validate().is_err());

        let limit = OrderParams {
            order_type: OrderType::Limit,
            price: Some(FixedPrice::new(100)),
            ..market
        };
        assert!(limit.validate().is_ok());

        let bad_limit = OrderParams { order_type: OrderType::Limit, price: None, ..market };
        assert!(bad_limit.validate().is_err());
    }

    #[test]
    fn bracket_order_validation() {
        let entry = OrderParams {
            symbol_id: 1,
            side: Side::Buy,
            quantity: 1,
            order_type: OrderType::Market,
            price: None,
        };
        let tp = OrderParams {
            symbol_id: 1,
            side: Side::Sell,
            quantity: 1,
            order_type: OrderType::Limit,
            price: Some(FixedPrice::new(200)),
        };
        let sl = OrderParams {
            symbol_id: 1,
            side: Side::Sell,
            quantity: 1,
            order_type: OrderType::Stop,
            price: Some(FixedPrice::new(50)),
        };

        assert!(BracketOrder::new(entry, tp, sl).is_ok());

        // Wrong entry type
        let bad_entry = OrderParams { order_type: OrderType::Limit, price: Some(FixedPrice::new(100)), ..entry };
        assert!(matches!(BracketOrder::new(bad_entry, tp, sl), Err(BracketOrderError::EntryNotMarket)));

        // Wrong sides
        let bad_tp = OrderParams { side: Side::Buy, ..tp };
        assert!(matches!(BracketOrder::new(entry, bad_tp, sl), Err(BracketOrderError::InconsistentSides)));
    }
}
