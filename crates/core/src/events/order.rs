// `OrderEvent` is defined in `crate::types::order` (the routing-queue payload —
// `Copy`, slim, `decision_id` mandatory). The `events` module re-exports it so
// existing imports under `crate::events::OrderEvent` continue to resolve.
pub use crate::types::OrderEvent;
