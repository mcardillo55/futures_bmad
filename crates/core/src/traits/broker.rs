use crate::types::{BrokerPosition, OrderParams, OrderState};

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("connection lost: {0}")]
    ConnectionLost(String),
    #[error("authentication failed: {0}")]
    AuthenticationFailed(String),
    #[error("subscription failed: {0}")]
    SubscriptionFailed(String),
    #[error("protocol error: {0}")]
    ProtocolError(String),
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
    async fn connect(&mut self) -> Result<(), BrokerError>;
    async fn disconnect(&mut self) -> Result<(), BrokerError>;
    async fn subscribe(&mut self, symbol: &str) -> Result<(), BrokerError>;
    async fn submit_order(&mut self, params: OrderParams) -> Result<u64, BrokerError>;
    async fn cancel_order(&mut self, order_id: u64) -> Result<(), BrokerError>;
    /// Story 4-5: returns the broker's view of open positions for
    /// reconciliation. Empty Vec means flat across all symbols. The engine's
    /// `PositionTracker::reconcile` compares this view against locally derived
    /// state and trips the circuit breaker on mismatch (NFR16: never silently
    /// correct).
    async fn query_positions(&self) -> Result<Vec<BrokerPosition>, BrokerError>;
    async fn query_open_orders(&self) -> Result<Vec<(u64, OrderState)>, BrokerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_error_display() {
        let err = BrokerError::ConnectionLost("network down".into());
        assert_eq!(err.to_string(), "connection lost: network down");

        let err = BrokerError::AuthenticationFailed("invalid credentials".into());
        assert_eq!(
            err.to_string(),
            "authentication failed: invalid credentials"
        );

        let err = BrokerError::SubscriptionFailed("unknown symbol".into());
        assert_eq!(err.to_string(), "subscription failed: unknown symbol");

        let err = BrokerError::ProtocolError("unexpected message type".into());
        assert_eq!(err.to_string(), "protocol error: unexpected message type");

        let err = BrokerError::OrderRejected {
            order_id: 42,
            reason: "insufficient margin".into(),
        };
        assert_eq!(err.to_string(), "order 42 rejected: insufficient margin");

        let err = BrokerError::Timeout {
            operation: "submit".into(),
            duration_ms: 5000,
        };
        assert_eq!(err.to_string(), "timeout on submit after 5000ms");
    }
}
