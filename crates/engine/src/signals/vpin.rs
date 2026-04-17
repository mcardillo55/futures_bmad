use std::collections::VecDeque;

use futures_bmad_core::{
    Clock, FixedPrice, MarketEvent, MarketEventType, OrderBook, Side, Signal, SignalSnapshot,
    UnixNanos,
};

/// A completed volume bucket recording buy/sell volume over a period.
#[derive(Debug, Clone, Copy)]
struct VolumeBucket {
    buy_volume: u64,
    sell_volume: u64,
    start_time: UnixNanos,
    end_time: UnixNanos,
}

impl VolumeBucket {
    fn empty(ts: UnixNanos) -> Self {
        Self {
            buy_volume: 0,
            sell_volume: 0,
            start_time: ts,
            end_time: ts,
        }
    }

    fn total_volume(&self) -> u64 {
        self.buy_volume + self.sell_volume
    }
}

/// VPIN (Volume-Synchronized Probability of Informed Trading) signal.
///
/// Classifies trade volume into buy/sell buckets and computes:
/// `VPIN = sum(|buy_vol - sell_vol|) / sum(total_vol)` across a rolling window.
///
/// Result is in [0.0, 1.0] where higher values indicate more informed flow.
/// Only processes trade events — book-only updates return the cached value.
pub struct VpinSignal {
    bucket_size: u64,
    num_buckets: usize,
    buckets: VecDeque<VolumeBucket>,
    current_bucket: VolumeBucket,
    last_vpin: Option<f64>,
    valid: bool,
    last_trade_price: Option<FixedPrice>,
    last_classification: Option<Side>,
    last_update_ts: UnixNanos,
}

impl VpinSignal {
    pub fn new(bucket_size: u64, num_buckets: usize) -> Self {
        assert!(bucket_size > 0, "bucket_size must be > 0");
        assert!(num_buckets > 0, "num_buckets must be > 0");
        Self {
            bucket_size,
            num_buckets,
            buckets: VecDeque::with_capacity(num_buckets),
            current_bucket: VolumeBucket::empty(UnixNanos::default()),
            last_vpin: None,
            valid: false,
            last_trade_price: None,
            last_classification: None,
            last_update_ts: UnixNanos::default(),
        }
    }

    /// Classify a trade as buy or sell. Uses explicit side if available,
    /// falls back to tick rule (compare to last trade price).
    fn classify_trade(&mut self, trade: &MarketEvent) -> Option<Side> {
        if let Some(side) = trade.side {
            self.last_classification = Some(side);
            self.last_trade_price = Some(trade.price);
            return Some(side);
        }

        // Tick rule fallback
        let classification = if let Some(last_price) = self.last_trade_price {
            if trade.price > last_price {
                Some(Side::Buy)
            } else if trade.price < last_price {
                Some(Side::Sell)
            } else {
                // Same price — use last classification
                self.last_classification
            }
        } else {
            // No previous trade — cannot classify
            None
        };

        self.last_trade_price = Some(trade.price);
        if let Some(side) = classification {
            self.last_classification = Some(side);
        }
        classification
    }

    fn compute_vpin(&self) -> Option<f64> {
        if self.buckets.len() < self.num_buckets {
            return None;
        }

        let mut total_imbalance: u64 = 0;
        let mut total_volume: u64 = 0;

        for bucket in &self.buckets {
            let imbalance = bucket.buy_volume.abs_diff(bucket.sell_volume);
            total_imbalance += imbalance;
            total_volume += bucket.total_volume();
        }

        if total_volume == 0 {
            return None;
        }

        let vpin = total_imbalance as f64 / total_volume as f64;

        if !vpin.is_finite() {
            return None;
        }

        Some(vpin)
    }
}

impl Signal for VpinSignal {
    fn update(
        &mut self,
        _book: &OrderBook,
        trade: Option<&MarketEvent>,
        clock: &dyn Clock,
    ) -> Option<f64> {
        let trade = match trade {
            Some(t) if t.event_type == MarketEventType::Trade => t,
            _ => return self.last_vpin.filter(|_| self.valid),
        };

        if trade.size == 0 {
            return self.last_vpin.filter(|_| self.valid);
        }

        let side = match self.classify_trade(trade) {
            Some(s) => s,
            None => return self.last_vpin.filter(|_| self.valid),
        };

        let now = clock.now();
        let volume = trade.size as u64;

        // Initialize bucket start time if needed
        if self.current_bucket.total_volume() == 0 {
            self.current_bucket.start_time = now;
        }
        self.current_bucket.end_time = now;

        match side {
            Side::Buy => self.current_bucket.buy_volume += volume,
            Side::Sell => self.current_bucket.sell_volume += volume,
        }

        // Check if bucket is complete
        while self.current_bucket.total_volume() >= self.bucket_size {
            let overflow = self.current_bucket.total_volume() - self.bucket_size;

            // Complete the current bucket (trim overflow from the current trade's side,
            // capped to that side's volume to prevent underflow)
            let mut completed = self.current_bucket;
            match side {
                Side::Buy => {
                    let trim = overflow.min(completed.buy_volume);
                    completed.buy_volume -= trim;
                    completed.sell_volume -= overflow - trim;
                }
                Side::Sell => {
                    let trim = overflow.min(completed.sell_volume);
                    completed.sell_volume -= trim;
                    completed.buy_volume -= overflow - trim;
                }
            }

            self.buckets.push_back(completed);
            if self.buckets.len() > self.num_buckets {
                self.buckets.pop_front();
            }

            // Start new bucket with overflow
            self.current_bucket = VolumeBucket::empty(now);
            match side {
                Side::Buy => self.current_bucket.buy_volume = overflow,
                Side::Sell => self.current_bucket.sell_volume = overflow,
            }
        }

        // Compute VPIN if enough buckets
        if let Some(vpin) = self.compute_vpin() {
            self.last_vpin = Some(vpin);
            self.valid = true;
            self.last_update_ts = now;
            Some(vpin)
        } else {
            self.last_vpin = None;
            self.valid = false;
            None
        }
    }

    fn name(&self) -> &'static str {
        "vpin"
    }

    fn is_valid(&self) -> bool {
        self.valid && self.last_vpin.is_some()
    }

    fn reset(&mut self) {
        self.buckets.clear();
        self.current_bucket = VolumeBucket::empty(UnixNanos::default());
        self.last_vpin = None;
        self.valid = false;
        self.last_trade_price = None;
        self.last_classification = None;
        self.last_update_ts = UnixNanos::default();
    }

    fn snapshot(&self) -> SignalSnapshot {
        SignalSnapshot {
            name: "vpin",
            value: self.last_vpin,
            valid: self.is_valid(),
            timestamp: self.last_update_ts,
        }
    }
}
