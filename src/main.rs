mod device;
mod protocol;
mod tui;

use crate::device::{ConnectionPreference, CorsairMouse};
use crate::protocol::{validate_debounce_ms, FLASH_ADDR_DEBOUNCE};
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "corsair-debounce-tool",
    version,
    about = "Cross-platform CLI & TUI utility to adjust Corsair Sabre v2 Pro button debounce timing",
    long_about = "Clean-room cross-platform (Windows, macOS, Linux) terminal utility exclusively for the \
                  Corsair Sabre v2 Pro (wired and 2.4 GHz wireless) to view device status and configure button debounce timing."
)]
struct Cli {
    /// Preferred connection transport (`auto`, `wired`, or `wireless`).
    #[arg(long, short = 'c', value_enum, default_value_t = ConnectionPreference::Auto, global = true)]
    connection: ConnectionPreference,

    /// Use an in-memory simulated mouse device instead of opening physical USB HID hardware.
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Launch the interactive terminal user interface (default when run with no subcommand).
    Tui {
        /// Unlock the extended 0..=255 ms debounce range (default range is 0..=20 ms).
        #[arg(long, short = 'f')]
        force: bool,
    },

    /// Display connection mode, online status, active profile, battery level, and current debounce time.
    #[command(visible_alias = "info")]
    Status,

    /// Read and print the current button debounce time in milliseconds.
    Get {
        /// Print only the numeric debounce value in milliseconds (for scripts).
        #[arg(long, short = 'q')]
        quiet: bool,
    },

    /// Set a new button debounce time in milliseconds (0..=20 ms by default, or 0..=255 ms with --force).
    Set {
        /// Target debounce time in milliseconds.
        debounce_ms: u16,

        /// Allow setting values above the standard 20 ms limit (up to 255 ms).
        #[arg(long, short = 'f')]
        force: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut mouse = if cli.dry_run {
        CorsairMouse::open_mock()
    } else {
        CorsairMouse::open(cli.connection)?
    };

    match cli.command {
        None => tui::run_tui(mouse, false),
        Some(Commands::Tui { force }) => tui::run_tui(mouse, force),
        Some(Commands::Status) => {
            let res = cmd_status(&mut mouse);
            let _ = mouse.set_pc_driver_status(false);
            res
        }
        Some(Commands::Get { quiet }) => {
            let res = cmd_get(&mut mouse, quiet);
            let _ = mouse.set_pc_driver_status(false);
            res
        }
        Some(Commands::Set { debounce_ms, force }) => {
            let res = cmd_set(&mut mouse, debounce_ms, force);
            let _ = mouse.set_pc_driver_status(false);
            res
        }
    }
}

fn cmd_status(mouse: &mut CorsairMouse) -> Result<()> {
    let status = mouse.query_full_status()?;

    println!("Device:          {}", status.product_name);
    println!("Transport:       {}", status.transport_description());
    println!("HID Path:        {}", status.device_path);
    println!(
        "Mouse Status:    {}",
        if status.online {
            "Online"
        } else {
            "Offline / Asleep"
        }
    );
    println!(
        "Wireless Addr:   {:02X}:{:02X}:{:02X}",
        status.wireless_address[0], status.wireless_address[1], status.wireless_address[2]
    );

    if let Some(profile) = status.active_profile {
        println!(
            "Active Profile:  Profile {} (slot {})",
            profile.saturating_add(1),
            profile
        );
    } else {
        println!("Active Profile:  Unavailable (mouse offline)");
    }

    if let Some(battery) = status.battery {
        let state = if battery.charging {
            "Charging"
        } else {
            "Discharging"
        };
        println!(
            "Battery:         {}% ({}, {} mV)",
            battery.level_percent, state, battery.voltage_mv
        );
    } else {
        println!("Battery:         Unavailable (mouse offline)");
    }

    if let Some(debounce) = status.debounce {
        if debounce.checksum_valid {
            println!(
                "Debounce Time:   {} ms (flash 0x{:04X} = 0x{:02X}, check 0x{:02X} OK)",
                debounce.debounce_ms,
                FLASH_ADDR_DEBOUNCE,
                debounce.raw_value,
                debounce.raw_check_byte
            );
        } else {
            println!(
                "Debounce Time:   {} ms (default; flash 0x{:04X} uninitialized: 0x{:02X} / 0x{:02X})",
                debounce.debounce_ms,
                FLASH_ADDR_DEBOUNCE,
                debounce.raw_value,
                debounce.raw_check_byte
            );
        }
    } else {
        println!("Debounce Time:   Unavailable (mouse offline)");
    }

    Ok(())
}

fn cmd_get(mouse: &mut CorsairMouse, quiet: bool) -> Result<()> {
    let online = mouse.query_online()?;
    if !online.online {
        bail!("Mouse is currently offline or asleep over the wireless receiver.");
    }

    let reading = mouse.read_debounce()?;
    if quiet {
        println!("{}", reading.debounce_ms);
    } else if reading.checksum_valid {
        println!("Current debounce time: {} ms", reading.debounce_ms);
    } else {
        println!(
            "Current debounce time: {} ms (factory default; flash uninitialized)",
            reading.debounce_ms
        );
    }
    Ok(())
}

fn cmd_set(mouse: &mut CorsairMouse, requested_ms: u16, force: bool) -> Result<()> {
    let validated_ms = validate_debounce_ms(requested_ms, force)?;
    let reading = mouse.write_debounce(validated_ms)?;
    println!(
        "Successfully updated debounce time to {} ms (flash 0x{:04X} = 0x{:02X}, check 0x{:02X}).",
        reading.debounce_ms, FLASH_ADDR_DEBOUNCE, reading.raw_value, reading.raw_check_byte
    );
    Ok(())
}
