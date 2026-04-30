// futures_bmad_broker: Broker connectivity and order routing.

pub mod adapter;
pub(crate) mod connection;
pub mod market_data;
pub(crate) mod message_validator;
pub(crate) mod messages;
pub mod order_routing;
pub mod position_flatten;

pub use adapter::RithmicAdapter;
pub use market_data::MarketDataStream;
pub use message_validator::ValidationError;
pub use order_routing::{
    FillQueueConsumer, FillQueueProducer, ORDER_FILL_QUEUE_CAPACITY, OrderQueueConsumer,
    OrderQueueProducer, OrderSubmitter, SubmissionError, create_order_fill_queues,
    route_pending_orders,
};
pub use position_flatten::{
    FLATTEN_MAX_ATTEMPTS, FLATTEN_RETRY_INTERVAL, FlattenOutcome, FlattenRequest, FlattenRetry,
};
