//! Limits of the persistent payload-only transfer socket that carries binary stream frames
//! apart from the control socket.

use crate::wire::stream::{HEADER_SIZE, MAX_TRANSFER_FRAME_PAYLOAD_BYTES};
use std::time::Duration;

/// The transfer socket's endpoint; it authenticates during the upgrade with the per-connection
/// token from `transfer_socket_open`.
pub const AGENT_TRANSFER_WS_PATH: &str = "/api/v1/agent-transfer/ws";
/// Bounds transfer messages to one framed stream chunk and prevents oversized allocations.
pub const MAX_TRANSFER_WEBSOCKET_MESSAGE_BYTES: usize =
    HEADER_SIZE + MAX_TRANSFER_FRAME_PAYLOAD_BYTES;
/// Avoids a tight reconnect loop while keeping temporary payload outages short.
pub const TRANSFER_RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// Keeps each direction limited to one queued payload frame for end-to-end backpressure.
pub const TRANSFER_OUTBOUND_QUEUE_CAPACITY: usize = 1;
