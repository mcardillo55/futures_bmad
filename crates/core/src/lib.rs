pub mod config;
pub mod events;
pub mod order_book;
pub mod traits;
pub mod types;

pub use config::{
    AlertingConfig, BrokerConfig, BrokerMode, ConfigValidationError, DEFAULT_REGIME_COOLDOWN_SECS,
    EventAction, EventWindowConfig, FeeConfig, RegimeOrchestrationConfig, TradingConfig,
    validate_all, validate_broker_config, validate_event_window_config, validate_fee_config,
    validate_trading_config,
};
pub use events::{
    BreakerCategory, BreakerState, BreakerType, CircuitBreakerEvent, ConnectionStateChange,
    EngineEvent, FillEvent, HeartbeatEvent, MarketEvent, MarketEventType, OrderEvent,
    RegimeTransition, SignalEvent,
};
pub use order_book::{Level, OrderBook};
pub use traits::{
    BrokerAdapter, BrokerError, Clock, RegimeDetector, RegimeState, Signal, SignalSnapshot,
    SystemClock,
};
pub use types::{
    Bar, BracketOrder, BracketOrderError, BracketState, BracketStateError, BrokerPosition,
    FillType, FixedPrice, NonFinitePrice, OrderKind, OrderParams, OrderParamsError, OrderState,
    OrderStateError, OrderType, Position, RejectReason, Side, UnixNanos,
};
