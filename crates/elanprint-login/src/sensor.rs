//! What the sensor says about itself, read from sysfs.
//!
//! No daemon, no root, no device bytes. It is all the window can show before
//! anything is claimed, and what tells someone with different hardware what
//! they have.

use std::path::Path;

const VID: &str = "04f3";
const PID: &str = "0c90";
const USB_DEVICES: &str = "/sys/bus/usb/devices";

pub struct Sensor {
    pub vendor: String,
    pub product: String,
    pub manufacturer: Option<String>,
    pub name: Option<String>,
    pub bus: Option<String>,
    /// From `bcdDevice`, which matched `fw_ver` on the unit this was written on.
    pub firmware: Option<String>,
}

impl Sensor {
    /// "ELAN ELAN:ARM-M4", or the ids when the unit leaves the strings empty.
    pub fn title(&self) -> String {
        match (&self.manufacturer, &self.name) {
            (Some(m), Some(n)) => format!("{m} {n}"),
            (None, Some(n)) => n.clone(),
            (Some(m), None) => format!("{m} {}:{}", self.vendor, self.product),
            (None, None) => format!("ELAN {}:{}", self.vendor, self.product),
        }
    }

    pub fn address(&self) -> String {
        match &self.bus {
            Some(bus) => format!("{}:{} on USB bus {bus}", self.vendor, self.product),
            None => format!("{}:{}", self.vendor, self.product),
        }
    }

    pub fn ids(&self) -> String {
        format!("{}:{}", self.vendor, self.product)
    }
}

pub enum Found {
    /// An `04f3:0c90`, the one this driver speaks to.
    Supported(Sensor),
    /// An ELAN device with a different product id.
    Unsupported(Sensor),
    None,
}

/// Walk sysfs for an ELAN device, preferring the supported one.
pub fn scan() -> Found {
    let Ok(entries) = std::fs::read_dir(USB_DEVICES) else {
        return Found::None;
    };
    let mut other = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(vendor) = attr(&dir, "idVendor") else {
            continue;
        };
        if vendor != VID {
            continue;
        }
        let Some(product) = attr(&dir, "idProduct") else {
            continue;
        };
        let sensor = Sensor {
            manufacturer: attr(&dir, "manufacturer"),
            name: attr(&dir, "product"),
            bus: attr(&dir, "busnum"),
            firmware: attr(&dir, "bcdDevice").as_deref().and_then(firmware),
            vendor,
            product,
        };
        if sensor.product == PID {
            return Found::Supported(sensor);
        }
        other = Some(sensor);
    }
    match other {
        Some(s) => Found::Unsupported(s),
        None => Found::None,
    }
}

fn attr(dir: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(name)).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `bcdDevice` is packed BCD: "0108" is 1.8.
fn firmware(raw: &str) -> Option<String> {
    let v = u16::from_str_radix(raw, 16).ok()?;
    let major = ((v >> 12) & 0xf) * 10 + ((v >> 8) & 0xf);
    let minor = ((v >> 4) & 0xf) * 10 + (v & 0xf);
    Some(format!("{major}.{minor}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcd_device_reads_as_a_version() {
        assert_eq!(firmware("0108").as_deref(), Some("1.8"));
        assert_eq!(firmware("0200").as_deref(), Some("2.0"));
        assert_eq!(firmware("0112").as_deref(), Some("1.12"));
        assert_eq!(firmware("zzzz"), None);
    }

    #[test]
    fn a_unit_with_no_strings_still_has_a_title() {
        let s = Sensor {
            vendor: "04f3".into(),
            product: "0c90".into(),
            manufacturer: None,
            name: None,
            bus: Some("1".into()),
            firmware: None,
        };
        assert_eq!(s.title(), "ELAN 04f3:0c90");
        assert_eq!(s.address(), "04f3:0c90 on USB bus 1");
    }

    #[test]
    fn strings_are_used_when_the_unit_populates_them() {
        let s = Sensor {
            vendor: "04f3".into(),
            product: "0c90".into(),
            manufacturer: Some("ELAN".into()),
            name: Some("ELAN:ARM-M4".into()),
            bus: Some("1".into()),
            firmware: Some("1.8".into()),
        };
        assert_eq!(s.title(), "ELAN ELAN:ARM-M4");
    }
}
