//! Supported device models and discovery.

pub mod blackshark_v2_hs;

use serde::Serialize;

use crate::Result;
use crate::hid::Transport;
use crate::hid::descriptor::{self, ReportKind};
use crate::hid::hidraw::Hidraw;
use crate::hid::sysfs::{self, HidrawNode};

pub use blackshark_v2_hs::BlackSharkV2Hs;

pub const RAZER_VID: u16 = 0x1532;

/// Set to `1` to use a simulated headset instead of real hardware.
pub const SIMULATE_ENV: &str = "SYNAPSE_LINUX_SIMULATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    /// 2.4 GHz HyperSpeed dongle; commands are relayed to the headset.
    Dongle,
    /// Headset plugged in directly with a USB cable.
    Wired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// MediaTek based HyperSpeed headsets (report 0x02 control channel).
    MediatekHeadset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Model {
    pub vid: u16,
    pub pid: u16,
    pub name: &'static str,
    pub family: Family,
    pub connection: Connection,
    /// Not verified on real hardware.
    pub experimental: bool,
}

pub const MODELS: &[Model] = &[
    Model {
        vid: RAZER_VID,
        pid: 0x0565,
        name: "Razer BlackShark V2 HyperSpeed",
        family: Family::MediatekHeadset,
        connection: Connection::Dongle,
        experimental: false,
    },
    Model {
        vid: RAZER_VID,
        pid: 0x056E,
        name: "Razer BlackShark V2 HyperSpeed",
        family: Family::MediatekHeadset,
        connection: Connection::Wired,
        experimental: false,
    },
    Model {
        vid: RAZER_VID,
        pid: 0x0566,
        name: "Razer BlackShark V2 HyperSpeed",
        family: Family::MediatekHeadset,
        connection: Connection::Dongle,
        experimental: true,
    },
];

pub fn lookup(vid: u16, pid: u16) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.vid == vid && m.pid == pid)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Hidraw(HidrawNode),
    Simulated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundDevice {
    pub model: &'static Model,
    pub location: Location,
}

impl FoundDevice {
    /// `/dev/hidrawN`, or `simulated`.
    pub fn path_display(&self) -> String {
        match &self.location {
            Location::Hidraw(node) => node.devnode.display().to_string(),
            Location::Simulated => "simulated".into(),
        }
    }

    pub fn is_simulated(&self) -> bool {
        matches!(self.location, Location::Simulated)
    }
}

pub fn simulation_requested() -> bool {
    std::env::var(SIMULATE_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// The simulated headset (dongle connection).
pub fn simulated() -> Vec<FoundDevice> {
    vec![FoundDevice {
        model: &MODELS[0],
        location: Location::Simulated,
    }]
}

/// Find every supported device that is currently plugged in
/// (or the simulated one when [`SIMULATE_ENV`] is set).
pub fn discover() -> Vec<FoundDevice> {
    if simulation_requested() {
        return simulated();
    }
    match sysfs::enumerate() {
        Ok(nodes) => select_devices(nodes),
        Err(err) => {
            log::warn!("cannot enumerate hidraw devices: {err}");
            Vec::new()
        }
    }
}

/// Keep the nodes that belong to a supported model and expose its control
/// interface (other interfaces of the same USB device are skipped).
pub fn select_devices(nodes: Vec<HidrawNode>) -> Vec<FoundDevice> {
    nodes
        .into_iter()
        .filter_map(|node| {
            let model = lookup(node.vendor_id, node.product_id)?;
            is_control_interface(model, &node).then_some(FoundDevice {
                model,
                location: Location::Hidraw(node),
            })
        })
        .collect()
}

fn is_control_interface(model: &Model, node: &HidrawNode) -> bool {
    match model.family {
        Family::MediatekHeadset => {
            if node.report_descriptor.is_empty() {
                // Descriptor unreadable: fall back to the known interface number.
                return node.interface.is_none_or(|i| i == 3);
            }
            [0xFF14, 0xFF00].iter().any(|&page| {
                descriptor::find_report(
                    &node.report_descriptor,
                    page,
                    ReportKind::Output,
                    blackshark_v2_hs::protocol::REPORT_ID,
                )
                .is_some()
            })
        }
    }
}

/// An opened device. One variant per protocol family.
pub enum Device {
    BlackSharkV2Hs(BlackSharkV2Hs<Box<dyn Transport>>),
}

pub fn open(found: &FoundDevice) -> Result<Device> {
    let transport: Box<dyn Transport> = match &found.location {
        Location::Hidraw(node) => Box::new(Hidraw::open(&node.devnode)?),
        Location::Simulated => Box::new(blackshark_v2_hs::sim::SimTransport::new(found.model.connection)),
    };
    match found.model.family {
        Family::MediatekHeadset => Ok(Device::BlackSharkV2Hs(BlackSharkV2Hs::new(transport, found.model))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::sysfs::parse_uevent;

    fn node(pid: u16, descriptor: Vec<u8>, phys: &str) -> HidrawNode {
        let mut node = parse_uevent(&format!("HID_ID=0003:00001532:{pid:08X}\nHID_PHYS={phys}\n")).unwrap();
        node.sysname = "hidraw0".into();
        node.devnode = "/dev/hidraw0".into();
        node.report_descriptor = descriptor;
        node
    }

    #[test]
    fn selects_only_the_control_interface_of_known_models() {
        let control = vec![
            0x06, 0x14, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, 0x75, 0x08, 0x95, 0x3F, 0x91, 0x02, 0xC0,
        ];
        let media_keys_only = vec![
            0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x0C, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xC0,
        ];
        let found = select_devices(vec![
            node(0x0565, control.clone(), "usb-1/input3"),
            node(0x0565, media_keys_only, "usb-1/input4"),
            node(0x0084, control, "usb-2/input0"), // unknown PID (a mouse)
        ]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].model.pid, 0x0565);
        assert_eq!(found[0].model.connection, Connection::Dongle);
    }

    #[test]
    fn unreadable_descriptor_falls_back_to_interface_number() {
        assert_eq!(select_devices(vec![node(0x056E, vec![], "usb-1/input3")]).len(), 1);
        assert_eq!(select_devices(vec![node(0x056E, vec![], "usb-1/input0")]).len(), 0);
    }

    #[test]
    fn lookup_knows_all_models() {
        assert_eq!(lookup(RAZER_VID, 0x056E).unwrap().connection, Connection::Wired);
        assert!(lookup(RAZER_VID, 0x1234).is_none());
        assert!(lookup(0x046D, 0x0565).is_none());
    }
}
