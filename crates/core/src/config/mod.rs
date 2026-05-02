mod alerting;
mod broker;
mod broker_mode;
mod fees;
mod regime;
mod trading;
mod validation;

pub use alerting::AlertingConfig;
pub use broker::BrokerConfig;
pub use broker_mode::BrokerMode;
pub use fees::FeeConfig;
pub use regime::{DEFAULT_REGIME_COOLDOWN_SECS, RegimeOrchestrationConfig};
pub use trading::{EventAction, EventWindowConfig, TradingConfig};
pub use validation::{
    ConfigValidationError, validate_all, validate_broker_config, validate_event_window_config,
    validate_fee_config, validate_trading_config,
};
