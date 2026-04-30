// `FillEvent` is defined in `crate::types::order` alongside `OrderEvent`/`FillType`.
// The `events` module re-exports it so existing imports under
// `crate::events::FillEvent` continue to resolve.
pub use crate::types::FillEvent;
