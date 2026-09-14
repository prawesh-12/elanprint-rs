use std::io;
use std::time::Duration;

use nusb::transfer::TransferError;

/// Everything the transport layer can fail with.
///
/// `Timeout`, `Cancelled` and `Disconnected` are separate variants on purpose.
/// Upper layers treat them very differently: a timeout on a touch-wait is
/// normal, a disconnect is not.
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
        /// What did arrive. `finger_info` has a documented two byte error form
        /// against an expected length of 70, so these bytes still parse.
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
    /// Map an open or claim failure, keeping `Permission` distinguishable.
    ///
    /// A missing udev rule is the most common setup mistake, and it is worth a
    /// better message than "permission denied (os error 13)".
    pub(crate) fn from_open(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::PermissionDenied => Self::Permission,
            io::ErrorKind::NotFound => Self::NotFound,
            _ => Self::Io(e),
        }
    }
}
