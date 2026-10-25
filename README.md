# Cool'n RGB

A lightweight replacement for SignalRGB/iCUE on this PC: RGB lighting and fan/pump control, written in Rust.

| | Memory (private) | CPU |
|---|---|---|
| Background service `coolrgbd.exe`, static color | **~5.8 MB** | ~0 % |
| Background service, animated effect | ~5.8 MB | USB devices ~0 %; RAM sticks ~7 % of one core (estimate, see below) |
| Control panel `coolrgb.exe` (only while open) | ~110 MB | — |
| SignalRGB (for comparison) | 410–840 MB | — |

SMBus writes to the RAM busy-wait inside the PawnIO driver. At 20 fps they measured 13.5 % of one core,
so RAM animates at 10 fps and skips frames that didn't change. Static colors cost nothing.

The service has no GUI toolkit: it is a tray icon, a few worker threads and a named pipe. The control
panel is a separate program that you open from the tray. Closing it gives all of its memory back.

## Supported hardware (detected on this PC)

| Device | Lighting | Other |
|---|---|---|
| Corsair iCUE Commander Core (AIO pump + 6 fans, 125 LEDs) | yes, one effect per LED port or for the whole hub | pump/fan speed, RPM, liquid temperature |
| Corsair Vengeance RGB Pro DDR4 x2 (SMBus 0x58, 0x5A) | yes | — |
| MSI B450 TOMAHAWK onboard RGB (MSI-RGB Super-IO) | yes (single zone, 4-bit color) | — |
| Corsair Harpoon RGB Wireless (USB cable) | yes | — |
| DesignedByGG Ironclad V3 (EVision 320F:511E) | yes (effects sweep across key columns) | — |
| Corsair K57 RGB Wireless (Slipstream dongle) | no (retired, not planned) | — |
| CPU (Ryzen Tctl), NVIDIA GPU | — | temperatures for fan curves |

The protocols come from OpenRGB, OpenLinkHub and liquidctl; see [docs/PROTOCOLS.md](docs/PROTOCOLS.md).

## Install

1. Quit SignalRGB completely and stop its service, or the two programs will fight over the devices.
   SignalRGB starts `SignalRgb.Service` at boot; set it to *Manual* in `services.msc` or uninstall SignalRGB.
2. [PawnIO](https://pawnio.eu) must be installed (needed for RAM, motherboard and CPU temperature).
3. Build and install from an **administrator** PowerShell:

   ```powershell
   powershell -ExecutionPolicy Bypass -File scripts\build.ps1
   powershell -ExecutionPolicy Bypass -File scripts\install.ps1
   ```

4. Open **Cool'n RGB** from the Start menu (or click the rainbow ring in the tray). Under **Settings**, tick
   **Start with Windows**. This creates a Task Scheduler task that starts the service with admin rights at
   logon, without a UAC prompt.

To remove it, run `scripts\uninstall.ps1` (add `-KeepSettings` to keep `C:\ProgramData\CoolRGB`).

## Using it

- **Lights on/off**: the switch in the header, or **Lights off** in the tray menu, turns every LED off
  without losing your effects.
- **Lighting**: brightness, plus one effect for all devices (Off, Static, Breathing, Rainbow, Wave).
  **Apply to** lets any device use its own effect.
- **Fans → Presets**: Quiet, Balanced and Performance set the pump, case fans and GPU in one click.
  Quiet stops the GPU fans below 55 °C.
- **Fans**: a pump curve and a fans curve (all 6 fans), or fixed speeds. The temperature source can be
  liquid, CPU, GPU or the hottest of the three. Drag the points to change a curve. The orange line is the
  current temperature, and the red band is the area below the minimum speed.
- **Devices**: what was detected, plus a **Rescan** button. The service also rescans by itself after sleep
  and when USB devices are unplugged and plugged back in.

## Fan safety

- The service never sends less than **40 %** to the fans or **70 %** to the pump (`config.rs`).
- Everything goes to **100 %** above 50 °C liquid or 95 °C CPU, and whenever a curve's temperature sensor
  is unavailable.
- Speeds rise immediately and fall by at most 3 % per second.
- When the service exits, the Commander Core goes back to the profile stored in its own memory (the one
  iCUE saved). If the service crashes or is killed, the hub does this by itself within about a minute.
  This was tested.

## Files

- Settings: `C:\ProgramData\CoolRGB\config.json`. The GUI edits it through the service, and changes apply live.
- Log: `C:\ProgramData\CoolRGB\daemon.log` (capped at 256 KB).

## Layout

```
crates/core    devices (Commander Core, Corsair V2, DRAM, MSI-RGB), effects, config, IPC, PawnIO binding
crates/daemon  coolrgbd.exe: tray icon, device threads, fan control, named pipe server
crates/gui     coolrgb.exe: egui control panel
vendor/pawnio  signed PawnIO modules (SmbusPIIX4, LpcIO, AMDFamily17), embedded in the binaries
```

Developer examples, run with `cargo run --release -p coolrgb-core --example <name>`:

- `probe`: read-only USB dump.
- `pawn_test`: temperatures and DRAM/Super-IO detection.
- `ctl`: pipe client. `ctl status`, `ctl config`, `ctl set file.json`, `ctl quit`, and `ctl handover`
  (quits without switching the lights back to their hardware profiles).
