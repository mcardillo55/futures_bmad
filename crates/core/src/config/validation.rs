use super::{BrokerConfig, FeeConfig, TradingConfig};
use chrono::NaiveDate;

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValidationError {
    ZeroPositionSize,
    ZeroConsecutiveLosses,
    EdgeMultipleTooLow(f64),
    NegativeDailyLoss,
    NegativeFee(String),
    NonFiniteValue(String),
    FeeScheduleStale { days_old: i64 },
    FeeScheduleExpired { days_old: i64 },
    FeeScheduleFuture,
    InvalidDateFormat(String),
    EmptyField(String),
    InvalidFormat { field: String, expected: String },
    ZeroTimeout(String),
    NegativeSpreadThreshold,
}

impl std::fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ZeroPositionSize => write!(f, "max_position_size must be > 0"),
            Self::ZeroConsecutiveLosses => write!(f, "max_consecutive_losses must be > 0"),
            Self::EdgeMultipleTooLow(v) => write!(f, "edge_multiple_threshold ({v}) must be >= 1.0"),
            Self::NegativeDailyLoss => write!(f, "max_daily_loss must be >= 0"),
            Self::NegativeFee(name) => write!(f, "{name} must be >= 0"),
            Self::NonFiniteValue(name) => write!(f, "{name} must be a finite number"),
            Self::FeeScheduleStale { days_old } => {
                write!(f, "fee schedule is {days_old} days old (>30 days, warning)")
            }
            Self::FeeScheduleExpired { days_old } => {
                write!(f, "fee schedule is {days_old} days old (>60 days, blocked)")
            }
            Self::FeeScheduleFuture => write!(f, "fee schedule effective_date is in the future"),
            Self::InvalidDateFormat(val) => write!(f, "invalid date format: {val}"),
            Self::EmptyField(name) => write!(f, "{name} must not be empty"),
            Self::InvalidFormat { field, expected } => {
                write!(f, "{field} has invalid format (expected {expected})")
            }
            Self::ZeroTimeout(name) => write!(f, "{name} must be > 0"),
            Self::NegativeSpreadThreshold => write!(f, "max_spread_threshold must be >= 0"),
        }
    }
}

impl std::error::Error for ConfigValidationError {}

pub fn validate_trading_config(config: &TradingConfig) -> Result<(), Vec<ConfigValidationError>> {
    let mut errors = Vec::new();

    if config.symbol.is_empty() {
        errors.push(ConfigValidationError::EmptyField("symbol".into()));
    }
    if config.max_position_size == 0 {
        errors.push(ConfigValidationError::ZeroPositionSize);
    }
    if config.max_consecutive_losses == 0 {
        errors.push(ConfigValidationError::ZeroConsecutiveLosses);
    }
    if !config.edge_multiple_threshold.is_finite() {
        errors.push(ConfigValidationError::NonFiniteValue("edge_multiple_threshold".into()));
    } else if config.edge_multiple_threshold < 1.0 {
        errors.push(ConfigValidationError::EdgeMultipleTooLow(config.edge_multiple_threshold));
    }
    if config.max_daily_loss.raw() < 0 {
        errors.push(ConfigValidationError::NegativeDailyLoss);
    }
    if config.max_spread_threshold.raw() < 0 {
        errors.push(ConfigValidationError::NegativeSpreadThreshold);
    }

    // Validate session time format (HH:MM)
    let time_re = |s: &str| -> bool {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 2 { return false; }
        let h = parts[0].parse::<u32>().ok();
        let m = parts[1].parse::<u32>().ok();
        matches!((h, m), (Some(h), Some(m)) if h < 24 && m < 60)
    };
    if config.session_start.is_empty() {
        errors.push(ConfigValidationError::EmptyField("session_start".into()));
    } else if !time_re(&config.session_start) {
        errors.push(ConfigValidationError::InvalidFormat {
            field: "session_start".into(),
            expected: "HH:MM".into(),
        });
    }
    if config.session_end.is_empty() {
        errors.push(ConfigValidationError::EmptyField("session_end".into()));
    } else if !time_re(&config.session_end) {
        errors.push(ConfigValidationError::InvalidFormat {
            field: "session_end".into(),
            expected: "HH:MM".into(),
        });
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

pub fn validate_fee_config(config: &FeeConfig) -> Result<(), Vec<ConfigValidationError>> {
    let mut errors = Vec::new();

    for (name, value) in [
        ("exchange_fee", config.exchange_fee),
        ("clearing_fee", config.clearing_fee),
        ("nfa_fee", config.nfa_fee),
        ("broker_commission", config.broker_commission),
    ] {
        if !value.is_finite() {
            errors.push(ConfigValidationError::NonFiniteValue(name.into()));
        } else if value < 0.0 {
            errors.push(ConfigValidationError::NegativeFee(name.into()));
        }
    }

    match NaiveDate::parse_from_str(&config.effective_date, "%Y-%m-%d") {
        Ok(date) => {
            let today = chrono::Utc::now().date_naive();
            let days_old = (today - date).num_days();
            if days_old < 0 {
                errors.push(ConfigValidationError::FeeScheduleFuture);
            } else if days_old > 60 {
                errors.push(ConfigValidationError::FeeScheduleExpired { days_old });
            } else if days_old > 30 {
                errors.push(ConfigValidationError::FeeScheduleStale { days_old });
            }
        }
        Err(_) => {
            errors.push(ConfigValidationError::InvalidDateFormat(config.effective_date.clone()));
        }
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

pub fn validate_broker_config(config: &BrokerConfig) -> Result<(), Vec<ConfigValidationError>> {
    let mut errors = Vec::new();

    if config.server.is_empty() {
        errors.push(ConfigValidationError::EmptyField("server".into()));
    }
    if config.gateway.is_empty() {
        errors.push(ConfigValidationError::EmptyField("gateway".into()));
    }
    if config.user.is_empty() {
        errors.push(ConfigValidationError::EmptyField("user".into()));
    }
    if config.reconnect_delay_ms == 0 {
        errors.push(ConfigValidationError::ZeroTimeout("reconnect_delay_ms".into()));
    }
    if config.heartbeat_interval_ms == 0 {
        errors.push(ConfigValidationError::ZeroTimeout("heartbeat_interval_ms".into()));
    }
    if config.order_timeout_ms == 0 {
        errors.push(ConfigValidationError::ZeroTimeout("order_timeout_ms".into()));
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

pub fn validate_all(
    trading: &TradingConfig,
    fees: &FeeConfig,
    broker: &BrokerConfig,
) -> Result<(), Vec<ConfigValidationError>> {
    let mut all_errors = Vec::new();
    if let Err(errs) = validate_trading_config(trading) {
        all_errors.extend(errs);
    }
    if let Err(errs) = validate_fee_config(fees) {
        all_errors.extend(errs);
    }
    if let Err(errs) = validate_broker_config(broker) {
        all_errors.extend(errs);
    }
    if all_errors.is_empty() { Ok(()) } else { Err(all_errors) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FixedPrice;
    use secrecy::SecretString;

    fn valid_trading_config() -> TradingConfig {
        TradingConfig {
            symbol: "ES".into(),
            max_position_size: 2,
            max_daily_loss: FixedPrice::new(400),
            max_consecutive_losses: 3,
            edge_multiple_threshold: 1.5,
            session_start: "09:30".into(),
            session_end: "16:00".into(),
            max_spread_threshold: FixedPrice::new(4),
        }
    }

    fn valid_fee_config() -> FeeConfig {
        FeeConfig {
            exchange_fee: 1.28,
            clearing_fee: 0.10,
            nfa_fee: 0.02,
            broker_commission: 2.78,
            effective_date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        }
    }

    fn valid_broker_config() -> BrokerConfig {
        BrokerConfig {
            server: "rithmic-paper".into(),
            gateway: "chicago".into(),
            user: "test_user".into(),
            password: SecretString::from("secret".to_string()),
            reconnect_delay_ms: 5000,
            heartbeat_interval_ms: 10000,
            order_timeout_ms: 30000,
        }
    }

    #[test]
    fn valid_configs_pass() {
        assert!(validate_trading_config(&valid_trading_config()).is_ok());
        assert!(validate_fee_config(&valid_fee_config()).is_ok());
        assert!(validate_broker_config(&valid_broker_config()).is_ok());
    }

    #[test]
    fn zero_position_size_rejected() {
        let mut config = valid_trading_config();
        config.max_position_size = 0;
        let errs = validate_trading_config(&config).unwrap_err();
        assert!(errs.contains(&ConfigValidationError::ZeroPositionSize));
    }

    #[test]
    fn zero_consecutive_losses_rejected() {
        let mut config = valid_trading_config();
        config.max_consecutive_losses = 0;
        let errs = validate_trading_config(&config).unwrap_err();
        assert!(errs.contains(&ConfigValidationError::ZeroConsecutiveLosses));
    }

    #[test]
    fn low_edge_multiple_rejected() {
        let mut config = valid_trading_config();
        config.edge_multiple_threshold = 0.5;
        let errs = validate_trading_config(&config).unwrap_err();
        assert!(matches!(errs[0], ConfigValidationError::EdgeMultipleTooLow(_)));
    }

    #[test]
    fn negative_fee_rejected() {
        let mut config = valid_fee_config();
        config.exchange_fee = -1.0;
        let errs = validate_fee_config(&config).unwrap_err();
        assert!(matches!(&errs[0], ConfigValidationError::NegativeFee(name) if name == "exchange_fee"));
    }

    #[test]
    fn fee_staleness_warn_at_31_days() {
        let mut config = valid_fee_config();
        let stale_date = (chrono::Utc::now() - chrono::Duration::days(31)).format("%Y-%m-%d").to_string();
        config.effective_date = stale_date;
        let errs = validate_fee_config(&config).unwrap_err();
        assert!(matches!(&errs[0], ConfigValidationError::FeeScheduleStale { days_old } if *days_old == 31));
    }

    #[test]
    fn fee_staleness_block_at_61_days() {
        let mut config = valid_fee_config();
        let expired_date = (chrono::Utc::now() - chrono::Duration::days(61)).format("%Y-%m-%d").to_string();
        config.effective_date = expired_date;
        let errs = validate_fee_config(&config).unwrap_err();
        assert!(matches!(&errs[0], ConfigValidationError::FeeScheduleExpired { days_old } if *days_old == 61));
    }

    #[test]
    fn fee_at_29_days_passes() {
        let mut config = valid_fee_config();
        let ok_date = (chrono::Utc::now() - chrono::Duration::days(29)).format("%Y-%m-%d").to_string();
        config.effective_date = ok_date;
        assert!(validate_fee_config(&config).is_ok());
    }

    #[test]
    fn empty_broker_fields_rejected() {
        let mut config = valid_broker_config();
        config.server = String::new();
        config.gateway = String::new();
        let errs = validate_broker_config(&config).unwrap_err();
        assert!(errs.len() >= 2);
    }

    #[test]
    fn zero_timeout_rejected() {
        let mut config = valid_broker_config();
        config.order_timeout_ms = 0;
        let errs = validate_broker_config(&config).unwrap_err();
        assert!(matches!(&errs[0], ConfigValidationError::ZeroTimeout(name) if name == "order_timeout_ms"));
    }

    #[test]
    fn validate_all_collects_all_errors() {
        let mut trading = valid_trading_config();
        trading.max_position_size = 0;
        trading.max_consecutive_losses = 0;

        let mut fees = valid_fee_config();
        fees.exchange_fee = -1.0;

        let mut broker = valid_broker_config();
        broker.server = String::new();

        let errs = validate_all(&trading, &fees, &broker).unwrap_err();
        assert!(errs.len() >= 4); // At least one from each config
    }
}
