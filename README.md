# corsair-debounce-tool

A cross-platform (**Windows**, **macOS**, and **Linux**) terminal utility in Rust for viewing device status and adjusting the button debounce timing of the **Corsair Sabre v2 Pro**.

> **Note:** This tool is built exclusively for the **Corsair Sabre v2 Pro** (wired and 2.4 GHz wireless modes). Do not use it with other mouse models.

---

## Features

- **Interactive Terminal UI (`tui`)**: Adjust debounce timing with arrow keys or quick presets, and view connection status, active profile, and battery level.
- **CLI Subcommands (`status`, `get`, `set`)**: Quickly inspect or set your debounce time from the command line or scripts.
- **Wired & Wireless Support**: Works seamlessly whether the Sabre v2 Pro is plugged in via USB cable or connected through its 2.4 GHz wireless receiver.
- **Safe Defaults**: Enforces the standard `0–20 ms` range by default, with an optional `--force` flag if you want to experiment beyond `20 ms`.

---

## Installation & Building

Install Rust via [rustup.rs](https://rustup.rs/), then build the release binary:

```sh
cargo build --release
```

### Linux Permissions (`udev`)

On Linux, allow non-root access to the Sabre v2 Pro and its wireless receiver by creating `/etc/udev/rules.d/99-corsair-sabre-v2-pro.rules`:

```udev
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1b1c", ATTRS{idProduct}=="2b2a", MODE="0666"
SUBSYSTEM=="hidraw", ATTRS{idVendor}=="1b1c", ATTRS{idProduct}=="2b28", MODE="0666"
```

Then reload `udev`:

```sh
sudo udevadm control --reload-rules && sudo udevadm trigger
```

---

## Usage

### Interactive TUI (Default)

Run with no arguments to open the interactive terminal interface:

```sh
corsair-debounce-tool
```

| Key | Action |
| :--- | :--- |
| `Left` / `Right` or `Down` / `Up` or `-` / `+` | Decrease / increase debounce time by `1 ms` (auto-saves) |
| `PageDown` / `PageUp` or `[` / `]` | Decrease / increase debounce time by `5 ms` (auto-saves) |
| `0`–`9` or `t` | Quick presets (`0–9 ms`, or `10 ms` with `t`) |
| `Enter` or `Space` | Apply and save immediately |
| `r` | Refresh device and battery status |
| `f` | Toggle extended range (`0–20 ms` vs `0–255 ms`) |
| `q` or `Esc` | Quit |

### CLI Commands

```sh
# View connection status, battery level, active profile, and current debounce time
corsair-debounce-tool status

# Print current debounce time
corsair-debounce-tool get

# Set debounce time to 2 ms
corsair-debounce-tool set 2

# Preview the UI without the mouse plugged in
corsair-debounce-tool --dry-run
```

---

## Credits

Made with **Antigravity**, **Gemini 4 Argon**, and [REA (`reverse-engineer-anything`)](https://github.com/morluto/rea).

## License

MIT — see [LICENSE](LICENSE).
