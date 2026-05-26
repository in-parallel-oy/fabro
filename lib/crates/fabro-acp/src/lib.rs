pub mod command;

#[cfg(feature = "runtime")]
pub mod error;
#[cfg(feature = "runtime")]
pub mod session;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

#[cfg(feature = "runtime")]
mod transport;

/// Re-export the ACP wire-level `SessionUpdate` type so callers
/// registering an `on_session_update` callback (e.g.
/// `fabro-workflow`) don't need to depend on
/// `agent-client-protocol` directly.
#[cfg(feature = "runtime")]
pub use agent_client_protocol::schema::SessionUpdate;
pub use command::{AcpCommandError, AcpProcessSpec};
#[cfg(feature = "runtime")]
pub use error::{AcpError, AcpProcessExit};
#[cfg(feature = "runtime")]
pub use session::{
    AcpControlHandle, AcpLiveControl, AcpRunRequest, AcpRunResult, AcpSessionUpdateCallback,
    render_stop_reason, run_acp_turn,
};
