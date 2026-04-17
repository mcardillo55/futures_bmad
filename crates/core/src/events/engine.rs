use super::{
    CircuitBreakerEvent, ConnectionStateChange, FillEvent, HeartbeatEvent, MarketEvent,
    OrderEvent, RegimeTransition, SignalEvent,
};
use crate::types::UnixNanos;

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Market(MarketEvent),
    Signal(SignalEvent),
    Order(OrderEvent),
    Fill(FillEvent),
    Regime(RegimeTransition),
    CircuitBreaker(CircuitBreakerEvent),
    Connection(ConnectionStateChange),
    Heartbeat(HeartbeatEvent),
}

impl EngineEvent {
    pub fn timestamp(&self) -> UnixNanos {
        match self {
            EngineEvent::Market(e) => e.timestamp,
            EngineEvent::Signal(e) => e.timestamp,
            EngineEvent::Order(e) => e.timestamp,
            EngineEvent::Fill(e) => e.timestamp,
            EngineEvent::Regime(e) => e.timestamp,
            EngineEvent::CircuitBreaker(e) => e.timestamp,
            EngineEvent::Connection(e) => e.timestamp,
            EngineEvent::Heartbeat(e) => e.timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::MarketEventType;
    use crate::types::{FixedPrice, Side};

    #[test]
    fn timestamp_from_market_event() {
        let ts = UnixNanos::new(12345);
        let event = EngineEvent::Market(MarketEvent {
            timestamp: ts,
            symbol_id: 1,
            event_type: MarketEventType::Trade,
            price: FixedPrice::new(100),
            size: 10,
            side: Some(Side::Buy),
        });
        assert_eq!(event.timestamp(), ts);
    }

    #[test]
    fn timestamp_from_heartbeat() {
        let ts = UnixNanos::new(99999);
        let event = EngineEvent::Heartbeat(HeartbeatEvent { timestamp: ts, sequence: 1 });
        assert_eq!(event.timestamp(), ts);
    }
}
