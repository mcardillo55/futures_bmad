use crate::types::{FixedPrice, Side, UnixNanos};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketEventType {
    Trade,
    BidUpdate,
    AskUpdate,
    BookSnapshot,
}

#[derive(Debug, Clone, Copy)]
pub struct MarketEvent {
    pub timestamp: UnixNanos,
    pub symbol_id: u32,
    pub event_type: MarketEventType,
    pub price: FixedPrice,
    pub size: u32,
    pub side: Option<Side>,
}
