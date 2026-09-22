//! Enumerate hidraw nodes through sysfs (no libudev needed).
//!
//! Everything read here (`uevent`, `report_descriptor`) is world-readable,
//! so discovery works even before the udev permission rule is installed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawNode {
    /// e.g. `hidraw3`
    pub sysname: String,
    /// e.g. `/dev/hidraw3`
    pub devnode: PathBuf,
    pub bus: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub name: String,
    pub phys: String,
    pub uniq: String,
    /// USB interface number parsed from `HID_PHYS` (`.../input3` -> 3).
    pub interface: Option<u8>,
    pub report_descriptor: Vec<u8>,
}

impl HidrawNode {
    pub fn is_usb(&self) -> bool {
        self.bus == 0x03
    }
}

const SYS_CLASS_HIDRAW: &str = "/sys/class/hidraw";

pub fn enumerate() -> io::Result<Vec<HidrawNode>> {
    enumerate_in(Path::new(SYS_CLASS_HIDRAW), Path::new("/dev"))
}

pub fn enumerate_in(class_dir: &Path, dev_dir: &Path) -> io::Result<Vec<HidrawNode>> {
    let entries = match fs::read_dir(class_dir) {
        Ok(entries) => entries,
        // No hidraw devices at all (or no hidraw support): nothing to list.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let sysname = entry.file_name().to_string_lossy().into_owned();
        if !sysname.starts_with("hidraw") {
            continue;
        }
        let device_dir = entry.path().join("device");
        let Ok(uevent) = fs::read_to_string(device_dir.join("uevent")) else {
            continue;
        };
        let Some(mut node) = parse_uevent(&uevent) else {
            continue;
        };
        node.devnode = dev_dir.join(&sysname);
        node.sysname = sysname;
        node.report_descriptor = fs::read(device_dir.join("report_descriptor")).unwrap_or_default();
        nodes.push(node);
    }
    nodes.sort_by_key(|node| natural_key(&node.sysname));
    Ok(nodes)
}

fn natural_key(sysname: &str) -> (usize, u32) {
    let digits = sysname.trim_start_matches(|c: char| !c.is_ascii_digit());
    (sysname.len() - digits.len(), digits.parse().unwrap_or(u32::MAX))
}

/// Parse the `uevent` file of a HID device. `devnode`/`sysname`/descriptor
/// are filled in by the caller.
pub fn parse_uevent(text: &str) -> Option<HidrawNode> {
    let mut hid_id = None;
    let mut name = String::new();
    let mut phys = String::new();
    let mut uniq = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "HID_ID" => hid_id = parse_hid_id(value),
            "HID_NAME" => name = value.to_string(),
            "HID_PHYS" => phys = value.to_string(),
            "HID_UNIQ" => uniq = value.to_string(),
            _ => {}
        }
    }
    let (bus, vendor_id, product_id) = hid_id?;
    let interface = phys.rsplit_once("/input").and_then(|(_, n)| n.parse::<u8>().ok());
    Some(HidrawNode {
        sysname: String::new(),
        devnode: PathBuf::new(),
        bus,
        vendor_id,
        product_id,
        name,
        phys,
        uniq,
        interface,
        report_descriptor: Vec::new(),
    })
}

/// `0003:00001532:00000565` -> (0x0003, 0x1532, 0x0565)
fn parse_hid_id(value: &str) -> Option<(u16, u16, u16)> {
    let mut parts = value.split(':');
    let bus = u32::from_str_radix(parts.next()?, 16).ok()?;
    let vendor = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product = u32::from_str_radix(parts.next()?, 16).ok()?;
    Some((
        u16::try_from(bus).ok()?,
        u16::try_from(vendor).ok()?,
        u16::try_from(product).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UEVENT: &str = "DRIVER=hid-generic\n\
        HID_ID=0003:00001532:00000565\n\
        HID_NAME=MediaTek Inc Razer BlackShark V2 HS 2.4\n\
        HID_PHYS=usb-0000:2b:00.3-1/input3\n\
        HID_UNIQ=0000000000000000\n\
        MODALIAS=hid:b0003g0001v00001532p00000565\n";

    #[test]
    fn parses_razer_uevent() {
        let node = parse_uevent(UEVENT).unwrap();
        assert_eq!(node.bus, 0x0003);
        assert_eq!(node.vendor_id, 0x1532);
        assert_eq!(node.product_id, 0x0565);
        assert_eq!(node.name, "MediaTek Inc Razer BlackShark V2 HS 2.4");
        assert_eq!(node.interface, Some(3));
        assert!(node.is_usb());
    }

    #[test]
    fn bluetooth_phys_has_no_interface() {
        let node = parse_uevent("HID_ID=0005:0000046D:0000B383\nHID_PHYS=8c:88:4b:66:18:6b\n").unwrap();
        assert_eq!(node.bus, 0x0005);
        assert_eq!(node.interface, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_uevent("HID_ID=zz:1:2\n").is_none());
        assert!(parse_uevent("HID_NAME=x\n").is_none());
    }

    #[test]
    fn enumerates_fake_sysfs_tree() {
        let root = std::env::temp_dir().join(format!("synapse-sysfs-{}", std::process::id()));
        let class = root.join("class");
        for (name, uevent) in [("hidraw10", UEVENT), ("hidraw2", "HID_ID=0003:0000046D:0000C548\n")] {
            let dir = class.join(name).join("device");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("uevent"), uevent).unwrap();
            fs::write(dir.join("report_descriptor"), [0x06, 0x14, 0xFF]).unwrap();
        }
        fs::create_dir_all(class.join("not-hidraw")).unwrap();

        let nodes = enumerate_in(&class, Path::new("/dev")).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].sysname, "hidraw2");
        assert_eq!(nodes[1].sysname, "hidraw10");
        assert_eq!(nodes[1].devnode, PathBuf::from("/dev/hidraw10"));
        assert_eq!(nodes[1].report_descriptor, vec![0x06, 0x14, 0xFF]);
    }
}
