use secrecy::SecretString;
use serde::Deserialize;
use std::fmt;

#[derive(Clone, Deserialize)]
pub struct BrokerConfig {
    pub server: String,
    pub gateway: String,
    pub user: String,
    pub password: SecretString,
    pub reconnect_delay_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub order_timeout_ms: u64,
}

impl fmt::Debug for BrokerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrokerConfig")
            .field("server", &self.server)
            .field("gateway", &self.gateway)
            .field("user", &self.user)
            .field("password", &"[REDACTED]")
            .field("reconnect_delay_ms", &self.reconnect_delay_ms)
            .field("heartbeat_interval_ms", &self.heartbeat_interval_ms)
            .field("order_timeout_ms", &self.order_timeout_ms)
            .finish()
    }
}
