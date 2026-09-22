---
name: Device support request / Cihaz desteği talebi
about: Ask for (or help with) support for another Razer device
title: "Device support: <model name>"
labels: device-support
---

**Device / Cihaz**
Model name and whether it is connected through a dongle, a cable or Bluetooth:

**USB ID** (`lsusb | grep 1532`):
```
```

**HID nodes** (`for h in /sys/class/hidraw/hidraw*; do echo "$h: $(grep HID_ID $h/device/uevent)"; done`):
```
```

**Report descriptor** (`xxd /sys/class/hidraw/hidrawN/device/report_descriptor` for the Razer node):
```
```

**Which settings does Synapse offer for this device?**
EQ, sidetone, battery, lighting, DPI, …

**Can you help test or capture the protocol?**
- [ ] I can test builds on Linux
- [ ] I can capture USB traffic from Synapse on Windows (see CONTRIBUTING.md)
