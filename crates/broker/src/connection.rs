use futures_bmad_core::BrokerError;
use rithmic_rs::{
    ConnectStrategy, RithmicConfig, RithmicEnv, RithmicTickerPlant, RithmicTickerPlantHandle,
};
use secrecy::{ExposeSecret, SecretString};
use tracing::{info, warn};

/// Credentials loaded from environment variables, held as secrets.
pub(crate) struct RithmicCredentials {
    pub user: SecretString,
    pub password: SecretString,
    pub system_name: String,
    pub account_id: String,
    pub fcm_id: String,
    pub ib_id: String,
    pub url: String,
    pub beta_url: String,
    pub env: RithmicEnv,
}

impl std::fmt::Debug for RithmicCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RithmicCredentials")
            .field("user", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("system_name", &self.system_name)
            .field("account_id", &self.account_id)
            .field("env", &self.env)
            .finish()
    }
}

impl RithmicCredentials {
    /// Load credentials from environment variables into SecretString.
    pub fn from_env(env: RithmicEnv) -> Result<Self, BrokerError> {
        let user = std::env::var("RITHMIC_USER").map_err(|_| {
            BrokerError::AuthenticationFailed("RITHMIC_USER env var not set".into())
        })?;
        let password = std::env::var("RITHMIC_PASSWORD").map_err(|_| {
            BrokerError::AuthenticationFailed("RITHMIC_PASSWORD env var not set".into())
        })?;
        let system_name =
            std::env::var("RITHMIC_SYSTEM_NAME").unwrap_or_else(|_| "Rithmic Paper Trading".into());
        let account_id = std::env::var("RITHMIC_ACCOUNT_ID").unwrap_or_default();
        let fcm_id = std::env::var("RITHMIC_FCM_ID").unwrap_or_default();
        let ib_id = std::env::var("RITHMIC_IB_ID").unwrap_or_default();
        let url = std::env::var("RITHMIC_URL").unwrap_or_default();
        let beta_url = std::env::var("RITHMIC_BETA_URL").unwrap_or_default();

        Ok(Self {
            user: SecretString::from(user),
            password: SecretString::from(password),
            system_name,
            account_id,
            fcm_id,
            ib_id,
            url,
            beta_url,
            env,
        })
    }

    /// Build a RithmicConfig, exposing secrets only at the TLS handshake boundary.
    fn to_rithmic_config(&self) -> Result<RithmicConfig, BrokerError> {
        let mut builder = RithmicConfig::builder(self.env.clone())
            .user(self.user.expose_secret().to_string())
            .password(self.password.expose_secret().to_string())
            .system_name(self.system_name.clone());

        if !self.account_id.is_empty() {
            builder = builder.account_id(self.account_id.clone());
        }
        if !self.fcm_id.is_empty() {
            builder = builder.fcm_id(self.fcm_id.clone());
        }
        if !self.ib_id.is_empty() {
            builder = builder.ib_id(self.ib_id.clone());
        }
        if !self.url.is_empty() {
            builder = builder.url(self.url.clone());
        }
        if !self.beta_url.is_empty() {
            builder = builder.beta_url(self.beta_url.clone());
        }

        builder
            .build()
            .map_err(|e| BrokerError::AuthenticationFailed(format!("invalid config: {e}")))
    }
}

/// Wraps the rithmic-rs TickerPlant connection lifecycle.
pub(crate) struct RithmicConnection {
    plant: Option<RithmicTickerPlant>,
    credentials: RithmicCredentials,
}

impl RithmicConnection {
    pub fn new(credentials: RithmicCredentials) -> Self {
        Self {
            plant: None,
            credentials,
        }
    }

    /// Establish WebSocket connection to Rithmic TickerPlant and login.
    /// If already connected, the old connection is dropped first.
    pub async fn connect(&mut self) -> Result<RithmicTickerPlantHandle, BrokerError> {
        // Drop any existing connection to avoid resource leaks
        if self.plant.is_some() {
            warn!("dropping existing connection before reconnecting");
            self.plant = None;
        }

        info!("connecting to Rithmic TickerPlant");

        let config = self.credentials.to_rithmic_config()?;

        let plant = RithmicTickerPlant::connect(&config, ConnectStrategy::Simple)
            .await
            .map_err(|e| BrokerError::ConnectionLost(format!("WebSocket connect failed: {e}")))?;

        let handle = plant.get_handle();

        match handle.login().await {
            Ok(_) => {
                info!("Rithmic TickerPlant connected and authenticated");
                self.plant = Some(plant);
                Ok(handle)
            }
            Err(e) => {
                // Drop plant explicitly to close WebSocket on login failure
                drop(plant);
                Err(BrokerError::AuthenticationFailed(format!(
                    "login failed: {e}"
                )))
            }
        }
    }

    /// Disconnect from Rithmic TickerPlant.
    pub async fn disconnect(
        &mut self,
        handle: &RithmicTickerPlantHandle,
    ) -> Result<(), BrokerError> {
        if self.plant.is_some() {
            handle
                .disconnect()
                .await
                .map_err(|e| BrokerError::ConnectionLost(format!("disconnect failed: {e}")))?;
            self.plant = None;
            info!("Rithmic TickerPlant disconnected");
        } else {
            warn!("disconnect called but no active connection");
        }
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.plant.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_debug_does_not_leak_secrets() {
        let creds = RithmicCredentials {
            user: SecretString::from("my_user".to_string()),
            password: SecretString::from("my_password".to_string()),
            system_name: "Test System".into(),
            account_id: "ACC123".into(),
            fcm_id: "FCM1".into(),
            ib_id: "IB1".into(),
            url: "wss://example.com".into(),
            beta_url: "wss://beta.example.com".into(),
            env: RithmicEnv::Demo,
        };

        let debug_str = format!("{:?}", creds);
        assert!(!debug_str.contains("my_user"));
        assert!(!debug_str.contains("my_password"));
        assert!(debug_str.contains("[REDACTED]"));
    }

    #[test]
    fn credentials_from_env_missing_user() {
        // Clear the env vars to test missing user
        // SAFETY: test-only, single-threaded access to env vars
        unsafe { std::env::remove_var("RITHMIC_USER") };
        let result = RithmicCredentials::from_env(RithmicEnv::Demo);
        assert!(result.is_err());
        match result.unwrap_err() {
            BrokerError::AuthenticationFailed(msg) => {
                assert!(msg.contains("RITHMIC_USER"));
            }
            other => panic!("expected AuthenticationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn credentials_from_env_missing_password() {
        // SAFETY: test-only, single-threaded access to env vars
        unsafe {
            std::env::set_var("RITHMIC_USER", "test_user");
            std::env::remove_var("RITHMIC_PASSWORD");
        }
        let result = RithmicCredentials::from_env(RithmicEnv::Demo);
        unsafe { std::env::remove_var("RITHMIC_USER") };
        assert!(result.is_err());
        match result.unwrap_err() {
            BrokerError::AuthenticationFailed(msg) => {
                assert!(msg.contains("RITHMIC_PASSWORD"));
            }
            other => panic!("expected AuthenticationFailed, got: {other:?}"),
        }
    }
}
