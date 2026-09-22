//! Minimal HID report descriptor parser.
//!
//! Only what discovery needs: which reports (input/output/feature, report ID,
//! byte length) exist inside which top-level application collection
//! (usage page + usage). Used to pick the vendor control interface.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportKind {
    Input,
    Output,
    Feature,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportInfo {
    pub kind: ReportKind,
    /// 0 when the descriptor does not use report IDs.
    pub report_id: u8,
    /// Payload length in bytes, excluding the report ID byte.
    pub size_bytes: usize,
    /// Usage page of the enclosing top-level collection.
    pub usage_page: u16,
    /// Usage of the enclosing top-level collection.
    pub usage: u16,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u16,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

/// Parse all reports declared by `desc`, merged per
/// (collection, kind, report ID).
pub fn parse_reports(desc: &[u8]) -> Vec<ReportInfo> {
    let mut reports: Vec<(ReportInfo, u32)> = Vec::new(); // (info, total bits)
    let mut globals = Globals::default();
    let mut global_stack: Vec<Globals> = Vec::new();
    // First Usage of the pending main item, and whether it was an extended
    // (4 byte) usage that carries its own usage page.
    let mut local_usage: Option<(u32, bool)> = None;
    let mut depth = 0usize;
    let mut top_collection = (0u16, 0u16);

    let mut i = 0usize;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: 0xFE, size, tag, data...
            let size = *desc.get(i + 1).unwrap_or(&0) as usize;
            i += 3 + size;
            continue;
        }
        let size = match prefix & 0x03 {
            3 => 4,
            n => n as usize,
        };
        if i + 1 + size > desc.len() {
            break; // truncated descriptor
        }
        let data = &desc[i + 1..i + 1 + size];
        let value = data.iter().rev().fold(0u32, |acc, &byte| (acc << 8) | u32::from(byte));
        let item_type = (prefix >> 2) & 0x03;
        let tag = prefix >> 4;
        i += 1 + size;

        match item_type {
            // Main items
            0 => {
                match tag {
                    0xA => {
                        // Collection
                        if depth == 0 {
                            let (usage, extended) = local_usage.unwrap_or((0, false));
                            let page = if extended {
                                (usage >> 16) as u16
                            } else {
                                globals.usage_page
                            };
                            top_collection = (page, usage as u16);
                        }
                        depth += 1;
                    }
                    0xC => depth = depth.saturating_sub(1), // End Collection
                    0x8 | 0x9 | 0xB => {
                        let kind = match tag {
                            0x8 => ReportKind::Input,
                            0x9 => ReportKind::Output,
                            _ => ReportKind::Feature,
                        };
                        let bits = globals.report_size.saturating_mul(globals.report_count);
                        let (usage_page, usage) = top_collection;
                        let id = globals.report_id;
                        match reports.iter_mut().find(|(r, _)| {
                            r.kind == kind && r.report_id == id && r.usage_page == usage_page && r.usage == usage
                        }) {
                            Some((_, total)) => *total = total.saturating_add(bits),
                            None => reports.push((
                                ReportInfo {
                                    kind,
                                    report_id: id,
                                    size_bytes: 0,
                                    usage_page,
                                    usage,
                                },
                                bits,
                            )),
                        }
                    }
                    _ => {}
                }
                // Local items only apply to the next main item.
                local_usage = None;
            }
            // Global items
            1 => match tag {
                0x0 => globals.usage_page = value as u16,
                0x7 => globals.report_size = value,
                0x8 => globals.report_id = value as u8,
                0x9 => globals.report_count = value,
                0xA => global_stack.push(globals),
                0xB => {
                    if let Some(g) = global_stack.pop() {
                        globals = g;
                    }
                }
                _ => {}
            },
            // Local items: remember the first Usage for the next Collection.
            2 if tag == 0x0 && local_usage.is_none() => local_usage = Some((value, size == 4)),
            _ => {}
        }
    }

    reports
        .into_iter()
        .map(|(mut info, bits)| {
            info.size_bytes = bits.div_ceil(8) as usize;
            info
        })
        .collect()
}

/// Find a report of `kind` with `report_id` inside a collection on `usage_page`.
pub fn find_report(desc: &[u8], usage_page: u16, kind: ReportKind, report_id: u8) -> Option<ReportInfo> {
    parse_reports(desc)
        .into_iter()
        .find(|r| r.usage_page == usage_page && r.kind == kind && r.report_id == report_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Report descriptor of the Razer BlackShark V2 HyperSpeed dongle (1532:0565), interface 3.
    pub const BLACKSHARK_V2_HS_DONGLE: [u8; 191] = [
        0x06, 0x13, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x85, 0x06, 0x09, 0x00, 0x75, 0x08,
        0x95, 0x3D, 0x91, 0x02, 0x85, 0x07, 0x09, 0x00, 0x75, 0x08, 0x95, 0x3D, 0x81, 0x02, 0xC0, 0x05, 0x0C, 0x09,
        0x01, 0xA1, 0x01, 0x85, 0x0C, 0x15, 0x00, 0x25, 0x01, 0x09, 0xE9, 0x09, 0xEA, 0x09, 0xE2, 0x09, 0xCD, 0x09,
        0xB5, 0x09, 0xB6, 0x75, 0x01, 0x95, 0x06, 0x81, 0x02, 0x09, 0x00, 0x95, 0x02, 0x81, 0x02, 0xC0, 0x05, 0x0B,
        0x09, 0x05, 0xA1, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x85, 0x05, 0x09, 0x20, 0x95, 0x01, 0x81, 0x22,
        0x09, 0x2F, 0x95, 0x01, 0x81, 0x06, 0x09, 0x24, 0x09, 0x21, 0x09, 0x97, 0x09, 0x2A, 0x09, 0x50, 0x95, 0x05,
        0x81, 0x06, 0x09, 0x07, 0x05, 0x09, 0x09, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x05, 0x08, 0x85, 0x05,
        0x09, 0x17, 0x09, 0x09, 0x09, 0x18, 0x09, 0x20, 0x09, 0x21, 0x09, 0x2A, 0x95, 0x06, 0x91, 0x22, 0x95, 0x02,
        0x91, 0x01, 0xC0, 0x06, 0x14, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x85, 0x02, 0x09,
        0x00, 0x75, 0x08, 0x95, 0x08, 0xB2, 0x02, 0x01, 0x85, 0x02, 0x09, 0x00, 0x75, 0x08, 0x95, 0x3F, 0x91, 0x02,
        0x85, 0x02, 0x09, 0x00, 0x75, 0x08, 0x95, 0x3F, 0x81, 0x02, 0xC0,
    ];

    #[test]
    fn parses_blackshark_dongle_descriptor() {
        let reports = parse_reports(&BLACKSHARK_V2_HS_DONGLE);
        let get = |page, kind, id| {
            reports
                .iter()
                .find(|r| r.usage_page == page && r.kind == kind && r.report_id == id)
                .unwrap_or_else(|| panic!("missing {page:04X} {kind:?} {id}"))
                .clone()
        };

        // Airoha/MediaTek RACE channel
        assert_eq!(get(0xFF13, ReportKind::Output, 6).size_bytes, 61);
        assert_eq!(get(0xFF13, ReportKind::Input, 7).size_bytes, 61);
        // Media keys
        assert_eq!(get(0x000C, ReportKind::Input, 12).size_bytes, 1);
        // Telephony (hook switch, phone mute) + its LEDs
        assert_eq!(get(0x000B, ReportKind::Input, 5).size_bytes, 1);
        assert_eq!(get(0x000B, ReportKind::Output, 5).size_bytes, 1);
        // Razer control channel
        let control = get(0xFF14, ReportKind::Output, 2);
        assert_eq!(control.size_bytes, 63);
        assert_eq!(control.usage, 0x01);
        assert_eq!(get(0xFF14, ReportKind::Input, 2).size_bytes, 63);
        assert_eq!(get(0xFF14, ReportKind::Feature, 2).size_bytes, 8);
        assert_eq!(reports.len(), 8);
    }

    #[test]
    fn find_report_matches_page_kind_and_id() {
        assert!(find_report(&BLACKSHARK_V2_HS_DONGLE, 0xFF14, ReportKind::Output, 2).is_some());
        assert!(find_report(&BLACKSHARK_V2_HS_DONGLE, 0xFF14, ReportKind::Output, 6).is_none());
        assert!(find_report(&[], 0xFF14, ReportKind::Output, 2).is_none());
    }

    #[test]
    fn extended_usage_sets_the_collection_page() {
        // Usage (4 bytes: page 0xFF14, usage 0x0001), Collection, Output report 2.
        let desc = [
            0x0B, 0x01, 0x00, 0x14, 0xFF, 0xA1, 0x01, 0x85, 0x02, 0x75, 0x08, 0x95, 0x3F, 0x91, 0x02, 0xC0,
        ];
        let report = find_report(&desc, 0xFF14, ReportKind::Output, 2).expect("extended usage page");
        assert_eq!(report.usage, 0x0001);
        assert_eq!(report.size_bytes, 63);
    }

    #[test]
    fn survives_truncated_and_long_items() {
        // Truncated short item
        assert!(parse_reports(&[0x06, 0x14]).is_empty());
        // A long item followed by a normal output report
        let desc = [
            0xFE, 0x02, 0x10, 0xAA, 0xBB, 0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x75, 0x08, 0x95, 0x04, 0x91, 0x02,
            0xC0,
        ];
        let reports = parse_reports(&desc);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].usage_page, 0xFF00);
        assert_eq!(reports[0].size_bytes, 4);
        assert_eq!(reports[0].report_id, 0);
    }
}
