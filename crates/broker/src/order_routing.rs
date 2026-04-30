//! Order routing — engine -> broker submission and broker -> engine fill reporting.
//!
//! Two SPSC `rtrb` ring buffers (capacity 4096) sit between the hot-path engine and
//! the broker's async task:
//!
//! ```text
//!     engine::order_manager (PRODUCER)
//!         │── OrderEvent [rtrb; cap=4096] ──► broker::order_routing (CONSUMER)
//!         │                                        │
//!         │                                        ├─► RithmicAdapter::submit_order(...)
//!         │                                        ▼
//!         │── FillEvent  [rtrb; cap=4096] ◄── broker::order_routing (PRODUCER)
//!     engine::order_manager (CONSUMER)
//! ```
//!
//! `OrderEvent` and `FillEvent` are `Copy` (defined in `core::types::order`) so each
//! queue carries pointer-free POD with no allocations on the hot path. Capacity 4096
//! is generous — orders are infrequent vs market data — and the producer logs (and
//! drops) on full, since a full order or fill queue indicates the consumer has
//! stalled, which is a separate operational alarm.
//!
//! The routing loop is `async` and lives on the broker's Tokio runtime; the fill
//! consumer is invoked synchronously from the engine event-loop tick (non-blocking
//! poll), preserving the hot-path's lock-free single-thread model.

use futures_bmad_core::{FillEvent, FillType, OrderEvent, RejectReason, UnixNanos};
use rtrb::{Consumer, Producer, PushError, RingBuffer};
use tracing::{error, info, warn};

/// SPSC ring-buffer capacity for OrderEvent and FillEvent queues.
///
/// Sized per architecture spec (orders are infrequent compared to market data; 4096
/// gives ample headroom for bursty bracket submissions while keeping the buffer
/// cache-friendly).
pub const ORDER_FILL_QUEUE_CAPACITY: usize = 4096;

// ---------------------------------------------------------------------------
// OrderQueue (engine -> broker)
// ---------------------------------------------------------------------------

/// Engine-side producer for the OrderEvent SPSC queue.
pub struct OrderQueueProducer {
    producer: Producer<OrderEvent>,
    drop_count: u64,
}

impl OrderQueueProducer {
    /// Non-blocking push. Logs an error on full (a full order queue indicates the
    /// broker routing loop has stalled — should never happen under normal load).
    /// Returns `true` if the event was accepted.
    pub fn try_push(&mut self, event: OrderEvent) -> bool {
        match self.producer.push(event) {
            Ok(()) => true,
            Err(PushError::Full(dropped)) => {
                self.drop_count += 1;
                error!(
                    target: "order_routing",
                    drop_count = self.drop_count,
                    order_id = dropped.order_id,
                    decision_id = dropped.decision_id,
                    "OrderQueue full — dropping OrderEvent (broker not consuming)"
                );
                false
            }
        }
    }

    pub fn drop_count(&self) -> u64 {
        self.drop_count
    }

    /// Available slots for writing.
    pub fn available_slots(&self) -> usize {
        self.producer.slots()
    }
}

/// Broker-side consumer for the OrderEvent SPSC queue.
pub struct OrderQueueConsumer {
    consumer: Consumer<OrderEvent>,
}

impl OrderQueueConsumer {
    /// Non-blocking pop. Returns `None` if the queue is empty.
    pub fn try_pop(&mut self) -> Option<OrderEvent> {
        self.consumer.pop().ok()
    }

    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    pub fn available(&self) -> usize {
        self.consumer.slots()
    }
}

// ---------------------------------------------------------------------------
// FillQueue (broker -> engine)
// ---------------------------------------------------------------------------

/// Broker-side producer for the FillEvent SPSC queue.
pub struct FillQueueProducer {
    producer: Producer<FillEvent>,
    drop_count: u64,
}

impl FillQueueProducer {
    /// Non-blocking push. Logs an error on full (engine is not draining fills —
    /// should never happen on the hot path).
    pub fn try_push(&mut self, event: FillEvent) -> bool {
        match self.producer.push(event) {
            Ok(()) => true,
            Err(PushError::Full(dropped)) => {
                self.drop_count += 1;
                error!(
                    target: "order_routing",
                    drop_count = self.drop_count,
                    order_id = dropped.order_id,
                    decision_id = dropped.decision_id,
                    "FillQueue full — dropping FillEvent (engine not consuming)"
                );
                false
            }
        }
    }

    pub fn drop_count(&self) -> u64 {
        self.drop_count
    }
}

/// Engine-side consumer for the FillEvent SPSC queue.
pub struct FillQueueConsumer {
    consumer: Consumer<FillEvent>,
}

impl FillQueueConsumer {
    /// Non-blocking pop. Returns `None` if the queue is empty.
    pub fn try_pop(&mut self) -> Option<FillEvent> {
        self.consumer.pop().ok()
    }

    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    pub fn available(&self) -> usize {
        self.consumer.slots()
    }
}

// ---------------------------------------------------------------------------
// Constructor
// ---------------------------------------------------------------------------

/// Create both SPSC queues that wire the engine and broker together.
///
/// Returns the four halves the routing loop needs:
/// `(order_producer, order_consumer, fill_producer, fill_consumer)`.
///
/// - `order_producer` is owned by the engine (engine -> broker direction).
/// - `order_consumer` is owned by the broker routing loop.
/// - `fill_producer` is owned by the broker fill listener.
/// - `fill_consumer` is owned by the engine event loop / order_manager.
///
/// # Panics
/// Panics if `ORDER_FILL_QUEUE_CAPACITY` is < 2 (it isn't).
pub fn create_order_fill_queues() -> (
    OrderQueueProducer,
    OrderQueueConsumer,
    FillQueueProducer,
    FillQueueConsumer,
) {
    let (op, oc) = RingBuffer::<OrderEvent>::new(ORDER_FILL_QUEUE_CAPACITY);
    let (fp, fc) = RingBuffer::<FillEvent>::new(ORDER_FILL_QUEUE_CAPACITY);
    (
        OrderQueueProducer {
            producer: op,
            drop_count: 0,
        },
        OrderQueueConsumer { consumer: oc },
        FillQueueProducer {
            producer: fp,
            drop_count: 0,
        },
        FillQueueConsumer { consumer: fc },
    )
}

// ---------------------------------------------------------------------------
// OrderSubmitter trait + routing loop
// ---------------------------------------------------------------------------

/// Submission-only trait used by the routing loop. The existing
/// `core::traits::BrokerAdapter` covers connect/subscribe/cancel/query — this
/// narrower trait isolates the OrderEvent-shaped submission path so the routing
/// loop can be unit-tested with a mock implementation that does NOT need to
/// carry the rest of `BrokerAdapter`'s surface.
#[async_trait::async_trait]
pub trait OrderSubmitter: Send + Sync {
    /// Submit a single `OrderEvent` to the exchange. Returns `Err` to indicate a
    /// transport failure (network/timeout/etc.); the routing loop turns the error
    /// into a synthetic [`FillType::Rejected`] fill back into the engine.
    async fn submit_order(&self, event: &OrderEvent) -> Result<(), SubmissionError>;
}

/// Placeholder Rithmic OrderPlant submitter.
///
/// Wires the [`OrderSubmitter`] trait to the real Rithmic OrderPlant once that
/// connectivity lands (a later epic-4 story will replace the stub body with the
/// actual `rithmic-rs` `OrderPlantHandle::send_new_order(...)` call). For now this
/// surfaces a `ConnectionLost` `SubmissionError` so the routing loop's reject-fill
/// pathway is exercised end-to-end without requiring a live broker session.
///
/// Field mapping (engine `OrderEvent` -> Rithmic `RequestNewOrder`):
/// - `event.symbol_id`           -> `symbol` (resolved via instrument table)
/// - `event.side`                -> `transaction_type` (Buy/Sell)
/// - `event.quantity`            -> `quantity`
/// - `event.order_type::Market`  -> `price_type = MARKET`
/// - `event.order_type::Limit`   -> `price_type = LIMIT, price = limit_price`
/// - `event.order_type::Stop`    -> `price_type = STOP_MARKET, trigger_price = trigger`
/// - `event.order_id`            -> `user_tag` (echoed back on fills for correlation)
pub struct RithmicSubmitter {
    /// Optional account context — populated when the OrderPlant connection is wired.
    pub account: String,
    /// Optional exchange code (e.g. `"CME"`).
    pub exchange: String,
}

impl RithmicSubmitter {
    /// Construct a placeholder submitter; once OrderPlant is wired this will take
    /// a `RithmicOrderPlantHandle` and the appropriate Rithmic env config.
    pub fn new(account: String, exchange: String) -> Self {
        Self { account, exchange }
    }
}

#[async_trait::async_trait]
impl OrderSubmitter for RithmicSubmitter {
    async fn submit_order(&self, _event: &OrderEvent) -> Result<(), SubmissionError> {
        // OrderPlant connectivity arrives in a later epic-4 story. Surface the
        // not-yet-wired state as a `ConnectionLost` rejection so the routing loop
        // emits a synthetic reject fill rather than swallowing the order.
        Err(SubmissionError::ConnectionLost)
    }
}

/// Submission failure as returned by [`OrderSubmitter::submit_order`]. Mapped 1:1
/// onto [`RejectReason`] when the routing loop synthesizes a rejection fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubmissionError {
    #[error("connection lost during submission")]
    ConnectionLost,
    #[error("submission timed out")]
    Timeout,
    #[error("invalid symbol")]
    InvalidSymbol,
    #[error("exchange-side reject")]
    ExchangeReject,
    #[error("unknown submission failure")]
    Unknown,
}

impl SubmissionError {
    /// Map a transport-level submission failure onto the reject-reason carried by
    /// the synthetic [`FillType::Rejected`] fill that the routing loop emits back
    /// to the engine.
    pub fn to_reject_reason(self) -> RejectReason {
        match self {
            SubmissionError::ConnectionLost => RejectReason::ConnectionLost,
            SubmissionError::InvalidSymbol => RejectReason::InvalidSymbol,
            SubmissionError::ExchangeReject => RejectReason::ExchangeReject,
            // Unknown failures — surface as Unknown so operators
            // dig into the diagnostic logs rather than misclassifying as a
            // hard exchange reject. (Timeout no longer flows through this
            // mapping — see `should_synthesize_reject` carryover (4-2 S-5).)
            SubmissionError::Timeout | SubmissionError::Unknown => RejectReason::Unknown,
        }
    }

    /// Whether this submission error should produce a synthetic `Rejected`
    /// fill back into the engine's FillQueue.
    ///
    /// Carryover (4-2 S-5): `Timeout` MUST NOT synthesize a Rejected fill.
    /// Story 4-4 introduced the `Submitted -> Uncertain -> PendingRecon`
    /// reconciliation arc precisely so that ambiguous submissions can be
    /// resolved via a broker query rather than guessed as a rejection. If we
    /// synthesize Rejected here, the engine drops the WAL row on a terminal
    /// state, but the order may have actually reached the exchange — the
    /// "phantom position" failure mode the architecture's NFR16 forbids.
    /// Leaving the order in `Submitted` lets the engine's 5s timeout
    /// watchdog flip it to `Uncertain` and 4-5 reconciliation resolves the
    /// true broker state.
    pub fn should_synthesize_reject(self) -> bool {
        match self {
            // Hard transport / exchange rejects — no order ever reached an
            // exchange match.
            SubmissionError::ConnectionLost
            | SubmissionError::InvalidSymbol
            | SubmissionError::ExchangeReject
            | SubmissionError::Unknown => true,
            // Ambiguous — let the engine's Uncertain-state machine handle it
            // via the 5s timeout + broker reconciliation path.
            SubmissionError::Timeout => false,
        }
    }
}

/// Drain the OrderEvent queue once, submitting each event via `submitter` and
/// pushing a synthetic rejection fill on failure. Returns the number of events
/// processed in this pass.
///
/// This is the inner step of the broker's order routing loop. It is intentionally
/// `async` so a real `RithmicAdapter::submit_order` can `.await` exchange
/// round-trips without blocking the loop. The function returns when the order
/// queue is empty so the caller can yield (or sleep briefly) before re-polling.
///
/// `now` is supplied by the caller (rather than read from the system clock here)
/// so the routing loop can be driven deterministically from a `SimClock` in tests.
///
/// On success the order's `decision_id` is recorded in `decisions` so subsequent
/// exchange execution reports can be enriched. On submission failure the synthetic
/// reject fill carries the original `decision_id` directly (no map lookup needed)
/// since it never reached the exchange.
pub async fn route_pending_orders<S: OrderSubmitter + ?Sized>(
    consumer: &mut OrderQueueConsumer,
    fill_producer: &mut FillQueueProducer,
    submitter: &S,
    decisions: &mut DecisionIdMap,
    now: UnixNanos,
) -> usize {
    let mut processed = 0usize;
    while let Some(event) = consumer.try_pop() {
        processed += 1;
        info!(
            target: "order_routing",
            order_id = event.order_id,
            symbol_id = event.symbol_id,
            side = ?event.side,
            quantity = event.quantity,
            order_type = ?event.order_type,
            decision_id = event.decision_id,
            "submitting order to broker"
        );

        match submitter.submit_order(&event).await {
            Ok(()) => {
                // Record the decision_id mapping so the fill listener can enrich
                // exchange execution reports back into FillEvents.
                decisions.insert(event.order_id, event.decision_id);
            }
            Err(err) => {
                if err.should_synthesize_reject() {
                    warn!(
                        target: "order_routing",
                        order_id = event.order_id,
                        decision_id = event.decision_id,
                        error = %err,
                        "order submission failed — synthesizing reject fill"
                    );
                    let reject = FillEvent {
                        order_id: event.order_id,
                        // Reject fills carry a zero fill price (no execution occurred);
                        // the engine should treat fill_size==0 with FillType::Rejected
                        // as the canonical rejection signal.
                        fill_price: futures_bmad_core::FixedPrice::default(),
                        fill_size: 0,
                        timestamp: now,
                        side: event.side,
                        decision_id: event.decision_id,
                        fill_type: FillType::Rejected {
                            reason: err.to_reject_reason(),
                        },
                    };
                    if !fill_producer.try_push(reject) {
                        error!(
                            target: "order_routing",
                            order_id = event.order_id,
                            decision_id = event.decision_id,
                            "FillQueue full — synthetic reject dropped (engine state will be reconciled via broker query)"
                        );
                    }
                } else {
                    // Carryover (4-2 S-5): submission Timeout is ambiguous —
                    // the order MAY have reached the exchange. Do NOT
                    // synthesize a Rejected fill (which would terminally
                    // resolve the order in the engine despite a real
                    // possibility of a fill). Leave the engine's order in
                    // `Submitted`; the 5s timeout watchdog will flip it to
                    // `Uncertain` and 4-5 reconciliation will query the
                    // broker for the true status.
                    warn!(
                        target: "order_routing",
                        order_id = event.order_id,
                        decision_id = event.decision_id,
                        error = %err,
                        "order submission ambiguous (timeout) — leaving Submitted for engine timeout/reconciliation"
                    );
                }
            }
        }
    }
    processed
}

// ---------------------------------------------------------------------------
// Fill listener — broker-side translation of execution reports into FillEvent
// ---------------------------------------------------------------------------

/// Tracks the `order_id -> decision_id` mapping needed to enrich exchange
/// execution reports (which echo back only the `order_id` / user-tag) with the
/// originating `decision_id` for causality tracing (NFR17).
///
/// The mapping is populated when the routing loop submits an order and consulted
/// when a fill / reject report is mapped into a [`FillEvent`]. Entries are removed
/// once a terminal event ([`FillType::Full`] or [`FillType::Rejected`]) is reported
/// to bound memory.
///
/// This is a single-thread structure owned by the broker side — the routing loop
/// and the fill listener run on the same Tokio task, so no synchronization is
/// needed.
#[derive(Debug, Default)]
pub struct DecisionIdMap {
    map: std::collections::HashMap<u64, u64>,
}

impl DecisionIdMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the `order_id -> decision_id` mapping at submission time.
    pub fn insert(&mut self, order_id: u64, decision_id: u64) {
        self.map.insert(order_id, decision_id);
    }

    /// Look up the decision_id for an order. Returns `None` if the routing loop
    /// never submitted this order id (which itself is a diagnostic — the fill
    /// listener will log and emit a `decision_id = 0` event so the journal entry
    /// surfaces the orphaned fill).
    pub fn get(&self, order_id: u64) -> Option<u64> {
        self.map.get(&order_id).copied()
    }

    /// Remove a tracked order — called on terminal fills/rejects to bound memory.
    pub fn remove(&mut self, order_id: u64) -> Option<u64> {
        self.map.remove(&order_id)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Broker-internal representation of an exchange execution report.
///
/// In the live system the broker's Tokio listener decodes Rithmic OrderPlant
/// messages and produces one `ExecutionReport` per relevant message. This type is
/// the translation seam — the fill-listener tests use it directly; the production
/// Rithmic listener (landing in a later epic-4 story) will construct values of
/// this type from `rithmic-rs` execution payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionReport {
    /// Order fully filled.
    Filled {
        order_id: u64,
        side: futures_bmad_core::Side,
        fill_price: futures_bmad_core::FixedPrice,
        fill_size: u32,
        timestamp: UnixNanos,
    },
    /// Order partially filled — `remaining` is the un-filled qty.
    PartiallyFilled {
        order_id: u64,
        side: futures_bmad_core::Side,
        fill_price: futures_bmad_core::FixedPrice,
        fill_size: u32,
        remaining: u32,
        timestamp: UnixNanos,
    },
    /// Order rejected by exchange.
    Rejected {
        order_id: u64,
        side: futures_bmad_core::Side,
        reason: RejectReason,
        timestamp: UnixNanos,
    },
}

impl ExecutionReport {
    pub fn order_id(&self) -> u64 {
        match self {
            ExecutionReport::Filled { order_id, .. }
            | ExecutionReport::PartiallyFilled { order_id, .. }
            | ExecutionReport::Rejected { order_id, .. } => *order_id,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ExecutionReport::Filled { .. } | ExecutionReport::Rejected { .. }
        )
    }
}

/// Map an exchange execution report onto a [`FillEvent`], enriching it with the
/// originating `decision_id` from the [`DecisionIdMap`]. Pushes the resulting
/// fill onto `producer` and returns whether the queue accepted it.
///
/// If `order_id` has no recorded decision mapping, logs a warning and emits the
/// fill with `decision_id = 0` (sentinel) so the orphaned event still appears in
/// the journal — better an annotated diagnostic than a silent drop.
pub fn publish_execution_report(
    report: &ExecutionReport,
    decisions: &mut DecisionIdMap,
    producer: &mut FillQueueProducer,
) -> bool {
    let order_id = report.order_id();
    let decision_id = match decisions.get(order_id) {
        Some(id) => id,
        None => {
            warn!(
                target: "order_routing",
                order_id,
                "no decision_id mapping for fill — emitting with decision_id=0 sentinel"
            );
            0
        }
    };

    let fill = match *report {
        ExecutionReport::Filled {
            order_id,
            side,
            fill_price,
            fill_size,
            timestamp,
        } => FillEvent {
            order_id,
            fill_price,
            fill_size,
            timestamp,
            side,
            decision_id,
            fill_type: FillType::Full,
        },
        ExecutionReport::PartiallyFilled {
            order_id,
            side,
            fill_price,
            fill_size,
            remaining,
            timestamp,
        } => FillEvent {
            order_id,
            fill_price,
            fill_size,
            timestamp,
            side,
            decision_id,
            fill_type: FillType::Partial { remaining },
        },
        ExecutionReport::Rejected {
            order_id,
            side,
            reason,
            timestamp,
        } => FillEvent {
            order_id,
            fill_price: futures_bmad_core::FixedPrice::default(),
            fill_size: 0,
            timestamp,
            side,
            decision_id,
            fill_type: FillType::Rejected { reason },
        },
    };

    let accepted = producer.try_push(fill);
    if report.is_terminal() {
        decisions.remove(order_id);
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_bmad_core::{FixedPrice, OrderType, Side};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn sample_order(order_id: u64, decision_id: u64) -> OrderEvent {
        OrderEvent {
            order_id,
            symbol_id: 1,
            side: Side::Buy,
            quantity: 1,
            order_type: OrderType::Market,
            decision_id,
            timestamp: UnixNanos::new(1_700_000_000_000_000_000),
        }
    }

    /// Task 6.2 — push OrderEvent, pop and verify fields match.
    #[test]
    fn order_queue_round_trip_preserves_fields() {
        let (mut prod, mut cons, _fp, _fc) = create_order_fill_queues();
        let evt = sample_order(42, 7);
        assert!(prod.try_push(evt));
        let got = cons.try_pop().expect("event should be available");
        assert_eq!(got, evt);
        assert!(cons.try_pop().is_none(), "queue should now be empty");
    }

    #[test]
    fn fill_queue_round_trip() {
        let (_op, _oc, mut prod, mut cons) = create_order_fill_queues();
        let evt = FillEvent {
            order_id: 99,
            fill_price: FixedPrice::new(17_929),
            fill_size: 2,
            timestamp: UnixNanos::new(1_700_000_000_000_000_000),
            side: Side::Buy,
            decision_id: 11,
            fill_type: FillType::Full,
        };
        assert!(prod.try_push(evt));
        assert_eq!(cons.try_pop(), Some(evt));
    }

    /// Task 6.3 — full queue: push 4096 events, next push returns false, no block.
    #[test]
    fn order_queue_full_drops_without_blocking() {
        let (mut prod, _cons, _fp, _fc) = create_order_fill_queues();
        // Fill the queue to capacity (rtrb capacity is the configured value).
        let mut accepted = 0usize;
        for i in 0..ORDER_FILL_QUEUE_CAPACITY {
            if prod.try_push(sample_order(i as u64, 0)) {
                accepted += 1;
            }
        }
        assert_eq!(accepted, ORDER_FILL_QUEUE_CAPACITY);

        // Next push must drop, must not block, and must increment drop_count.
        let start = Instant::now();
        let ok = prod.try_push(sample_order(99_999, 0));
        let elapsed = start.elapsed();
        assert!(!ok, "push past capacity must return false");
        assert!(
            elapsed < Duration::from_millis(50),
            "push must not block on full (took {elapsed:?})"
        );
        assert_eq!(prod.drop_count(), 1);
    }

    /// Mock submitter that records every submission. Optionally fails the Nth
    /// submission so we can verify the rejection-fill path.
    struct MockSubmitter {
        calls: Mutex<Vec<OrderEvent>>,
        fail_after: Option<usize>,
        fail_with: SubmissionError,
    }

    impl MockSubmitter {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_after: None,
                fail_with: SubmissionError::Unknown,
            }
        }

        fn failing(after: usize, err: SubmissionError) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_after: Some(after),
                fail_with: err,
            }
        }

        fn call_count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }

        fn last_call(&self) -> Option<OrderEvent> {
            self.calls.lock().unwrap().last().copied()
        }
    }

    #[async_trait::async_trait]
    impl OrderSubmitter for MockSubmitter {
        async fn submit_order(&self, event: &OrderEvent) -> Result<(), SubmissionError> {
            let mut guard = self.calls.lock().unwrap();
            guard.push(*event);
            if let Some(after) = self.fail_after
                && guard.len() > after
            {
                return Err(self.fail_with);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn routing_loop_submits_each_pending_order() {
        let (mut op, mut oc, mut fp, _fc) = create_order_fill_queues();
        let submitter = MockSubmitter::new();
        let mut decisions = DecisionIdMap::new();

        for i in 0..5 {
            assert!(op.try_push(sample_order(100 + i, i)));
        }

        let now = UnixNanos::new(1);
        let processed =
            route_pending_orders(&mut oc, &mut fp, &submitter, &mut decisions, now).await;
        assert_eq!(processed, 5);
        assert_eq!(submitter.call_count(), 5);
        assert_eq!(submitter.last_call().unwrap().order_id, 104);
        // Each successful submission populates the decision-id map.
        assert_eq!(decisions.len(), 5);
        assert_eq!(decisions.get(102), Some(2));
    }

    #[tokio::test]
    async fn routing_loop_synthesizes_reject_fill_on_submission_error() {
        let (mut op, mut oc, mut fp, mut fc) = create_order_fill_queues();
        // Fail every submission.
        let submitter = MockSubmitter::failing(0, SubmissionError::ConnectionLost);
        let mut decisions = DecisionIdMap::new();

        let evt = sample_order(7, 11);
        assert!(op.try_push(evt));
        let now = UnixNanos::new(123);
        route_pending_orders(&mut oc, &mut fp, &submitter, &mut decisions, now).await;

        let fill = fc.try_pop().expect("a reject fill should be enqueued");
        assert_eq!(fill.order_id, 7);
        assert_eq!(fill.decision_id, 11);
        assert_eq!(fill.fill_size, 0);
        assert_eq!(fill.timestamp, now);
        assert!(matches!(
            fill.fill_type,
            FillType::Rejected {
                reason: RejectReason::ConnectionLost
            }
        ));
        // Failed submissions do NOT populate the decision-id map (no exchange
        // record to subsequently match against).
        assert!(decisions.is_empty());
    }

    /// Carryover (4-2 S-5): SubmissionError::Timeout MUST NOT synthesize a
    /// Rejected fill — the order may have reached the exchange. Leaving it
    /// Submitted lets the engine's 5s timeout watchdog flip it to Uncertain
    /// and 4-5 reconciliation resolve via broker query.
    #[tokio::test]
    async fn routing_loop_does_not_synthesize_reject_on_timeout() {
        let (mut op, mut oc, mut fp, mut fc) = create_order_fill_queues();
        let submitter = MockSubmitter::failing(0, SubmissionError::Timeout);
        let mut decisions = DecisionIdMap::new();

        let evt = sample_order(7, 11);
        assert!(op.try_push(evt));
        let now = UnixNanos::new(123);
        route_pending_orders(&mut oc, &mut fp, &submitter, &mut decisions, now).await;

        // No synthetic Rejected fill should appear — order stays Submitted.
        assert!(
            fc.try_pop().is_none(),
            "Timeout submission must NOT generate a synthetic Rejected fill"
        );
        // Decision id is also NOT populated (no exchange round-trip).
        assert!(decisions.is_empty());
    }

    #[test]
    fn submission_error_should_synthesize_reject_for_hard_failures_only() {
        assert!(SubmissionError::ConnectionLost.should_synthesize_reject());
        assert!(SubmissionError::InvalidSymbol.should_synthesize_reject());
        assert!(SubmissionError::ExchangeReject.should_synthesize_reject());
        assert!(SubmissionError::Unknown.should_synthesize_reject());
        // Timeout is the carve-out per 4-2 S-5.
        assert!(!SubmissionError::Timeout.should_synthesize_reject());
    }

    #[tokio::test]
    async fn fill_listener_emits_partial_then_full_with_decision_id() {
        let (mut op, mut oc, mut fp, mut fc) = create_order_fill_queues();
        let submitter = MockSubmitter::new();
        let mut decisions = DecisionIdMap::new();

        // Submit one order so the decision_id is mapped.
        let evt = sample_order(31, 91);
        assert!(op.try_push(evt));
        route_pending_orders(
            &mut oc,
            &mut fp,
            &submitter,
            &mut decisions,
            UnixNanos::new(1),
        )
        .await;

        // Drain any unexpected fills (there should be none on success).
        assert!(fc.try_pop().is_none());

        let partial = ExecutionReport::PartiallyFilled {
            order_id: 31,
            side: Side::Buy,
            fill_price: FixedPrice::new(17_929),
            fill_size: 1,
            remaining: 1,
            timestamp: UnixNanos::new(2),
        };
        publish_execution_report(&partial, &mut decisions, &mut fp);
        let f1 = fc.try_pop().unwrap();
        assert_eq!(f1.order_id, 31);
        assert_eq!(f1.decision_id, 91);
        assert!(matches!(f1.fill_type, FillType::Partial { remaining: 1 }));
        // Partial is not terminal — the mapping must remain.
        assert_eq!(decisions.get(31), Some(91));

        let full = ExecutionReport::Filled {
            order_id: 31,
            side: Side::Buy,
            fill_price: FixedPrice::new(17_930),
            fill_size: 1,
            timestamp: UnixNanos::new(3),
        };
        publish_execution_report(&full, &mut decisions, &mut fp);
        let f2 = fc.try_pop().unwrap();
        assert_eq!(f2.decision_id, 91);
        assert!(matches!(f2.fill_type, FillType::Full));
        // Terminal fill cleans up the mapping.
        assert!(decisions.get(31).is_none());
    }

    #[test]
    fn fill_listener_logs_orphan_with_zero_decision_id() {
        let (_op, _oc, mut fp, mut fc) = create_order_fill_queues();
        let mut decisions = DecisionIdMap::new();

        let report = ExecutionReport::Rejected {
            order_id: 999,
            side: Side::Buy,
            reason: RejectReason::ExchangeReject,
            timestamp: UnixNanos::new(5),
        };
        // No prior submission for order 999 -> decision_id sentinel = 0, but the
        // event must still flow into the journal pathway for diagnostics.
        publish_execution_report(&report, &mut decisions, &mut fp);
        let f = fc.try_pop().unwrap();
        assert_eq!(f.order_id, 999);
        assert_eq!(f.decision_id, 0);
        assert!(matches!(
            f.fill_type,
            FillType::Rejected {
                reason: RejectReason::ExchangeReject
            }
        ));
    }

    #[tokio::test]
    async fn rithmic_submitter_placeholder_yields_connection_lost_reject() {
        let submitter = RithmicSubmitter::new("ACCT".into(), "CME".into());
        let evt = sample_order(1, 1);
        let result = submitter.submit_order(&evt).await;
        assert_eq!(result, Err(SubmissionError::ConnectionLost));
    }

    #[test]
    fn submission_error_maps_to_reject_reason() {
        assert_eq!(
            SubmissionError::ConnectionLost.to_reject_reason(),
            RejectReason::ConnectionLost
        );
        assert_eq!(
            SubmissionError::InvalidSymbol.to_reject_reason(),
            RejectReason::InvalidSymbol
        );
        assert_eq!(
            SubmissionError::ExchangeReject.to_reject_reason(),
            RejectReason::ExchangeReject
        );
        assert_eq!(
            SubmissionError::Timeout.to_reject_reason(),
            RejectReason::Unknown
        );
        assert_eq!(
            SubmissionError::Unknown.to_reject_reason(),
            RejectReason::Unknown
        );
    }
}
