mod broker;
mod fees;
mod trading;
mod validation;

pub use broker::BrokerConfig;
pub use fees::FeeConfig;
pub use trading::{EventAction, EventWindowConfig, TradingConfig};
pub use validation::{
    ConfigValidationError, validate_all, validate_broker_config, validate_event_window_config,
    validate_fee_config, validate_trading_config,
};
