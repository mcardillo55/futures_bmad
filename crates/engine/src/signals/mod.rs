pub mod composite;
pub mod levels;
pub mod microprice;
pub mod obi;
pub mod vpin;

pub use composite::{CompositeConfig, CompositeEvaluator, DecisionReason, TradeDecision};
pub use levels::{
    LevelConfig, LevelEngine, LevelProximity, LevelSource, LevelType, SessionData, StructuralLevel,
};
pub use microprice::MicropriceSignal;
pub use obi::ObiSignal;
pub use vpin::VpinSignal;

use futures_bmad_core::{Clock, FixedPrice, MarketEvent, OrderBook, Signal, SignalSnapshot};

/// Concrete signal pipeline — zero vtable overhead, no dynamic dispatch.
pub struct SignalPipeline {
    pub obi: ObiSignal,
    pub vpin: VpinSignal,
    pub microprice: MicropriceSignal,
}

/// Snapshot of all pipeline signals.
#[derive(Debug)]
pub struct PipelineSnapshot {
    pub obi: SignalSnapshot,
    pub vpin: SignalSnapshot,
    pub microprice: SignalSnapshot,
}

impl SignalPipeline {
    pub fn new(
        obi_warmup: u64,
        max_spread: FixedPrice,
        vpin_bucket_size: u64,
        vpin_num_buckets: usize,
    ) -> Self {
        Self {
            obi: ObiSignal::new(obi_warmup, max_spread),
            vpin: VpinSignal::new(vpin_bucket_size, vpin_num_buckets),
            microprice: MicropriceSignal::new(max_spread),
        }
    }

    /// Update all signals with new market data.
    pub fn update_all(
        &mut self,
        book: &OrderBook,
        trade: Option<&MarketEvent>,
        clock: &dyn Clock,
    ) -> (Option<f64>, Option<f64>, Option<f64>) {
        let obi = self.obi.update(book, trade, clock);
        let vpin = self.vpin.update(book, trade, clock);
        let mp = self.microprice.update(book, trade, clock);
        (obi, vpin, mp)
    }

    /// Returns true if all signals are valid and producing output.
    pub fn all_valid(&self) -> bool {
        self.obi.is_valid() && self.vpin.is_valid() && self.microprice.is_valid()
    }

    /// Reset all signals for replay.
    pub fn reset(&mut self) {
        self.obi.reset();
        self.vpin.reset();
        self.microprice.reset();
    }

    /// Capture snapshots from all signals.
    pub fn snapshot(&self) -> PipelineSnapshot {
        PipelineSnapshot {
            obi: self.obi.snapshot(),
            vpin: self.vpin.snapshot(),
            microprice: self.microprice.snapshot(),
        }
    }
}
