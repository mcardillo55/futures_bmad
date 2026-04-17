use futures_bmad_core::{BrokerError, MarketEvent};
use tracing::{debug, warn};

use crate::adapter::RithmicAdapter;
use crate::message_validator::{MessageValidator, ValidationError};

/// Wraps RithmicAdapter and MessageValidator to produce validated MarketEvents.
/// This is the public-facing stream assembly that the engine will consume via SPSC (Story 2.2).
pub struct MarketDataStream {
    adapter: RithmicAdapter,
    validator: MessageValidator,
}

impl MarketDataStream {
    pub fn new(adapter: RithmicAdapter) -> Self {
        Self {
            adapter,
            validator: MessageValidator::new(),
        }
    }

    /// Read the next validated market event from the Rithmic stream.
    /// Skips irrelevant messages (heartbeats, login responses, etc.).
    /// Returns Err on connection issues or circuit break.
    /// When a BBO contains both bid+ask, two events are returned on successive calls.
    pub async fn next_event(&mut self) -> Result<MarketEvent, BrokerError> {
        // Drain any pending event from a previous dual-sided BBO
        if let Some(pending) = self.validator.take_pending() {
            return Ok(pending);
        }

        let handle = self
            .adapter
            .handle_mut()
            .ok_or_else(|| BrokerError::ConnectionLost("no active connection".into()))?;

        loop {
            let response = handle
                .subscription_receiver
                .recv()
                .await
                .map_err(|e| BrokerError::ConnectionLost(format!("channel closed: {e}")))?;

            // Check for connection-level errors
            if response.is_connection_issue() {
                return Err(BrokerError::ConnectionLost(
                    response
                        .error
                        .unwrap_or_else(|| "connection issue detected".into()),
                ));
            }

            if let Some(ref err) = response.error {
                warn!(error = %err, "Rithmic response error");
                continue;
            }

            // Use wall clock for sliding window (not market timestamp)
            let now_nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;

            match self.validator.validate(&response.message, now_nanos) {
                Ok(event) => return Ok(event),
                Err(ValidationError::Irrelevant) => {
                    debug!("skipping irrelevant message type");
                    continue;
                }
                Err(ValidationError::Malformed(reason)) => {
                    debug!(reason, "skipping malformed message");
                    continue;
                }
                Err(ValidationError::CircuitBreak { count }) => {
                    return Err(BrokerError::ProtocolError(format!(
                        "circuit break: {count} malformed messages in 60s window"
                    )));
                }
            }
        }
    }

    pub fn adapter(&self) -> &RithmicAdapter {
        &self.adapter
    }

    pub fn adapter_mut(&mut self) -> &mut RithmicAdapter {
        &mut self.adapter
    }

    pub fn malformed_count(&self) -> usize {
        self.validator.malformed_count_in_window()
    }
}
