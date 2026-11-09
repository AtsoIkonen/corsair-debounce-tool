# AGENTS.md

## Project Overview

`corsair-debounce-tool` is a clean-room, cross-platform (Windows, macOS, Linux) Rust CLI and interactive TUI application exclusively for the **Corsair Sabre v2 Pro** gaming mouse. It communicates directly with the mouse (wired USB or 2.4 GHz wireless receiver) over USB HID to read device status and configure button debounce timing.

---

## Commands

- **Build (debug)**: `cargo build`
- **Build (release)**: `cargo build --release`
- **Run unit tests**: `cargo test`
- **Lint / Check**: `cargo clippy --all-targets -- -D warnings`
- **Format check**: `cargo fmt -- --check`
- **Run without hardware (simulated device)**:
  - Interactive TUI: `cargo run -- --dry-run`
  - Status output: `cargo run -- --dry-run status`
  - Read debounce: `cargo run -- --dry-run get`
  - Write debounce: `cargo run -- --dry-run set 4`

---

## Architecture & Module Map

- `src/main.rs` — CLI entrypoint (`clap` derive) dispatching `tui` (default), `status`, `get`, and `set` subcommands, and ensuring `PCDriverStatus(false)` is sent before exit.
- `src/protocol.rs` — Pure, hardware-independent packet builders, parsers, checksum functions, and unit tests for the 17-byte USB HID protocol.
- `src/device.rs` — Cross-platform `hidapi` device discovery, endpoint ranking, retry/timeout transaction loop, and the `--dry-run` mock backend.
- `src/tui.rs` — Interactive terminal interface built with `ratatui` and `crossterm`.

---

## Hardware & Protocol Invariants (Critical)

When modifying `src/protocol.rs` or `src/device.rs`, preserve these hardware invariants for the **Corsair Sabre v2 Pro**:

1. **Exclusive Target Hardware**:
   - Vendor ID (`VID`): `0x1B1C`
   - Wired Mouse Product ID (`PID`): `0x2B28`
   - 2.4 GHz Wireless Receiver Product ID (`PID`): `0x2B2A`
   - Control Channel: USB Interface `1` (`mi_01`), HID Collection `5` (`col05`), **Report ID `0x08`**.
   - Never send vendor control packets to Interface `0` (boot mouse interface) or other USB devices.

2. **17-Byte Report Format & Checksum**:
   - Every request and response is exactly **17 bytes** (`0x11` bytes):
     - `buf[0] = 0x08` (Report ID)
     - `buf[1] = command_id`
     - `buf[2] = status` (`0x00`)
     - `buf[3..=4] = address.to_be_bytes()` (16-bit big-endian flash offset)
     - `buf[5] = payload_len` (`0..=10`)
     - `buf[6..16] = payload` (zero-padded)
     - `buf[16] = 0x55u8.wrapping_sub(sum(buf[0..16]))`
   - Note that `buf[0] = 0x08` **is included** in the packet checksum calculation.

3. **Flash Memory Safety (`0x00A9..=0x00AA`)**:
   - Button debounce time (`keyDebounceTime`) resides at flash address `0x00A9` (1 byte, in ms) and **must** always be written together with its paired validation byte at `0x00AA` (`0x55u8.wrapping_sub(debounce_ms)`), using `WriteFlashData (0x07)` with `address = 0x00A9` and `length = 2`.
   - Never write a flash parameter without its corresponding `0x55 - val` check byte; an invalid check byte causes the mouse firmware/driver to reject the stored value and reset to factory defaults (`8 ms`).
   - Always verify that the mouse is online (`DeviceOnLine (0x03)` -> `payload[0] != 0`) before issuing flash writes over the wireless receiver, and read back `0x00A9` after writing to confirm persistence.

4. **Asynchronous Device Reports**:
   - While connected, pressing physical DPI or profile buttons on the mouse emits spontaneous `ReportDataToPc (0x0A)` input reports. The transaction loop in `src/device.rs` must continue to skip `0x0A` packets when awaiting a command response.

---

## Coding Conventions & Best Practices

- **Cross-Platform Compatibility**: Avoid OS-specific Win32, `ioctls`, or platform-only crates. Use `hidapi` for USB HID and `crossterm`/`ratatui` for terminal handling so Windows, macOS, and Linux builds work out of the box.
- **Error Handling**: Use `anyhow::Result` with descriptive `.context(...)` messages; never `unwrap()` on I/O, USB reads/writes, or user input.
- **Testing**: Any change to packet encoding, flash offsets, or checksum logic in `src/protocol.rs` must be accompanied by unit tests in `protocol::tests` and verified via `--dry-run`.
