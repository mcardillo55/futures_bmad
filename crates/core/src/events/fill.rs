use crate::types::{FixedPrice, Side, UnixNanos};

#[derive(Debug, Clone, Copy)]
pub struct FillEvent {
    pub order_id: u64,
    pub fill_price: FixedPrice,
    pub fill_size: u32,
    pub timestamp: UnixNanos,
    pub side: Side,
}
