use crate::types::{OrderParams, OrderState, UnixNanos};

#[derive(Debug, Clone)]
pub struct OrderEvent {
    pub order_id: u64,
    pub state: OrderState,
    pub params: OrderParams,
    pub timestamp: UnixNanos,
    pub decision_id: Option<u64>,
}
