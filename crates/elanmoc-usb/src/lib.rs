//! USB transport for the ELAN 04f3:0c90 fingerprint sensor.
//!
//! Moves bytes. Holds no protocol knowledge: it does not know what any opcode
//! means, and it never chooses an endpoint on the caller's behalf.

use std::time::Duration;

use nusb::transfer::{EndpointType, RequestBuffer, TransferError};
use nusb::Interface;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use error::UsbError;

mod error;

/// Vendor id of the sensor.
pub const VID: u16 = 0x04f3;
/// Product id of the sensor.
pub const PID: u16 = 0x0c90;

const INTERFACE: u8 = 0;
const EP_OUT: u8 = 0x01; // commands out, per protocol.md
const MAX_PACKET: usize = 64; // wMaxPacketSize on every endpoint, per descriptors.txt

/// The three IN endpoints, which are not interchangeable.
///
/// Commands that wait for a finger reply on [`EndpointIn::TouchWait`].
/// Everything else replies on [`EndpointIn::Status`]. Reading the wrong one
/// turns a successful operation into a timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointIn {
    /// `0x82`, image data.
    Image,
    /// `0x83`, status and data responses.
    Status,
    /// `0x84`, results of commands that wait for a touch.
    TouchWait,
}

impl EndpointIn {
    /// Wire address of this endpoint.
    pub fn address(self) -> u8 {
        match self {
            Self::Image => 0x82,
            Self::Status => 0x83,
            Self::TouchWait => 0x84,
        }
    }
}

/// One endpoint as the device describes itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointInfo {
    /// Wire address, with the direction bit set for IN endpoints.
    pub address: u8,
    /// `true` for IN, device to host.
    pub is_in: bool,
    /// Bulk, interrupt, isochronous or control.
    pub transfer_type: &'static str,
    /// `wMaxPacketSize`.
    pub max_packet_size: usize,
    /// `bInterval`.
    pub interval: u8,
}

/// An open, claimed handle to the sensor.
///
/// Dropping it releases interface 0.
pub struct Device {
    interface: Interface,
    device: nusb::Device,
    bus: u8,
    address: u8,
}

impl Device {
    /// Find and claim the sensor by vendor and product id.
    pub async fn open() -> Result<Self, UsbError> {
        let info = nusb::list_devices()?
            .find(|d| d.vendor_id() == VID && d.product_id() == PID)
            .ok_or(UsbError::NotFound)?;

        tracing::debug!(
            bus = info.bus_number(),
            address = info.device_address(),
            product = info.product_string().unwrap_or(""),
            "opening device"
        );

        let device = info.open().map_err(UsbError::from_open)?;
        let interface = device
            .detach_and_claim_interface(INTERFACE)
            .map_err(UsbError::from_open)?;

        Ok(Self {
            interface,
            device,
            bus: info.bus_number(),
            address: info.device_address(),
        })
    }

    /// Bus and device address the handle was opened on.
    pub fn location(&self) -> (u8, u8) {
        (self.bus, self.address)
    }

    /// Endpoints of the claimed interface, read from the live descriptors.
    pub fn endpoints(&self) -> Vec<EndpointInfo> {
        self.interface
            .descriptors()
            .flat_map(|alt| {
                alt.endpoints()
                    .map(|ep| EndpointInfo {
                        address: ep.address(),
                        is_in: ep.direction() == nusb::transfer::Direction::In,
                        transfer_type: match ep.transfer_type() {
                            EndpointType::Bulk => "bulk",
                            EndpointType::Interrupt => "interrupt",
                            EndpointType::Isochronous => "isochronous",
                            EndpointType::Control => "control",
                        },
                        max_packet_size: ep.max_packet_size(),
                        interval: ep.interval(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Write a command to `0x01`.
    pub async fn send(
        &self,
        data: &[u8],
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<(), UsbError> {
        tracing::debug!(dir = "OUT", len = data.len(), bytes = %hex(data));

        let transfer = self.interface.bulk_out(EP_OUT, data.to_vec());
        let completion = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(UsbError::Cancelled),
            () = tokio::time::sleep(timeout) => return Err(UsbError::Timeout(timeout)),
            completion = transfer => completion,
        };
        completion.status.map_err(UsbError::from)?;
        Ok(())
    }

    /// Read exactly `len` bytes from `ep`, reassembling across packet boundaries.
    ///
    /// Returns [`UsbError::ShortRead`] if the device ends the transfer early,
    /// which is a different situation from the device never answering
    /// ([`UsbError::Timeout`]).
    pub async fn recv(
        &self,
        ep: EndpointIn,
        len: usize,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, UsbError> {
        let deadline = Instant::now() + timeout;
        let mut buf: Vec<u8> = Vec::with_capacity(len);

        while buf.len() < len {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(UsbError::Timeout(timeout));
            }

            let want = (len - buf.len()).next_multiple_of(MAX_PACKET);
            let transfer = self.interface.bulk_in(ep.address(), RequestBuffer::new(want));
            let completion = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(UsbError::Cancelled),
                () = tokio::time::sleep(remaining) => return Err(UsbError::Timeout(timeout)),
                completion = transfer => completion,
            };

            let chunk = completion.into_result()?;
            let ended_early = chunk.len() < want;
            tracing::debug!(dir = "IN", ep = ep.address(), len = chunk.len(), bytes = %hex(&chunk));
            buf.extend_from_slice(&chunk);

            if buf.len() < len && ended_early {
                return Err(UsbError::ShortRead {
                    wanted: len,
                    got: buf.len(),
                });
            }
        }

        if buf.len() > len {
            tracing::debug!(
                expected = len,
                got = buf.len(),
                extra = %hex(&buf[len..]),
                "device sent more than the documented response length"
            );
        }
        buf.truncate(len);
        Ok(buf)
    }

    /// Send a command and read its reply as one operation.
    pub async fn cmd(
        &self,
        out: &[u8],
        ep: EndpointIn,
        in_len: usize,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, UsbError> {
        self.send(out, timeout, cancel).await?;
        self.recv(ep, in_len, timeout, cancel).await
    }

    /// Clear a STALL condition on an IN endpoint.
    pub fn clear_halt(&self, ep: EndpointIn) -> Result<(), UsbError> {
        self.interface.clear_halt(ep.address())?;
        Ok(())
    }

    /// Reset the port. The device re-enumerates, so this handle is dead afterwards.
    pub async fn reset(&self) -> Result<(), UsbError> {
        tracing::debug!("resetting device");
        self.device.reset()?;
        Ok(())
    }
}

/// Format bytes as lowercase space separated hex, the form used in `findings.md`.
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

impl From<TransferError> for UsbError {
    fn from(e: TransferError) -> Self {
        match e {
            TransferError::Cancelled => Self::Cancelled,
            TransferError::Disconnected => Self::Disconnected,
            TransferError::Stall => Self::Stall,
            TransferError::Fault | TransferError::Unknown => Self::Transfer(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_addresses_match_the_protocol_table() {
        assert_eq!(EndpointIn::Image.address(), 0x82);
        assert_eq!(EndpointIn::Status.address(), 0x83);
        assert_eq!(EndpointIn::TouchWait.address(), 0x84);
    }

    #[test]
    fn touch_wait_is_not_status() {
        assert_ne!(
            EndpointIn::TouchWait.address(),
            EndpointIn::Status.address()
        );
    }

    #[test]
    fn hex_formats_lowercase_space_separated() {
        assert_eq!(hex(&[0x40, 0xff, 0x02]), "40 ff 02");
    }

    #[test]
    fn hex_of_empty_is_empty() {
        assert_eq!(hex(&[]), "");
    }
}
