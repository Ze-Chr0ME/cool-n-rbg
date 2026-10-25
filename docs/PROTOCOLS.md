# Device protocol notes

Sources: OpenLinkHub (`src/devices/cc/cc.go`, `slipstream/slipstream.go`, `k57rgbW/k57rgbW.go`),
OpenRGB (`CorsairCommanderCoreController`, `CorsairPeripheralV2Controller`), liquidctl `commander_core.py`.
Verified on this machine with `cargo run -p coolrgb-core --example probe`.

All HID writes are 65 bytes: `[0x00 report id, <prefix>, cmd..., data...]`. Reads are 64 bytes
(hidapi strips report id 0). Response: `[0]=00, [1]=echo of cmd[0], [2]=status (0 = ok), [3..]=payload`.

## Corsair iCUE Commander Core 1B1C:0C32 (interface 0, usage page FF42 usage 1)

Prefix `0x08`. Packet 64 (+1 report id). Firmware here: 2.0.19, pump version 6, radiator 360 mm.

| op | bytes |
|---|---|
| firmware | `02 13` -> `[3]=maj [4]=min [5..7]=patch LE` |
| software mode (wake) | `01 03 00 02` |
| hardware mode (sleep) | `01 03 00 01` (device runs its stored iCUE hardware profile) |
| close ep | `05 01 01 <ep>` |
| open ep (data) | `0d 01 <ep>` |
| read | `08 01` -> `[3..5]=data type, [5]=count, [6..]=entries` |
| write | `06 01` + `[len LE u16][00 00][datatype 2][data]`; len = data+2 |

Reads REQUIRE software mode first (status 03 otherwise).

Endpoints:
- `20` LED counts, type `0f 00`: 7 x `[status u16 (2=connected), leds u16]`. Here: pump 29, fans 8,8,8,16,16,40(?)
- `1a` fans connected, type `09 00`: 7 x status (`07` spinning, `02` connected no tach)
- `17` speeds, type `06 00`: 7 x rpm u16 (ch0 = pump)
- `21` temps, type `10 00`: n x `[status (0=ok), temp*10 i16]`; sensor 0 = liquid
- `18` set speed, type `07 00`: `[n] + n x [channel, mode(0=percent), value, 0]`

RGB: once per session `05 01 01 22` then `0d 00 22` (color endpoint, slot 0).
Write colors: first chunk cmd `06 00` + `[len LE = data+2][00 00][12 00][RGB...]`, following chunks cmd `07 00`;
chunk size 61 bytes of payload after the 2 cmd bytes. RGB bytes = contiguous LEDs, ports in order 0..6, no padding.
Re-send at least every ~10 s or the device times out of software mode.

## Corsair V2 / Bragi peripherals

Command interface: usage page FF42, usage 1. `GET 0x12` (= `02 12`) returns PID at `[3..5]`.
- Harpoon RGB Wireless 1B5E (wired): prefix `08` answers PID 1B5E. OpenRGB "software" controller.
- K57 RGB Wireless dongle 1B62: prefix `08` -> dongle itself (PID 1B62). Keyboard behind it is prefix `09`
  (single paired device). OLH K57 wireless: software mode `01 03 00 06`, colors written as hardware
  static mode `7e 20 01` with per-key index list.

SET = `01`, GET = `02`, mode register `03`: value `01` hardware, `02` software (OpenRGB SetRenderMode).
Start tx `0d <opt> <light_ctrl 0x01|0x22>`, block write `06 00 [len LE]` data at offset 8, more `07` data at offset 4, stop `05 01 <opt>`.

### Harpoon device-control properties (non-RGB)

GET `02 <id>` replies `[1]=02 [2]=status (0 ok) [3..]=value LE`; SET is `01 <id> 00 <value LE>`, accepted
only in software mode (hardware mode answers status 03).
Values read from this mouse by `examples/bragi_props.rs` (read-only scan, fw 5.0.129, daemon in
software mode), 2026-10-25. Names from OpenLinkHub `src/devices/harpoonWU/harpoonWU.go`
unless marked as a guess.

| id | value here | meaning |
|---|---|---|
| `01` | 4 | polling rate: 1=125 Hz, 2=250, 3=500, 4=1000; a change makes the mouse reconnect on USB (active stage resets to 0) |
| `02` | 1000 | LED brightness 0..1000 |
| `03` | 2 | mode: 1 hardware, 2 software |
| `07` | 0 | angle snapping 0/1 |
| `0e` | 900000 | sleep timeout ms (OpenLinkHub writes `0e` and `37` together, after `01 0d 00 01`) |
| `37` | 300000 | sleep timeout ms, second register |
| `0f` | 1000 | battery level x10 (%) |
| `10` | 3 | battery status (guess: 3 = full) |
| `13` | 05 00 81 00 | firmware 5.0.129 (`[0].[1].[2..4] LE`) |
| `18..1c` | 10000, 1000, 1400, 3000, 5000 | hardware DPI stage 0..4 (verified: hardware mode switches `20` to the active stage value) |
| `1e` | 1 | active hardware DPI stage; writable in software mode |
| `1f` | 1 | unknown, DPI-related (enabled stages?) |
| `20` | 1000 | current DPI, X and Y: 200..10000 in steps of 200, anything else answers status 01 |
| `2f..33` | blue, white, green, purple, orange | DPI stage 0..4 indicator colors, RGB (guess) |
| `b0` | 0 | button response optimization 0/1 |

Not on this mouse (GET fails): `21`/`22` (separate DPI X/Y on other Bragi mice), `1d`.
Unknown but readable: `04 09 0a 0b 0d 17 35 38 39 40 48 5f 6f..74 96`.

In software mode the firmware ignores the DPI button (verified); interface 2 (usage page FF42,
usage 2) sends `[1]=02 [2]=held button bits` on every press and release (left `01`, right `02`, middle `04`,
DPI `08`, side `10`/`20`) and the host steps the stage: write `1e`, then `20`.
Button remap: open `0d 01 02`, write `06 01 [len LE 2 bytes, 0, 0]` + one byte per
button (1 = firmware default, 0 = host handles it), close `05 01 01`.
Keep-alive: OpenLinkHub sends `12` every 20 s.

## EVision V2 keyboards: DesignedByGG Ironclad V3 320F:511E (interface 1, usage page FF1C usage 92)

Source: OpenRGB `EVisionV2KeyboardController` (lists the Ironclad V1, 320F:5041). The stock app
`ICL03.exe` hardcodes 320F/511E and talks to the same collection with `HidD_SetOutputReport`.

Every request and reply is one 64-byte report:
`[0]=04 (report id) [1..3]=checksum LE (sum of bytes 3..63) [3]=cmd [4]=size [5..7]=offset LE [7]=status [8..]=data (max 56)`.
The reply echoes bytes 1..7; `[7]` non-zero = error.

| cmd | use |
|---|---|
| `03` read capabilities, size 7 | `[0..2]=AA 55`, `[5]`=color slots (126 here = 21 columns x 6 rows) |
| `12` send dynamic colors | RGB per slot, written at byte offset `slot*3`, chunked by 56 |
| `13` end dynamic colors | keyboard returns to its stored effect |

| `01` / `02` begin / end configure | wrap every `06` write |
| `05` read config, `06` write config | stored settings, offset = config address |

Capabilities (cmd `03`, 56 bytes read on this keyboard): `[5]`=slots, `[21..23]`=VID LE, `[31..]`=supported
built-in effect codes, zero-terminated (`01..0f 11 10 12 14`).

Config memory: `0x00` = active profile (0..2). Profile `p` at `1 + p*0x40`: `+0` effect, `+1` brightness
0..4, `+2` speed 0 (fast)..5, `+3` direction (0 = counter-clockwise, matches the stock app), `+4` random
colors (non-zero), `+5..8` RGB; we rewrite the first 18 bytes and keep `+8..18`. `+0x15` USB polling
rate (0..3 = 1000/500/250/125 Hz), `+0x18` debounce (0..3 = 5/10/15/20 ms). Key map from `0xC0`,
3 bytes per key (`20 00 <HID usage>`), repeated per profile.

Polling and debounce were captured passively (`examples/evision_listen.rs`) while clicking each option in
ICL03 in a known order, 2026-10-25. ICL03 saves by rewriting config `0x00..0x46` inside begin/end
configure, and sometimes sends `13` (end dynamic colors), which drops our direct mode; the service
re-sends its whole frame every second to take it back.

Slot = `column*6 + row`; slots without a key are ignored. Direct colors revert unless refreshed about
every 200 ms: we re-send slot 0 (3 bytes) every 150 ms. Verified 2026-10-25: capabilities, color writes
and refresh all acknowledged.

## PawnIO (driver installed at C:\Program Files\PawnIO, modules in vendor\pawnio)

`PawnIOLib.dll`: `pawnio_open(&h)`, `pawnio_load(h, blob, len)`, `pawnio_execute(h, "ioctl_name", in u64[], n, out u64[], n, &ret)`.
One handle per module. Requires admin.

- SmbusPIIX4: `ioctl_smbus_xfer` in `[addr, rw (1 read / 0 write), cmd, proto, data...]`; proto 0 quick, 1 byte,
  2 byte data, 3 word, 5 block (block write: 4 + 5 cells, bytes packed LE starting with length). out `[len|data packed]`.
  `ioctl_piix4_port_sel` [port]. Hold mutex `Global\Access_SMBUS.HTP.Method`.
- LpcIO: `ioctl_select_slot` [0 => 0x2E, 1 => 0x4E], `ioctl_pio_outb` [port, val] (register port allowed),
  `ioctl_superio_inb` [reg], `ioctl_superio_outb` [reg, val]. Enter config: outb(0x2E,0x87) twice; exit outb(0x2E,0xAA).
  Hold mutex `Global\Access_ISABUS.HTP.Method`.
- AMDFamily17: `ioctl_read_smn` [addr]. Tctl at SMN 0x00059800: `((v >> 21) & 0x7FF) / 8`, minus 49 if bit 19 set.
  Hold mutex `Global\Access_PCI`.

## Corsair Vengeance RGB Pro DDR4 (SMBus, PIIX4 port 0, addresses 0x58-0x5F)

Detect: read byte 0x43 in {1A,1B,1C} and 0x44 in {01,03,04}. NEVER touch 0x50-0x57 (SPD EEPROM).
Device info: write byte 0x61=0, 0x21=0, then 32 x read byte 0x40, CRC8 at 0x42. PID at [2..4] (0x0100/0x0101), protocol at [28].
Direct mode (protocol >= 4, volatile): block write reg 0x31 = `[10 leds, R,G,B x10, crc8]` (32 bytes). 10 LEDs.

## MSI B450 TOMAHAWK (7C02) onboard RGB: Nuvoton NCT6795/6797 Super-IO, invert = true

Enter config, `07 = 09`, reg 0x2C: clear bit3, set bit4. `07 = 12` (RGB logical device). E0 |= 0xE0 enable.
E4 = 00, FF = 0xE2|0x1C (invert), FD = 00. Color: 4 bits per channel, F0-F3 red, F4-F7 green, F8-FB blue,
each byte = (c | c<<4) where c = value >> 4. With invert boards the value written is inverted by the chip.

### Verified behaviour (2026-10-25)
- 125 LEDs: ports [29, 8, 8, 8, 16, 16, 40]; no LED port re-config needed, colors written as-is. ~22 fps max.
- Set speed works: fans 60% -> ~1450 RPM (8-LED fans) / ~1070 (16-LED fans); pump 80% -> ~2400 RPM.
- Crash safety: after the last command the hub stays in software mode briefly, then by 75 s it is back on
  its stored hardware profile by itself (fans ~620 RPM, pump ~2320). Daemon must keep-alive every few seconds.

### LED port table (required for all ports to light)
After software mode send `14 <port> 01` for ports 0..6, wait 500 ms, then write endpoint `1e` (no data type)
with `0d 00 07` + per port `01 <code>` or `00` (codes: 4 LEDs 03, 8 LEDs 05, strip 01, HD 04, LL 02, pump 08,
QL 06). Without it the hub only drove the AIO ports; the 3 LL120s on ports 4-6 stayed dark.
The hub's LED-count detection is unreliable: port 6 (an LL120) read 40, 21, "status 03", 35 across reads.
Counts that match no Corsair device are driven as LL (16); the GUI can force a type per port (`led_ports`).
