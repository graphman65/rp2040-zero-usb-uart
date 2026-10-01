# RP2040-Zero USB UART Bridge

USB-UART Firmware for a [Waveshare RP2040-Zero](https://www.waveshare.com/wiki/RP2040-Zero). Connects a 3.3 V TTL UART device to a computer over USB CDC-ACM. Data passes unchanged in both directions.

## Why?
I had a few spare RP2040-Zero boards and needed to test a UART GPS module from my PC. Rather than wait a week for a USB-to-UART adapter to arrive, I used one of the boards I already had.

## Wiring

| RP2040-Zero | UART device |
| --- | --- |
| GPIO0 — TX | RX |
| GPIO1 — RX | TX |
| GND | GND |

### Power

With the board powered over USB, select a supply that matches the device:

| Device supply | RP2040-Zero pin |
| --- | --- |
| 5 V | **5V** — USB supply voltage |
| 3.3 V | **3V3** — regulated 3.3 V output |

Connect **one** supply to the device's VCC, never both. Check the device specification; do not choose the supply based on its UART signal voltage.

For a separately powered device, connect only TX, RX and GND. Do not tie its supply to the board's power pins while USB is connected: the 5V pin is directly connected to USB power and can backfeed the computer. 
The firmware declares 150 mA total USB current, including the board; use a separate supply for larger loads.

**UART signals must be 3.3 V.** Powering a device from 5 V does not make its UART signals safe for the RP2040. Use level shifting for 5 V UART signals, or a transceiver for RS-232/RS-485. Disconnect USB and any external supply before changing wiring.

## Build

Requires Rust and the `thumbv6m-none-eabi` target. Install the pinned UF2 converter once per checkout. These commands use an x86-64 Linux host:

```sh
rustup target add thumbv6m-none-eabi
cargo install elf2uf2-rs --version 2.2.0 --locked --no-default-features \
  --target x86_64-unknown-linux-gnu --root target/tools
cargo build --release --locked
target/tools/bin/elf2uf2-rs \
  target/thumbv6m-none-eabi/release/rp2040-zero-usb-uart \
  target/thumbv6m-none-eabi/release/rp2040-zero-usb-uart.uf2
```

The converter lives under `target/tools`, not in the global Cargo bin directory. The explicit host target prevents Cargo from building it for the microcontroller. `cargo clean` removes the local tool; reinstall it afterward.

For traffic LED pulses, add `--features activity-led` to the Cargo command. Startup blinks are enabled in both builds.

## Flash

1. Alternatively, hold BOOT while connecting USB.
2. Copy the generated UF2 to the mounted `RPI-RP2` drive.
3. The board restarts and appears as a USB serial port.

## Usage

On Linux, find the port under `/dev/serial/by-id/` or `/dev/ttyACM*`. The USB product name is `RP2040 USB UART Bridge`; each board has a serial number derived from its flash ID.

Configure your serial application to match the attached device and **assert DTR**. Disable hardware flow control.

| Setting | Supported values |
| --- | --- |
| Baud rate | 1,200–921,600 |
| Data bits | 5–8 |
| Parity | None, odd, even |
| Stop bits | 1 or 2 |
| Boot default | 38,400 baud, 8N1 |

Host serial settings configure the bridge UART, not the attached device. Unsupported settings are rejected. Requested settings persist across USB reconnects; power-on restores the boot default.

DTR enables forwarding. With DTR low or USB disconnected, incoming UART data is discarded. DTR and RTS do not reset the board or control physical pins. RTS/CTS flow control, serial break, and USB host mode are not supported.

The receive buffer is 4 KiB and the transmit buffer is 1 KiB. A stalled host reader can cause data loss; **there is no hardware flow control.**

The USB VID/PID is `c0de:cafe`, an example identity for local development.

## LED

The onboard RGB LED uses GPIO16, PIO0 state machine 0, and DMA channel 0.

- **Startup:** two green blinks, then off.
- **With `activity-led`:** blue pulses during forwarding. Sustained traffic blinks at roughly 8 Hz.

LED updates run independently of serial transfers.

## Checks

Run the host-side Rust tests:

```sh
cargo test --locked --no-default-features --target x86_64-unknown-linux-gnu
```
