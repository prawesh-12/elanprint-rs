use std::io;
use std::time::Duration;

use nusb::transfer::TransferError;

/// Everything the transport layer can fail with.
///
/// A touch-wait timeout is normal, a disconnect is not, so they stay distinct.
#[derive(Debug, thiserror::Error)]
pub enum UsbError {
    #[error("no ELAN 04f3:0c90 fingerprint sensor on this machine")]
    NotFound,
    /// An ELAN sensor, but not one whose protocol is confirmed here.
    #[error(
        "found ELAN 04f3:{0:04x}, which this driver does not support. Only \
         04f3:0c90 is confirmed, and sending its bytes to another model \
         could write flash"
    )]
    UnsupportedProduct(u16),
    #[error("permission denied, check the udev rule")]
    Permission,
    #[error("transfer timed out after {0:?}")]
    Timeout(Duration),
    #[error(
        "no reply on the touch-wait endpoint after {0:?}. The session was \
         armed with enrolled_num, which this firmware is expected to need. \
         On another firmware the arming rule may not hold. The version is \
         logged on the first claim"
    )]
    TouchWaitSilent(Duration),
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
    #[error("{0}")]
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
