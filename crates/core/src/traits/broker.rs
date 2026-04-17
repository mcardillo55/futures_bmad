use crate::types::{OrderParams, OrderState, Position};

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("connection lost: {0}")]
    ConnectionLost(String),
    #[error("order {order_id} rejected: {reason}")]
    OrderRejected { order_id: u64, reason: String },
    #[error("deserialization failed: {0}")]
    DeserializationFailed(String),
    #[error("timeout on {operation} after {duration_ms}ms")]
    Timeout { operation: String, duration_ms: u64 },
    #[error("position query failed: {0}")]
    PositionQueryFailed(String),
}

/// Trait for broker connectivity. Async because broker communication is I/O-bound.
#[async_trait::async_trait]
pub trait BrokerAdapter: Send + Sync {
    async fn subscribe(&mut self, symbol: &str) -> Result<(), BrokerError>;
    async fn submit_order(&mut self, params: OrderParams) -> Result<u64, BrokerError>;
    async fn cancel_order(&mut self, order_id: u64) -> Result<(), BrokerError>;
    async fn query_positions(&self) -> Result<Vec<Position>, BrokerError>;
    async fn query_open_orders(&self) -> Result<Vec<(u64, OrderState)>, BrokerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_error_display() {
        let err = BrokerError::ConnectionLost("network down".into());
        assert_eq!(err.to_string(), "connection lost: network down");

        let err = BrokerError::OrderRejected { order_id: 42, reason: "insufficient margin".into() };
        assert_eq!(err.to_string(), "order 42 rejected: insufficient margin");

        let err = BrokerError::Timeout { operation: "submit".into(), duration_ms: 5000 };
        assert_eq!(err.to_string(), "timeout on submit after 5000ms");
    }
}
