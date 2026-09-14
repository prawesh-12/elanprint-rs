use std::io;
use std::time::Duration;

use nusb::transfer::TransferError;

/// Everything the transport layer can fail with.
///
/// A touch-wait timeout is normal, a disconnect is not, so they stay distinct.
#[derive(Debug, thiserror::Error)]
pub enum UsbError {
    #[error("device not found (04f3:0c90)")]
    NotFound,
    #[error("permission denied, check the udev rule")]
    Permission,
    #[error("transfer timed out after {0:?}")]
    Timeout(Duration),
    #[error("cancelled")]
    Cancelled,
    #[error("short read: wanted {wanted}, got {got}")]
    ShortRead {
        wanted: usize,
        got: usize,
        /// What arrived. `finger_info`'s 2 byte form still parses.
        data: Vec<u8>,
    },
    #[error("device disconnected")]
    Disconnected,
    #[error("endpoint stalled")]
    Stall,
    #[error("transfer failed: {0}")]
    Transfer(TransferError),
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

impl UsbError {
    pub(crate) fn from_open(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::PermissionDenied => Self::Permission,
            io::ErrorKind::NotFound => Self::NotFound,
            _ => Self::Io(e),
        }
    }
}
