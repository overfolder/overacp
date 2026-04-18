mod startup;

pub use startup::{build_state_from_env, StartupError};

pub mod api;
pub mod auth;
pub mod hooks;
#[cfg(feature = "redis")]
pub mod redis_backend;
pub mod registry;
pub mod routes;
pub mod state;
pub mod tunnel;

pub use auth::{AuthError, Authenticator, Claims, StaticJwtAuthenticator};
pub use hooks::{
    BootError, BootProvider, DefaultBootProvider, DefaultQuotaPolicy, DefaultToolHost, QuotaError,
    QuotaPolicy, ToolError, ToolHost,
};
pub use registry::{
    AgentDescription, AgentEntry, AgentRegistry, AgentRegistryProvider, DeliveryOutcome,
    MessageQueue, MessageQueueProvider, QueueError, RegistryError, TunnelLease,
};
pub use routes::router;
pub use state::AppState;
pub use tunnel::{InMemoryStreamBroker, StreamBrokerProvider};
