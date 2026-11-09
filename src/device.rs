use crate::protocol::{
    parse_battery_response, parse_current_config_response, parse_debounce_response,
    parse_online_response, verify_field_checksum, verify_packet_checksum, verify_write_response,
    BatteryStatus, CommandId, DebounceReading, HidPacket, OnlineStatus, DEFAULT_DEBOUNCE_MS,
    FLASH_ADDR_DEBOUNCE, PACKET_LEN, PID_WIRED_USB, PID_WIRELESS_RECEIVER, REPORT_ID,
    TARGET_COLLECTION_SUBSTR, TARGET_INTERFACE, VENDOR_ID,
};
use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::fmt;
use std::time::{Duration, Instant};

/// Read timeout for a single 17-byte response attempt.
const READ_TIMEOUT_MS: i32 = 300;

/// Maximum number of command transmit/receive attempts before failing.
const MAX_RETRIES: usize = 3;

/// Connection mode preference when searching for the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConnectionPreference {
    /// Automatically prefer wired USB if connected, otherwise use the wireless receiver.
    Auto,
    /// Only connect via wired USB cable (`PID 0x2B28`).
    Wired,
    /// Only connect via 2.4 GHz wireless receiver (`PID 0x2B2A`).
    Wireless,
}

/// Active connection type for the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionKind {
    /// Connected directly via USB cable (`PID 0x2B28`).
    Wired,
    /// Connected wirelessly via the 2.4 GHz USB receiver (`PID 0x2B2A`).
    Wireless,
}

impl ConnectionKind {
    pub fn from_pid(pid: u16) -> Option<Self> {
        match pid {
            PID_WIRED_USB => Some(Self::Wired),
            PID_WIRELESS_RECEIVER => Some(Self::Wireless),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn pid(self) -> u16 {
        match self {
            Self::Wired => PID_WIRED_USB,
            Self::Wireless => PID_WIRELESS_RECEIVER,
        }
    }

    /// Short conversational phrase for status messages (e.g. "via USB" or "wirelessly").
    pub fn status_phrase(self) -> &'static str {
        match self {
            Self::Wired => "via USB",
            Self::Wireless => "wirelessly",
        }
    }
}

impl fmt::Display for ConnectionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wired => write!(f, "Wired (USB)"),
            Self::Wireless => write!(f, "Wireless (2.4 GHz Receiver)"),
        }
    }
}

/// Complete status snapshot of the mouse.
#[derive(Debug, Clone)]
pub struct MouseStatus {
    pub product_name: String,
    pub connection_kind: ConnectionKind,
    pub device_path: String,
    pub online: bool,
    pub wireless_address: [u8; 3],
    pub active_profile: Option<u8>,
    pub battery: Option<BatteryStatus>,
    pub debounce: Option<DebounceReading>,
}

impl MouseStatus {
    pub fn transport_description(&self) -> String {
        self.connection_kind.to_string()
    }
}

enum Transport {
    Hardware {
        device: HidDevice,
        kind: ConnectionKind,
        product_name: String,
        device_path: String,
    },
    Mock {
        debounce_ms: u8,
        active_profile: u8,
    },
}

/// High-level handle to the Corsair Sabre v2 Pro control interface.
pub struct CorsairMouse {
    transport: Transport,
    /// Recent TX/RX packet trace entries for UI/diagnostic display.
    pub trace_log: Vec<String>,
}

impl CorsairMouse {
    /// Creates an in-memory mock device for `--dry-run` testing without hardware attached.
    pub fn open_mock() -> Self {
        let mut mouse = Self {
            transport: Transport::Mock {
                debounce_ms: DEFAULT_DEBOUNCE_MS,
                active_profile: 0,
            },
            trace_log: Vec::new(),
        };
        mouse.push_trace(
            "Initialized dry-run simulated Corsair Sabre v2 Pro (VID 0x1B1C, PID 0x2B2A)",
        );
        mouse
    }

    /// Discovers and opens the Corsair Sabre v2 Pro or its wireless receiver across Windows, macOS, and Linux.
    pub fn open(preference: ConnectionPreference) -> Result<Self> {
        let api = HidApi::new().context("Failed to initialize cross-platform HID API")?;

        let mut candidates: Vec<&DeviceInfo> = api
            .device_list()
            .filter(|info| {
                if info.vendor_id() != VENDOR_ID {
                    return false;
                }
                match (preference, ConnectionKind::from_pid(info.product_id())) {
                    (ConnectionPreference::Auto, Some(_)) => true,
                    (ConnectionPreference::Wired, Some(ConnectionKind::Wired)) => true,
                    (ConnectionPreference::Wireless, Some(ConnectionKind::Wireless)) => true,
                    _ => false,
                }
            })
            .collect();

        if candidates.is_empty() {
            bail!(
                "No Corsair Sabre v2 Pro or wireless receiver found (expected VID 0x{:04X}, PID 0x{:04X} or 0x{:04X}).\n\
                 Ensure your mouse or 2.4 GHz receiver is plugged in.",
                VENDOR_ID,
                PID_WIRED_USB,
                PID_WIRELESS_RECEIVER
            );
        }

        candidates.sort_by_key(|info| candidate_priority_key(info));

        let mut last_open_error: Option<anyhow::Error> = None;
        let mut offline_dongle: Option<Self> = None;

        for info in candidates {
            // Skip standard mouse boot collections on Interface 0 when Interface 1 is known.
            let iface = info.interface_number();
            if iface == 0 {
                continue;
            }

            let kind = ConnectionKind::from_pid(info.product_id())
                .expect("filtered to supported PIDs");
            let path_str = info.path().to_string_lossy().into_owned();
            let product_name = info
                .product_string()
                .map(str::to_owned)
                .unwrap_or_else(|| "CORSAIR SABRE v2 PRO".to_string());

            match info.open_device(&api) {
                Ok(device) => {
                    let mut candidate_mouse = Self {
                        transport: Transport::Hardware {
                            device,
                            kind,
                            product_name,
                            device_path: path_str,
                        },
                        trace_log: Vec::new(),
                    };

                    // Probe the candidate collection with `DeviceOnLine (0x03)` to confirm it handles Report ID 0x08.
                    if let Ok(online_status) = candidate_mouse.query_online() {
                        let _ = candidate_mouse.set_pc_driver_status(true);
                        if online_status.online {
                            return Ok(candidate_mouse);
                        } else if offline_dongle.is_none() {
                            offline_dongle = Some(candidate_mouse);
                        }
                    }
                }
                Err(err) => {
                    last_open_error = Some(anyhow!(
                        "Failed to open HID path {} (VID 0x{:04X}, PID 0x{:04X}): {}",
                        path_str,
                        info.vendor_id(),
                        info.product_id(),
                        err
                    ));
                }
            }
        }

        if let Some(dongle) = offline_dongle {
            return Ok(dongle);
        }

        if let Some(err) = last_open_error {
            return Err(err.context(platform_permission_hint()));
        }

        bail!(
            "Found Corsair USB device (VID 0x{:04X}), but no HID collection responded to control Report ID 0x{:02X}.\n{}",
            VENDOR_ID,
            REPORT_ID,
            platform_permission_hint()
        )
    }

    pub fn connection_kind(&self) -> ConnectionKind {
        match &self.transport {
            Transport::Hardware { kind, .. } => *kind,
            Transport::Mock { .. } => ConnectionKind::Wired,
        }
    }

    pub fn product_name(&self) -> &str {
        match &self.transport {
            Transport::Hardware { product_name, .. } => product_name,
            Transport::Mock { .. } => "CORSAIR SABRE v2 PRO (Dry-Run Simulation)",
        }
    }

    pub fn device_path(&self) -> &str {
        match &self.transport {
            Transport::Hardware { device_path, .. } => device_path,
            Transport::Mock { .. } => "mock://vid_1b1c&pid_2b2a&mi_01&col05",
        }
    }

    /// Notifies the mouse/receiver that the host software is active or exiting (`PCDriverStatus = 0x02`).
    pub fn set_pc_driver_status(&mut self, active: bool) -> Result<()> {
        let req = HidPacket::pc_driver_status(active);
        let _ = self.transact(&req)?;
        Ok(())
    }

    /// Queries whether the mouse is currently online and awake (`DeviceOnLine = 0x03`).
    pub fn query_online(&mut self) -> Result<OnlineStatus> {
        let req = HidPacket::device_online();
        let rsp = self.transact(&req)?;
        parse_online_response(&rsp)
    }

    /// Queries battery percentage, charging state, and voltage (`BatteryLevel = 0x04`).
    pub fn query_battery(&mut self) -> Result<BatteryStatus> {
        let req = HidPacket::battery_level();
        let rsp = self.transact(&req)?;
        parse_battery_response(&rsp)
    }

    /// Queries the active onboard profile index (`0..=4`, corresponding to Profile 1..=5).
    pub fn query_active_profile(&mut self) -> Result<u8> {
        let req = HidPacket::get_current_config();
        let rsp = self.transact(&req)?;
        parse_current_config_response(&rsp)
    }

    /// Reads the current button debounce time (`0x00A9..=0x00AA`) from mouse flash memory.
    pub fn read_debounce(&mut self) -> Result<DebounceReading> {
        let req = HidPacket::read_debounce();
        let rsp = self.transact(&req)?;
        parse_debounce_response(&rsp)
    }

    /// Writes a new button debounce time (`ms` in milliseconds) to flash address `0x00A9`
    /// along with its check byte (`0x55 - ms`) at `0x00AA`, and verifies persistence by reading back.
    pub fn write_debounce(&mut self, debounce_ms: u8) -> Result<DebounceReading> {
        let online = self.query_online()?;
        if !online.online {
            bail!(
                "Mouse is currently offline or asleep over the wireless receiver. Wake the mouse and try again."
            );
        }

        let req = HidPacket::write_debounce(debounce_ms);
        let rsp = self.transact(&req)?;
        verify_write_response(&req, &rsp)?;

        let readback = self.read_debounce()?;
        if readback.debounce_ms != debounce_ms {
            bail!(
                "Flash readback mismatch after writing {} ms: device reported {} ms",
                debounce_ms,
                readback.debounce_ms
            );
        }
        Ok(readback)
    }

    /// Queries a complete snapshot of the device's status and debounce configuration.
    pub fn query_full_status(&mut self) -> Result<MouseStatus> {
        let online_status = self.query_online()?;
        let (active_profile, battery, debounce) = if online_status.online {
            (
                self.query_active_profile().ok(),
                self.query_battery().ok(),
                self.read_debounce().ok(),
            )
        } else {
            (None, None, None)
        };

        Ok(MouseStatus {
            product_name: self.product_name().to_string(),
            connection_kind: self.connection_kind(),
            device_path: self.device_path().to_string(),
            online: online_status.online,
            wireless_address: online_status.wireless_address,
            active_profile,
            battery,
            debounce,
        })
    }

    fn push_trace(&mut self, entry: impl Into<String>) {
        const MAX_TRACE_ENTRIES: usize = 50;
        if self.trace_log.len() >= MAX_TRACE_ENTRIES {
            self.trace_log.remove(0);
        }
        self.trace_log.push(entry.into());
    }

    /// Sends a 17-byte `HidPacket` and waits for the matching 17-byte response,
    /// skipping any asynchronous `ReportDataToPc (0x0A)` notifications emitted by the mouse.
    pub fn transact(&mut self, req: &HidPacket) -> Result<HidPacket> {
        match &mut self.transport {
            Transport::Mock {
                debounce_ms,
                active_profile,
            } => {
                let rsp = simulate_mock_response(req, debounce_ms, active_profile)?;
                self.push_trace(format!("TX [{}] -> RX [{}]", req, rsp));
                Ok(rsp)
            }
            Transport::Hardware { device, .. } => {
                let mut last_err = anyhow!("HID transaction timed out");

                for attempt in 1..=MAX_RETRIES {
                    // Drain any stale pending input reports before transmitting.
                    let mut drain_buf = [0u8; 64];
                    while let Ok(n) = device.read_timeout(&mut drain_buf, 0) {
                        if n == 0 {
                            break;
                        }
                    }

                    let written = device
                        .write(req.as_bytes())
                        .context("Failed to write 17-byte HID output report")?;
                    if written < PACKET_LEN {
                        last_err = anyhow!(
                            "Short HID write: wrote {} of {} bytes",
                            written,
                            PACKET_LEN
                        );
                        continue;
                    }

                    let deadline = Instant::now() + Duration::from_millis(READ_TIMEOUT_MS as u64);
                    while Instant::now() < deadline {
                        let remaining_ms = deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis()
                            .max(1) as i32;

                        let mut rx_buf = [0u8; 64];
                        let n = device
                            .read_timeout(&mut rx_buf, remaining_ms)
                            .context("Failed to read HID input report")?;
                        if n == 0 {
                            break;
                        }
                        if n < PACKET_LEN {
                            continue;
                        }

                        let mut pkt_bytes = [0u8; PACKET_LEN];
                        pkt_bytes.copy_from_slice(&rx_buf[0..PACKET_LEN]);
                        if pkt_bytes[0] != REPORT_ID {
                            continue;
                        }

                        // Skip spontaneous button/DPI/profile notifications (CommandId::ReportDataToPc = 0x0A).
                        if pkt_bytes[1] == CommandId::ReportDataToPc as u8 {
                            continue;
                        }

                        if pkt_bytes[1] == req.command_byte() {
                            let rsp = HidPacket(pkt_bytes);
                            self.push_trace(format!(
                                "TX [{}] -> RX [{}] (try {})",
                                req, rsp, attempt
                            ));
                            return Ok(rsp);
                        }
                    }

                    last_err = anyhow!(
                        "Timed out waiting for response to command 0x{:02X} (attempt {}/{})",
                        req.command_byte(),
                        attempt,
                        MAX_RETRIES
                    );
                }

                self.push_trace(format!("TX [{}] -> ERR ({})", req, last_err));
                Err(last_err)
            }
        }
    }
}

fn candidate_priority_key(info: &DeviceInfo) -> (u8, u8, u8) {
    // Prefer wired USB (0x2B28) over wireless receiver (0x2B2A) when both are connected.
    let pid_rank = if info.product_id() == PID_WIRED_USB {
        0
    } else {
        1
    };

    let path_lower = info.path().to_string_lossy().to_ascii_lowercase();
    let path_rank = if path_lower.contains(TARGET_COLLECTION_SUBSTR) && path_lower.contains("mi_01")
    {
        0
    } else if path_lower.contains(TARGET_COLLECTION_SUBSTR) {
        1
    } else {
        2
    };

    let iface = info.interface_number();
    let usage_page = info.usage_page();
    let iface_rank = if iface == TARGET_INTERFACE && usage_page >= 0xFF00 {
        0
    } else if iface == TARGET_INTERFACE {
        1
    } else if usage_page >= 0xFF00 {
        2
    } else {
        3
    };

    (pid_rank, path_rank, iface_rank)
}

fn platform_permission_hint() -> &'static str {
    if cfg!(target_os = "linux") {
        "On Linux, ensure you have read/write permissions for /dev/hidraw* or install a udev rule:\n  \
         SUBSYSTEM==\"hidraw\", ATTRS{idVendor}==\"1b1c\", ATTRS{idProduct}==\"2b2a\", MODE=\"0666\"\n  \
         SUBSYSTEM==\"hidraw\", ATTRS{idVendor}==\"1b1c\", ATTRS{idProduct}==\"2b28\", MODE=\"0666\""
    } else if cfg!(target_os = "macos") {
        "On macOS, ensure Input Monitoring / USB access permissions are granted to your terminal if prompted."
    } else {
        "Ensure CORSAIR Test Driver (`Mouse Drive Beta.exe`) or iCUE is closed if it has exclusive access to the device."
    }
}

fn simulate_mock_response(
    req: &HidPacket,
    debounce_ms: &mut u8,
    active_profile: &mut u8,
) -> Result<HidPacket> {
    let _ = verify_packet_checksum(req.as_bytes());
    let cmd = req.command_byte();

    if cmd == CommandId::PcDriverStatus as u8 {
        return Ok(*req);
    }
    if cmd == CommandId::DeviceOnLine as u8 {
        return HidPacket::new(CommandId::DeviceOnLine, 0x0000, &[0x01, 0x12, 0x34, 0x56]);
    }
    if cmd == CommandId::BatteryLevel as u8 {
        return HidPacket::new(CommandId::BatteryLevel, 0x0000, &[85, 0, 0x0F, 0xD2]);
    }
    if cmd == CommandId::GetCurrentConfig as u8 {
        return HidPacket::new(CommandId::GetCurrentConfig, 0x0000, &[*active_profile]);
    }
    if cmd == CommandId::SetCurrentConfig as u8 {
        *active_profile = req.payload()[0];
        return Ok(*req);
    }
    if cmd == CommandId::ReadFlashData as u8 && req.address() == FLASH_ADDR_DEBOUNCE {
        let val = *debounce_ms;
        let chk = crate::protocol::compute_checksum(&[val]);
        return HidPacket::new(CommandId::ReadFlashData, FLASH_ADDR_DEBOUNCE, &[val, chk]);
    }
    if cmd == CommandId::WriteFlashData as u8 && req.address() == FLASH_ADDR_DEBOUNCE {
        let val = req.payload()[0];
        let chk = req.payload()[1];
        if !verify_field_checksum(val, chk) {
            bail!("Invalid flash check byte 0x{:02X} for value {}", chk, val);
        }
        *debounce_ms = val;
        return Ok(*req);
    }

    bail!("Unsupported mock command 0x{:02X}", cmd)
}
