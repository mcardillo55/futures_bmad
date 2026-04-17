use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct FeeConfig {
    pub exchange_fee: f64,
    pub clearing_fee: f64,
    pub nfa_fee: f64,
    pub broker_commission: f64,
    pub effective_date: String,
}

impl FeeConfig {
    pub fn total_per_side(&self) -> f64 {
        self.exchange_fee + self.clearing_fee + self.nfa_fee + self.broker_commission
    }
}
