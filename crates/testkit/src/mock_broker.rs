use futures_bmad_core::{BrokerAdapter, BrokerError, OrderParams, OrderState, Position};

#[derive(Debug, Clone)]
pub enum MockBehavior {
    Fill,
    PartialFill(u32),
    Reject(String),
    Timeout,
}

pub struct MockBrokerAdapter {
    behavior: MockBehavior,
    submitted_orders: Vec<OrderParams>,
    cancelled_orders: Vec<u64>,
    next_order_id: u64,
    subscriptions: Vec<String>,
    positions: Vec<Position>,
    open_orders: Vec<(u64, OrderState)>,
}

impl MockBrokerAdapter {
    pub fn new(behavior: MockBehavior) -> Self {
        Self {
            behavior,
            submitted_orders: Vec::new(),
            cancelled_orders: Vec::new(),
            next_order_id: 1,
            subscriptions: Vec::new(),
            positions: Vec::new(),
            open_orders: Vec::new(),
        }
    }

    pub fn submitted_orders(&self) -> &[OrderParams] {
        &self.submitted_orders
    }

    pub fn was_subscribed(&self, symbol: &str) -> bool {
        self.subscriptions.iter().any(|s| s == symbol)
    }

    pub fn order_count(&self) -> usize {
        self.submitted_orders.len()
    }

    pub fn cancelled_orders(&self) -> &[u64] {
        &self.cancelled_orders
    }

    /// Returns the partial fill quantity if behavior is `PartialFill`, else `None`.
    pub fn partial_fill_qty(&self) -> Option<u32> {
        match &self.behavior {
            MockBehavior::PartialFill(qty) => Some(*qty),
            _ => None,
        }
    }

    pub fn set_positions(&mut self, positions: Vec<Position>) {
        self.positions = positions;
    }

    pub fn set_open_orders(&mut self, orders: Vec<(u64, OrderState)>) {
        self.open_orders = orders;
    }
}

#[async_trait::async_trait]
impl BrokerAdapter for MockBrokerAdapter {
    async fn connect(&mut self) -> Result<(), BrokerError> {
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), BrokerError> {
        Ok(())
    }

    async fn subscribe(&mut self, symbol: &str) -> Result<(), BrokerError> {
        self.subscriptions.push(symbol.to_string());
        Ok(())
    }

    async fn submit_order(&mut self, params: OrderParams) -> Result<u64, BrokerError> {
        self.submitted_orders.push(params);
        let id = self.next_order_id;
        self.next_order_id += 1;

        match &self.behavior {
            MockBehavior::Fill => Ok(id),
            MockBehavior::PartialFill(_qty) => {
                // Returns success like Fill, but the partial fill quantity is available
                // via the behavior field for downstream fill simulation.
                Ok(id)
            }
            MockBehavior::Reject(reason) => Err(BrokerError::OrderRejected {
                order_id: id,
                reason: reason.clone(),
            }),
            MockBehavior::Timeout => Err(BrokerError::Timeout {
                operation: "submit_order".into(),
                duration_ms: 30000,
            }),
        }
    }

    async fn cancel_order(&mut self, order_id: u64) -> Result<(), BrokerError> {
        self.cancelled_orders.push(order_id);
        Ok(())
    }

    async fn query_positions(&self) -> Result<Vec<Position>, BrokerError> {
        Ok(self.positions.clone())
    }

    async fn query_open_orders(&self) -> Result<Vec<(u64, OrderState)>, BrokerError> {
        Ok(self.open_orders.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{OrderType, Side};

    fn test_order() -> OrderParams {
        OrderParams {
            symbol_id: 1,
            side: Side::Buy,
            quantity: 1,
            order_type: OrderType::Market,
            price: None,
        }
    }

    #[tokio::test]
    async fn fill_behavior() {
        let mut broker = MockBrokerAdapter::new(MockBehavior::Fill);
        let id = broker.submit_order(test_order()).await.unwrap();
        assert_eq!(id, 1);
        assert_eq!(broker.order_count(), 1);
    }

    #[tokio::test]
    async fn reject_behavior() {
        let mut broker = MockBrokerAdapter::new(MockBehavior::Reject("no margin".into()));
        let result = broker.submit_order(test_order()).await;
        assert!(result.is_err());
        assert_eq!(broker.order_count(), 1); // still recorded
    }

    #[tokio::test]
    async fn subscription_recording() {
        let mut broker = MockBrokerAdapter::new(MockBehavior::Fill);
        broker.subscribe("ES").await.unwrap();
        assert!(broker.was_subscribed("ES"));
        assert!(!broker.was_subscribed("NQ"));
    }
}
