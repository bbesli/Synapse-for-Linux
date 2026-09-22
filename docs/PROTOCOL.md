# BlackShark V2 HyperSpeed control protocol

Notes for the MediaTek based Razer BlackShark V2 HyperSpeed as implemented in
`crates/synapse-core/src/devices/blackshark_v2_hs/`. The register map comes from
the MIT licensed reverse engineering work at
<https://github.com/justik13/razer-blackshark-v2-hyperspeed-webhid>; everything
marked *verified* was checked against a real dongle (`1532:0565`, headset and
dongle firmware 1.07.0.0) while writing this program.

This protocol is **not** the classic 90-byte Razer feature-report protocol used
by mice and keyboards (and by OpenRazer); the dongle is a MediaTek/Airoha design.

## USB layout

| Interface | Class | Purpose |
|---|---|---|
| 0–2 | Audio (UAC1) | 48 kHz stereo playback, 48 kHz mono microphone |
| 3 | HID | everything below, one `/dev/hidraw*` node |

HID interface 3 has four top-level collections:

| Usage page | Reports | Purpose |
|---|---|---|
| `0xFF13` | out 6 / in 7, 61 bytes | Airoha "RACE" service channel (firmware update etc.) — **not used** |
| `0x000C` | in 12 | media keys (volume wheel) — handled by the kernel |
| `0x000B` | in 5 / out 5 | telephony: hook switch, phone mute |
| `0xFF14` | out 2 / in 2, 63 bytes; feature 2, 8 bytes | **Razer control channel** |

The control interface is chosen by looking for an output report with ID 2 on
usage page `0xFF14` in the report descriptor.

## Frame format

All exchanges are 64-byte reports with report ID `0x02`, written with
`write(2)` and read with `read(2)` on the hidraw node (the dongle has no
interrupt OUT endpoint, so the kernel sends output reports as SET_REPORT).
Offsets include the report ID:

```
[0]      0x02        report ID
[1]      0x00        request: 0x00 / reply: 0x02 (verified)
[2]      0x60|seq    client mask + 5 bit sequence number, echoed in the reply
[3..5]   0x00
[6]      len         4 + number of parameter bytes
[7]      0x00
[8]      dir         0x00 request, 0x80 reply
[9]      domain      0x80 = relay to the headset over 2.4 GHz, 0x00 = local
[10]     cmd         register; SET = GET | 0x80
[11]     status      0x00 in requests, 0x01 in replies (verified)
[12]     count       parameter / payload byte count
[13..]   parameters / payload
[62]     checksum
[63]     0x00
```

Checksum (verified):

* requests: XOR of bytes `[0..62)` — i.e. *including* the report ID
* replies: XOR of bytes `[1..62)` — i.e. *excluding* the report ID

A reply is matched to its request by `cmd` and the low 5 bits of the sequence
byte. Every process that has the hidraw node open receives every reply, so
tools must serialize request/response pairs (this project uses an `flock` on
`$XDG_RUNTIME_DIR/synapse-linux/hidrawN.lock`) and ignore frames they did not
ask for.

SET replies carry `count = 1`, payload `0x00` (verified).

## Domains

On the dongle (`1532:0565`) headset registers use domain `0x80`; the dongle
relays them over the RF link. Domain `0x00` addresses the dongle itself (link
status, LED). With the headset plugged in by cable (`1532:056E`) everything
uses domain `0x00`.

When the headset is off, requests on `0x80` are simply not answered — check
the link status first to avoid waiting for time-outs.

## Registers

| GET | SET | Domain | Meaning | Values | Verified |
|---|---|---|---|---|---|
| `0x00` | – | 0x80 / 0x00 | serial number | ASCII (15 chars) | yes |
| `0x02` | – | 0x80 / 0x00 | firmware version | 4 bytes: major, minor, build, rev | yes |
| `0x03` | – | 0x80 / 0x00 | USB product id | 2 bytes | – |
| `0x13` | `0x93` | 0x80 | active EQ preset | `0x00` flat, `0x07` game, `0x08` music, `0x09` movie, `0xFF` custom | yes |
| `0x15` | `0x95` | 0x80 | **Custom slot** EQ curve | 10 × int8 dB, see below | yes |
| `0x18` | `0x98` | 0x80 | sidetone on/off | 0 / 1 | yes |
| `0x19` | `0x99` | 0x80 | sidetone level | 0–15 | yes |
| `0x1D` | `0x9D` | 0x80 | audio enhancement (bass/spatial expander) | 0 / 1 | read; write accepted |
| `0x1E` | `0x9E` | 0x80 | EQ stage enable | 0 / 1 | read; write accepted |
| `0x20` | – | 0x00 | RF link to the headset | 0 / 1 | yes |
| `0x21` | – | 0x80 | battery | 0–100 % | yes |
| `0x27` | `0xA7` | 0x80 | block Bluetooth calls during 2.4 GHz | 0 / 1 | yes |
| `0x2A` | – | 0x80 | charging | 0 / >0 | read |
| `0x2C` | `0xAC` | 0x80 | auto power-off | minutes, 0 = never | yes |
| `0x55` | – | 0x80 | microphone mute switch | 0 / 1 | read |
| `0x66` | `0xE6` | 0x00 | dongle LED | 0 off, 1 link, 2 battery, 3 warning | yes |

### Equalizer

* Bands: 31, 62, 125, 250, 500 Hz, 1, 2, 4, 8, 16 kHz; the DSP range is −9…+6 dB.
* Writing (`0x95`, 10 parameter bytes) stores `wire − 5`, so send
  `wire = dB + 5` as a signed byte. Reading `0x15` returns the stored dB
  value directly. (Verified: writing `+1 +2 +3 +4 +5 +6 −1 −2 −3 −9` sends
  `06 07 08 09 0a 0b 04 03 02 fc` and reads back `01 02 03 04 05 06 ff fe fd f7`.)
* **`0x15` always returns the Custom slot, whatever preset is active**
  (verified by selecting game/custom/flat/music in turn). The factory curves
  of the ROM presets cannot be read; the application shows an approximation.
* Sequence used to write a curve: `0x9E=1` (EQ on), `0x9D=<keep>`,
  `0x93=0xFF` (select custom), `0x95=<curve>`, ~40 ms pause, `0x93=0xFF` again
  so the DSP picks up the new values.
* Selecting a preset does not change the enhancement flag (verified).
* A headset that has only seen Synapse may report `−5 dB` on every band: a
  "flat" curve written as raw zeros.

## Timing

A request/response round trip through the dongle takes a few milliseconds;
reading every register (`synapsectl status`) takes about 0.6 s. The device
sends nothing on its own while idle, so state is polled.
