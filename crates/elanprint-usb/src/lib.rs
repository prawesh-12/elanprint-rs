//! Byte transport, no protocol knowledge.

use std::time::Duration;

use nusb::transfer::{EndpointType, RequestBuffer, TransferError};
use nusb::Interface;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use error::UsbError;

mod error;

pub const VID: u16 = 0x04f3;
pub const PID: u16 = 0x0c90;

const INTERFACE: u8 = 0;
const EP_OUT: u8 = 0x01; // commands out, per protocol.md
const MAX_PACKET: usize = 64; // wMaxPacketSize, per descriptors.txt

/// IN endpoints are not interchangeable; wrong one times out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointIn {
    Image,
    Status,
    TouchWait,
}

impl EndpointIn {
    pub fn address(self) -> u8 {
        match self {
            Self::Image => 0x82,
            Self::Status => 0x83,
            Self::TouchWait => 0x84,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointInfo {
    pub address: u8,
    pub is_in: bool,
    pub transfer_type: &'static str,
    pub max_packet_size: usize,
    pub interval: u8,
}

/// Dropping releases interface 0.
pub struct Device {
    interface: Interface,
    device: nusb::Device,
    bus: u8,
    address: u8,
}

impl Device {
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

    pub fn location(&self) -> (u8, u8) {
        (self.bus, self.address)
    }

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

    /// Read exactly `len` bytes from `ep`, reassembling across packets.
    ///
    /// [`UsbError::ShortRead`] means the device ended the transfer early.
    /// [`UsbError::Timeout`] means it never answered at all.
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
                    data: buf,
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

    /// Post a read on two IN endpoints and take whichever answers first.
    ///
    /// Sends nothing; the loser is cancelled when its future drops. Finds
    /// which endpoint a reply arrives on, which usbmon cannot show: with no
    /// posted read the device is NAKed and nothing is recorded.
    pub async fn recv_first(
        &self,
        a: EndpointIn,
        b: EndpointIn,
        len: usize,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<(EndpointIn, Vec<u8>), UsbError> {
        let want = len.next_multiple_of(MAX_PACKET).max(MAX_PACKET);
        let on_a = self.interface.bulk_in(a.address(), RequestBuffer::new(want));
        let on_b = self.interface.bulk_in(b.address(), RequestBuffer::new(want));

        let (ep, completion) = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(UsbError::Cancelled),
            () = tokio::time::sleep(timeout) => return Err(UsbError::Timeout(timeout)),
            completion = on_a => (a, completion),
            completion = on_b => (b, completion),
        };

        let bytes = completion.into_result()?;
        tracing::debug!(dir = "IN", ep = ep.address(), len = bytes.len(), bytes = %hex(&bytes));
        Ok((ep, bytes))
    }

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

    pub fn clear_halt(&self, ep: EndpointIn) -> Result<(), UsbError> {
        self.interface.clear_halt(ep.address())?;
        Ok(())
    }

    /// Resets the port. The device re-enumerates; this handle is then dead.
    pub async fn reset(&self) -> Result<(), UsbError> {
        tracing::debug!("resetting device");
        self.device.reset()?;
        Ok(())
    }
}

/// What a protocol driver needs from the wire.
///
/// [`Device`] is the real one. A fake that records sent bytes is how the
/// daemon's erase paths are proven with no sensor attached.
pub trait Transport: Sized + Send + Sync + 'static {
    fn open() -> impl std::future::Future<Output = Result<Self, UsbError>> + Send;

    fn cmd<'a>(
        &'a self,
        out: &'a [u8],
        ep: EndpointIn,
        in_len: usize,
        timeout: Duration,
        cancel: &'a CancellationToken,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, UsbError>> + Send + 'a;
}

impl Transport for Device {
    fn open() -> impl std::future::Future<Output = Result<Self, UsbError>> + Send {
        Device::open()
    }

    fn cmd<'a>(
        &'a self,
        out: &'a [u8],
        ep: EndpointIn,
        in_len: usize,
        timeout: Duration,
        cancel: &'a CancellationToken,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, UsbError>> + Send + 'a {
        Device::cmd(self, out, ep, in_len, timeout, cancel)
    }
}

/// Lowercase space separated hex, as `docs/findings.md` writes it.
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
