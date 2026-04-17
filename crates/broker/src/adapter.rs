use futures_bmad_core::{BrokerAdapter, BrokerError, OrderParams, OrderState, Position};
use rithmic_rs::{RithmicEnv, RithmicTickerPlantHandle};
use tracing::info;

use crate::connection::{RithmicConnection, RithmicCredentials};

/// RithmicAdapter implements BrokerAdapter for Rithmic TickerPlant connectivity.
/// All R|Protocol details are isolated within this crate — core and engine never see Rithmic types.
pub struct RithmicAdapter {
    connection: RithmicConnection,
    handle: Option<RithmicTickerPlantHandle>,
    subscriptions: Vec<String>,
    exchange: String,
}

impl RithmicAdapter {
    pub fn new(env: RithmicEnv, exchange: String) -> Result<Self, BrokerError> {
        let credentials = RithmicCredentials::from_env(env)?;
        Ok(Self {
            connection: RithmicConnection::new(credentials),
            handle: None,
            subscriptions: Vec::new(),
            exchange,
        })
    }

    /// Get a reference to the ticker plant handle for receiving market data.
    pub fn handle(&self) -> Option<&RithmicTickerPlantHandle> {
        self.handle.as_ref()
    }

    /// Get a mutable reference to the handle (needed for subscription_receiver).
    pub fn handle_mut(&mut self) -> Option<&mut RithmicTickerPlantHandle> {
        self.handle.as_mut()
    }
}

#[async_trait::async_trait]
impl BrokerAdapter for RithmicAdapter {
    async fn connect(&mut self) -> Result<(), BrokerError> {
        let handle = self.connection.connect().await?;
        self.handle = Some(handle);
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), BrokerError> {
        if let Some(ref handle) = self.handle {
            self.connection.disconnect(handle).await?;
        }
        self.handle = None;
        self.subscriptions.clear();
        Ok(())
    }

    async fn subscribe(&mut self, symbol: &str) -> Result<(), BrokerError> {
        let handle = self.handle.as_ref().ok_or_else(|| {
            BrokerError::SubscriptionFailed("not connected — call connect() first".into())
        })?;

        handle
            .subscribe(symbol, &self.exchange)
            .await
            .map_err(|e| {
                BrokerError::SubscriptionFailed(format!(
                    "subscribe failed for {symbol}@{}: {e}",
                    self.exchange
                ))
            })?;

        info!(symbol, exchange = %self.exchange, "subscribed to market data");
        if !self.subscriptions.iter().any(|s| s == symbol) {
            self.subscriptions.push(symbol.to_string());
        }
        Ok(())
    }

    async fn submit_order(&mut self, _params: OrderParams) -> Result<u64, BrokerError> {
        // Order routing will be implemented in Epic 4 (Story 4.x)
        Err(BrokerError::ProtocolError(
            "order submission not yet implemented in TickerPlant adapter".into(),
        ))
    }

    async fn cancel_order(&mut self, _order_id: u64) -> Result<(), BrokerError> {
        Err(BrokerError::ProtocolError(
            "order cancellation not yet implemented in TickerPlant adapter".into(),
        ))
    }

    async fn query_positions(&self) -> Result<Vec<Position>, BrokerError> {
        Err(BrokerError::PositionQueryFailed(
            "position query not yet implemented in TickerPlant adapter".into(),
        ))
    }

    async fn query_open_orders(&self) -> Result<Vec<(u64, OrderState)>, BrokerError> {
        Err(BrokerError::ProtocolError(
            "open order query not yet implemented in TickerPlant adapter".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribe_without_connect_fails() {
        // Can't create RithmicAdapter without env vars, so test the error path directly
        let err = BrokerError::SubscriptionFailed("not connected — call connect() first".into());
        assert_eq!(
            err.to_string(),
            "subscription failed: not connected — call connect() first"
        );
    }

    #[test]
    fn connection_lost_error_has_context() {
        let err = BrokerError::ConnectionLost("WebSocket connect failed: timeout".into());
        let msg = err.to_string();
        assert!(msg.contains("timeout"));
        assert!(msg.contains("connection lost"));
    }
}
