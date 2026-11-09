use anyhow::{bail, ensure, Result};
use std::fmt;

/// Corsair USB Vendor ID.
pub const VENDOR_ID: u16 = 0x1B1C;

/// Product ID when the Sabre v2 Pro is connected directly via USB cable.
pub const PID_WIRED_USB: u16 = 0x2B28;

/// Product ID for the Sabre v2 Pro 2.4 GHz wireless USB receiver.
pub const PID_WIRELESS_RECEIVER: u16 = 0x2B2A;

/// USB interface number carrying the vendor control HID collection (`mi_01`).
pub const TARGET_INTERFACE: i32 = 1;

/// Windows HID top-level collection identifier for the vendor control channel (`col05`).
pub const TARGET_COLLECTION_SUBSTR: &str = "col05";

/// HID Report ID used for all 17-byte control commands and responses.
pub const REPORT_ID: u8 = 0x08;

/// Fixed total byte length of each HID report (including `buf[0]` Report ID and `buf[16]` checksum).
pub const PACKET_LEN: usize = 17;

/// Maximum payload bytes carried in `buf[6..16]`.
pub const MAX_PAYLOAD_LEN: usize = 10;

/// Checksum base constant (`0x55 - sum(bytes)` mod 256).
pub const CHECKSUM_SEED: u8 = 0x55;

/// 16-bit flash offset for `keyDebounceTime` (1-byte value in ms at `0x00A9`, check byte at `0x00AA`).
pub const FLASH_ADDR_DEBOUNCE: u16 = 0x00A9;

/// Factory default debounce time in milliseconds when flash is uninitialized.
pub const DEFAULT_DEBOUNCE_MS: u8 = 8;

/// Maximum debounce time exposed by the reference GUI without `--force`.
pub const MAX_SAFE_DEBOUNCE_MS: u8 = 20;

/// USB HID command identifiers used by the mouse firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
#[repr(u8)]
pub enum CommandId {
    EncryptionData = 0x01,
    PcDriverStatus = 0x02,
    DeviceOnLine = 0x03,
    BatteryLevel = 0x04,
    DongleEnterPair = 0x05,
    GetPairState = 0x06,
    WriteFlashData = 0x07,
    ReadFlashData = 0x08,
    ReadVersion = 0x09,
    ReportDataToPc = 0x0A,
    GetCurrentConfig = 0x0E,
    SetCurrentConfig = 0x0F,
}

/// Computes the single-byte additive complement checksum (`(0x55 - sum(bytes)) & 0xFF`).
///
/// This checksum is used both for:
/// 1. The trailing packet checksum at `packet[16]` over `packet[0..16]` (including `packet[0] = 0x08`).
/// 2. Individual flash parameter validation bytes stored immediately after each flash field.
#[inline]
pub fn compute_checksum(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().copied().fold(0u8, u8::wrapping_add);
    CHECKSUM_SEED.wrapping_sub(sum)
}

/// Validates that a 1-byte value and its paired check byte satisfy `check == (0x55 - value) & 0xFF`.
#[inline]
pub fn verify_field_checksum(value: u8, check_byte: u8) -> bool {
    compute_checksum(&[value]) == check_byte
}

/// Validates the trailing checksum byte (`buf[16]`) of a 17-byte HID report.
#[inline]
pub fn verify_packet_checksum(packet: &[u8; PACKET_LEN]) -> bool {
    compute_checksum(&packet[0..PACKET_LEN - 1]) == packet[PACKET_LEN - 1]
}

/// A 17-byte USB HID command or response packet.
///
/// Layout:
/// - `buf[0]`: Report ID (`0x08`)
/// - `buf[1]`: Command ID (`CommandId`)
/// - `buf[2]`: Command status (`0x00` for requests and successful responses)
/// - `buf[3]`: High byte of 16-bit flash address (`(address >> 8) as u8`)
/// - `buf[4]`: Low byte of 16-bit flash address (`(address & 0xFF) as u8`)
/// - `buf[5]`: Data length (`0..=10`)
/// - `buf[6..16]`: 10-byte payload buffer
/// - `buf[16]`: Packet checksum (`compute_checksum(&buf[0..16])`)
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HidPacket(pub [u8; PACKET_LEN]);

impl HidPacket {
    /// Constructs a new 17-byte HID packet with the given command, 16-bit big-endian address,
    /// explicit length field, and up to 10 payload bytes.
    pub fn with_length(
        cmd: CommandId,
        address: u16,
        length_field: u8,
        payload: &[u8],
    ) -> Result<Self> {
        ensure!(
            payload.len() <= MAX_PAYLOAD_LEN,
            "HID payload length {} exceeds maximum of {} bytes",
            payload.len(),
            MAX_PAYLOAD_LEN
        );

        let mut buf = [0u8; PACKET_LEN];
        buf[0] = REPORT_ID;
        buf[1] = cmd as u8;
        buf[2] = 0x00;
        buf[3] = (address >> 8) as u8;
        buf[4] = (address & 0xFF) as u8;
        buf[5] = length_field;
        buf[6..6 + payload.len()].copy_from_slice(payload);
        buf[PACKET_LEN - 1] = compute_checksum(&buf[0..PACKET_LEN - 1]);
        Ok(Self(buf))
    }

    /// Constructs a new 17-byte HID packet where `buf[5]` equals `payload.len()`.
    pub fn new(cmd: CommandId, address: u16, payload: &[u8]) -> Result<Self> {
        Self::with_length(cmd, address, payload.len() as u8, payload)
    }

    /// Builds `PCDriverStatus (0x02)` notification packet (`active = true` on connect, `false` on exit).
    pub fn pc_driver_status(active: bool) -> Self {
        Self::new(CommandId::PcDriverStatus, 0x0000, &[u8::from(active)])
            .expect("static 1-byte payload is valid")
    }

    /// Builds `DeviceOnLine (0x03)` query packet.
    pub fn device_online() -> Self {
        Self::new(CommandId::DeviceOnLine, 0x0000, &[]).expect("empty payload is valid")
    }

    /// Builds `BatteryLevel (0x04)` query packet.
    pub fn battery_level() -> Self {
        Self::new(CommandId::BatteryLevel, 0x0000, &[]).expect("empty payload is valid")
    }

    /// Builds `GetCurrentConfig (0x0E)` query packet to fetch the active profile slot (`0..=4`).
    pub fn get_current_config() -> Self {
        Self::new(CommandId::GetCurrentConfig, 0x0000, &[]).expect("empty payload is valid")
    }

    /// Builds `ReadFlashData (0x08)` request to read `length` bytes (`1..=10`) starting at `address`.
    pub fn read_flash(address: u16, length: u8) -> Result<Self> {
        ensure!(
            (1..=MAX_PAYLOAD_LEN as u8).contains(&length),
            "flash read length must be between 1 and {} bytes",
            MAX_PAYLOAD_LEN
        );
        Self::with_length(CommandId::ReadFlashData, address, length, &[])
    }

    /// Builds `ReadFlashData (0x08)` request for the 2-byte debounce entry at `0x00A9..=0x00AA`.
    pub fn read_debounce() -> Self {
        Self::read_flash(FLASH_ADDR_DEBOUNCE, 2).expect("2-byte flash read is valid")
    }

    /// Builds `WriteFlashData (0x07)` request to write `debounce_ms` at `0x00A9` and its check byte at `0x00AA`.
    pub fn write_debounce(debounce_ms: u8) -> Self {
        let check_byte = compute_checksum(&[debounce_ms]);
        Self::new(
            CommandId::WriteFlashData,
            FLASH_ADDR_DEBOUNCE,
            &[debounce_ms, check_byte],
        )
        .expect("2-byte flash write is valid")
    }

    #[inline]
    pub fn as_bytes(&self) -> &[u8; PACKET_LEN] {
        &self.0
    }

    #[inline]
    pub fn command_byte(&self) -> u8 {
        self.0[1]
    }

    #[inline]
    pub fn status_byte(&self) -> u8 {
        self.0[2]
    }

    #[inline]
    pub fn address(&self) -> u16 {
        u16::from_be_bytes([self.0[3], self.0[4]])
    }

    #[inline]
    #[allow(dead_code)]
    pub fn payload_len(&self) -> u8 {
        self.0[5]
    }

    #[inline]
    pub fn payload(&self) -> &[u8] {
        &self.0[6..16]
    }
}

impl fmt::Debug for HidPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HidPacket({})", self)
    }
}

impl fmt::Display for HidPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{:02X}", byte)?;
        }
        Ok(())
    }
}

/// Parsed response for `DeviceOnLine (0x03)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnlineStatus {
    pub online: bool,
    pub wireless_address: [u8; 3],
}

/// Parsed response for `BatteryLevel (0x04)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    pub level_percent: u8,
    pub charging: bool,
    pub voltage_mv: u16,
}

/// Parsed response for reading debounce time at flash address `0x00A9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebounceReading {
    /// Effective debounce time in milliseconds (falls back to `DEFAULT_DEBOUNCE_MS` if flash is uninitialized).
    pub debounce_ms: u8,
    /// Raw byte stored at flash address `0x00A9`.
    pub raw_value: u8,
    /// Raw check byte stored at flash address `0x00AA`.
    pub raw_check_byte: u8,
    /// Whether `raw_check_byte == (0x55 - raw_value) & 0xFF`.
    pub checksum_valid: bool,
}

/// Parses a `DeviceOnLine (0x03)` response packet.
pub fn parse_online_response(rsp: &HidPacket) -> Result<OnlineStatus> {
    ensure!(
        rsp.command_byte() == CommandId::DeviceOnLine as u8,
        "unexpected command byte 0x{:02X} for DeviceOnLine response",
        rsp.command_byte()
    );
    let data = rsp.payload();
    Ok(OnlineStatus {
        online: data[0] != 0,
        wireless_address: [data[1], data[2], data[3]],
    })
}

/// Parses a `BatteryLevel (0x04)` response packet (`CS_GetDeviceBatteryStatus`).
pub fn parse_battery_response(rsp: &HidPacket) -> Result<BatteryStatus> {
    ensure!(
        rsp.command_byte() == CommandId::BatteryLevel as u8,
        "unexpected command byte 0x{:02X} for BatteryLevel response",
        rsp.command_byte()
    );
    let data = rsp.payload();
    Ok(BatteryStatus {
        level_percent: data[0].min(100),
        charging: data[1] != 0,
        voltage_mv: u16::from_be_bytes([data[2], data[3]]),
    })
}

/// Parses a `GetCurrentConfig (0x0E)` response packet, returning the 0-based profile index.
pub fn parse_current_config_response(rsp: &HidPacket) -> Result<u8> {
    ensure!(
        rsp.command_byte() == CommandId::GetCurrentConfig as u8,
        "unexpected command byte 0x{:02X} for GetCurrentConfig response",
        rsp.command_byte()
    );
    Ok(rsp.payload()[0])
}

/// Parses a `ReadFlashData (0x08)` response for `FLASH_ADDR_DEBOUNCE (0x00A9)`.
pub fn parse_debounce_response(rsp: &HidPacket) -> Result<DebounceReading> {
    ensure!(
        rsp.command_byte() == CommandId::ReadFlashData as u8,
        "unexpected command byte 0x{:02X} for ReadFlashData response",
        rsp.command_byte()
    );
    ensure!(
        rsp.address() == FLASH_ADDR_DEBOUNCE,
        "unexpected flash address 0x{:04X} (expected 0x{:04X})",
        rsp.address(),
        FLASH_ADDR_DEBOUNCE
    );
    let data = rsp.payload();
    let raw_value = data[0];
    let raw_check_byte = data[1];
    let checksum_valid = verify_field_checksum(raw_value, raw_check_byte);
    let debounce_ms = if checksum_valid {
        raw_value
    } else {
        DEFAULT_DEBOUNCE_MS
    };

    Ok(DebounceReading {
        debounce_ms,
        raw_value,
        raw_check_byte,
        checksum_valid,
    })
}

/// Verifies the device's response to a `WriteFlashData (0x07)` request.
///
/// The firmware echoes the 17-byte `WriteFlashData` packet back on success.
pub fn verify_write_response(req: &HidPacket, rsp: &HidPacket) -> Result<()> {
    if req.as_bytes() == rsp.as_bytes() {
        return Ok(());
    }
    if rsp.command_byte() == CommandId::WriteFlashData as u8
        && rsp.status_byte() == 0x00
        && rsp.address() == req.address()
        && rsp.payload()[0..2] == req.payload()[0..2]
    {
        return Ok(());
    }
    bail!(
        "WriteFlashData response mismatch: sent [{}], received [{}]",
        req,
        rsp
    )
}

/// Validates a user-requested debounce value in milliseconds.
pub fn validate_debounce_ms(ms: u16, force: bool) -> Result<u8> {
    if force {
        ensure!(
            ms <= u8::MAX as u16,
            "debounce time {} ms exceeds 1-byte flash maximum (255 ms)",
            ms
        );
        Ok(ms as u8)
    } else {
        ensure!(
            ms <= MAX_SAFE_DEBOUNCE_MS as u16,
            "debounce time {} ms is outside the standard 0..={} ms range (pass --force to allow up to 255 ms)",
            ms,
            MAX_SAFE_DEBOUNCE_MS
        );
        Ok(ms as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_calculation() {
        // Single byte field checksum: 0x55 - 8 = 0x4D
        assert_eq!(compute_checksum(&[8]), 0x4D);
        assert!(verify_field_checksum(8, 0x4D));

        // 0 ms debounce: 0x55 - 0 = 0x55
        assert_eq!(compute_checksum(&[0]), 0x55);
        assert!(verify_field_checksum(0, 0x55));

        // DeviceOnLine request: [0x08, 0x03, 0, ..., 0] -> sum = 0x0B -> 0x55 - 0x0B = 0x4A
        let online_pkt = HidPacket::device_online();
        assert_eq!(online_pkt.as_bytes()[0], 0x08);
        assert_eq!(online_pkt.as_bytes()[1], 0x03);
        assert_eq!(online_pkt.as_bytes()[16], 0x4A);
        assert!(verify_packet_checksum(online_pkt.as_bytes()));
    }

    #[test]
    fn test_write_debounce_packet_format() {
        // Writing 4 ms debounce to 0x00A9:
        // buf[0] = 0x08 (Report ID)
        // buf[1] = 0x07 (WriteFlashData)
        // buf[2] = 0x00
        // buf[3] = 0x00 (Addr High)
        // buf[4] = 0xA9 (Addr Low)
        // buf[5] = 0x02 (Length = 2)
        // buf[6] = 0x04 (4 ms)
        // buf[7] = 0x51 (0x55 - 0x04)
        let pkt = HidPacket::write_debounce(4);
        let b = pkt.as_bytes();
        assert_eq!(&b[0..8], &[0x08, 0x07, 0x00, 0x00, 0xA9, 0x02, 0x04, 0x51]);
        assert_eq!(&b[8..16], &[0u8; 8]);
        assert!(verify_packet_checksum(b));
    }

    #[test]
    fn test_read_debounce_response_parsing() {
        let mut raw = [0u8; PACKET_LEN];
        raw[0] = REPORT_ID;
        raw[1] = CommandId::ReadFlashData as u8;
        raw[3] = 0x00;
        raw[4] = 0xA9;
        raw[5] = 0x02;
        raw[6] = 2; // 2 ms
        raw[7] = compute_checksum(&[2]); // 0x53
        raw[16] = compute_checksum(&raw[0..16]);

        let parsed = parse_debounce_response(&HidPacket(raw)).unwrap();
        assert_eq!(parsed.debounce_ms, 2);
        assert_eq!(parsed.raw_value, 2);
        assert_eq!(parsed.raw_check_byte, 0x53);
        assert!(parsed.checksum_valid);
    }

    #[test]
    fn test_uninitialized_flash_falls_back_to_default() {
        let mut raw = [0u8; PACKET_LEN];
        raw[0] = REPORT_ID;
        raw[1] = CommandId::ReadFlashData as u8;
        raw[3] = 0x00;
        raw[4] = 0xA9;
        raw[5] = 0x02;
        raw[6] = 0xFF;
        raw[7] = 0xFF;
        raw[16] = compute_checksum(&raw[0..16]);

        let parsed = parse_debounce_response(&HidPacket(raw)).unwrap();
        assert_eq!(parsed.debounce_ms, DEFAULT_DEBOUNCE_MS);
        assert!(!parsed.checksum_valid);
    }
}
