#![allow(non_camel_case_types, non_snake_case, dead_code)]

use wdk::println;
use wdk_sys::{
    NTSTATUS, STATUS_BUFFER_TOO_SMALL, STATUS_DEVICE_PROTOCOL_ERROR, STATUS_INVALID_PARAMETER,
    STATUS_SUCCESS, STATUS_UNSUCCESSFUL,
};

use crate::bluetooth::L2capA2dpConnection;

// ============================================================================
// 1. AVDTP v1.3 PROTOCOL CONSTANTS & ERROR CODES
// ============================================================================

/// AVDTP Message Types (Bits 1..0 of Header Byte 0)
pub const AVDTP_MSG_COMMAND: u8         = 0b00;
pub const AVDTP_MSG_GENERAL_REJECT: u8  = 0b01;
pub const AVDTP_MSG_RESPONSE_ACCEPT: u8 = 0b10;
pub const AVDTP_MSG_RESPONSE_REJECT: u8 = 0b11;

/// AVDTP Packet Types (Bits 3..2 of Header Byte 0)
pub const AVDTP_PKT_SINGLE: u8          = 0b00;
pub const AVDTP_PKT_START: u8           = 0b01;
pub const AVDTP_PKT_CONTINUE: u8        = 0b10;
pub const AVDTP_PKT_END: u8             = 0b11;

/// AVDTP Signal Identifiers (Bits 5..0 of Signal Byte)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvdtpSignalId {
    Discover           = 0x01,
    GetCapabilities    = 0x02,
    SetConfiguration   = 0x03,
    GetConfiguration   = 0x04,
    Reconfigure        = 0x05,
    Open               = 0x06,
    Start              = 0x07,
    Close              = 0x08,
    Suspend            = 0x09,
    Abort              = 0x0A,
    SecurityControl    = 0x0B,
    GetAllCapabilities = 0x0C,
    DelayReport        = 0x0D,
}

/// AVDTP Service Categories (TLV tags inside Capabilities)
pub const AVDTP_CAT_MEDIA_TRANSPORT: u8    = 0x01;
pub const AVDTP_CAT_REPORTING: u8          = 0x02;
pub const AVDTP_CAT_RECOVERY: u8           = 0x03;
pub const AVDTP_CAT_CONTENT_PROTECTION: u8 = 0x04;
pub const AVDTP_CAT_HEADER_COMPRESSION: u8 = 0x05;
pub const AVDTP_CAT_MULTIPLEXING: u8       = 0x06;
pub const AVDTP_CAT_MEDIA_CODEC: u8        = 0x07;
pub const AVDTP_CAT_DELAY_REPORTING: u8    = 0x08;

/// Standard Vendor & Codec IDs for fast kernel-side SEP filtering
pub const VENDOR_SAVITECH: u32 = 0x0000_053A; // LHDC
pub const VENDOR_SONY: u32     = 0x0000_012D; // LDAC
pub const VENDOR_QUALCOMM: u32 = 0x0000_00D7; // aptX HD / Adaptive
pub const VENDOR_APTX: u32     = 0x0000_004F; // Classic aptX

pub const CODEC_ID_LHDC_V5: u16 = 0x4C35;
pub const CODEC_ID_LDAC: u16    = 0x00AA;
pub const CODEC_ID_APTX_HD: u16 = 0x0024;

// ============================================================================
// 2. DATA STRUCTURES FOR DISCOVERED ENDPOINTS & CAPABILITIES
// ============================================================================

/// Represents a discovered Stream End Point (SEP) on the remote headphones
#[derive(Debug, Clone, Copy)]
pub struct RemoteSepDescriptor {
    pub seid: u8,                     // 1..=62
    pub in_use: bool,                 // True if already locked by another stream
    pub media_type: u8,               // 0x00 = Audio, 0x01 = Video
    pub is_sink: bool,                // True if TSEP == 0 (Audio Sink / Headphone)
    pub supports_delay_report: bool,  // True if AVDTP v1.3 Delay Reporting is supported
    pub codec_type: u8,               // 0x00 = SBC, 0x02 = AAC, 0xFF = Vendor Specific
    pub vendor_id: u32,               // e.g. 0x0000053A for Savitech
    pub codec_id: u16,                // e.g. 0x4C35 for LHDC v5
    pub media_codec_cie_len: u8,      // Length of the raw Media Codec capability bytes
    pub media_codec_cie: [u8; 24],    // Exact CIE payload (passed to user-service codecs.rs)
}

impl RemoteSepDescriptor {
    pub const fn empty() -> Self {
        Self {
            seid: 0,
            in_use: false,
            media_type: 0,
            is_sink: false,
            supports_delay_report: false,
            codec_type: 0,
            vendor_id: 0,
            codec_id: 0,
            media_codec_cie_len: 0,
            media_codec_cie: [0u8; 24],
        }
    }

    pub fn is_lhdc_v5(&self) -> bool {
        self.codec_type == 0xFF
            && self.vendor_id == VENDOR_SAVITECH
            && self.codec_id == CODEC_ID_LHDC_V5
    }

    pub fn is_ldac(&self) -> bool {
        self.codec_type == 0xFF
            && self.vendor_id == VENDOR_SONY
            && self.codec_id == CODEC_ID_LDAC
    }

    pub fn is_aptx_hd(&self) -> bool {
        self.codec_type == 0xFF
            && self.vendor_id == VENDOR_QUALCOMM
            && self.codec_id == CODEC_ID_APTX_HD
    }
}

/// Tracks the current state of the AVDTP state machine per the Bluetooth spec
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvdtpState {
    Idle,
    Discovered,
    Configured { acp_seid: u8, int_seid: u8 },
    Open { acp_seid: u8, int_seid: u8 },
    Streaming { acp_seid: u8, int_seid: u8 },
}

// ============================================================================
// 3. AVDTP SIGNALING ENGINE & MULTI-PACKET REASSEMBLER
// ============================================================================

pub struct AvdtpSession<'a> {
    pub conn: &'a mut L2capA2dpConnection,
    pub state: AvdtpState,
    tx_label: u8,
}

impl<'a> AvdtpSession<'a> {
    pub fn new(conn: &'a mut L2capA2dpConnection) -> Self {
        Self {
            conn,
            state: AvdtpState::Idle,
            tx_label: 0,
        }
    }

    /// Generates the 1-byte AVDTP Single-Packet Command Header:
    /// `[Bits 7..4: Transaction Label] [Bits 3..2: Packet Type (00)] [Bits 1..0: Message Type (00)]`
    #[inline]
    fn next_cmd_header(&mut self) -> (u8, u8) {
        self.tx_label = (self.tx_label.wrapping_add(1)) & 0x0F;
        let hdr = (self.tx_label << 4) | (AVDTP_PKT_SINGLE << 2) | AVDTP_MSG_COMMAND;
        (self.tx_label, hdr)
    }

    /// Sends an AVDTP command over L2CAP PSM 0x0019 and reassembles both Single
    /// and Fragmented (`START` / `CONTINUE` / `END`) responses from the headphones.
    unsafe fn transact(
        &mut self,
        sig_id: AvdtpSignalId,
        params: &[u8],
        out_payload: &mut [u8],
    ) -> Result<usize, NTSTATUS> {
        let (expected_label, hdr_byte) = self.next_cmd_header();

        let mut tx_buf = [0u8; 64];
        if 2 + params.len() > tx_buf.len() {
            return Err(STATUS_BUFFER_TOO_SMALL);
        }

        tx_buf[0] = hdr_byte;
        tx_buf[1] = sig_id as u8;
        tx_buf[2..2 + params.len()].copy_from_slice(params);

        let status = self.conn.send_l2cap_packet(
            self.conn.signaling_channel,
            &tx_buf[..2 + params.len()],
        );
        if status < 0 {
            println!("[AVDTP] L2CAP TX failed for signal {:#x}: {:#x}", sig_id as u8, status);
            return Err(status);
        }

        // Receive & reassemble response
        let mut rx_chunk = [0u8; 256];
        let mut total_copied = 0usize;
        let mut packets_remaining: u8 = 1;
        let mut is_first_chunk = true;

        while packets_remaining > 0 {
            let bytes_read = self.conn.recv_l2cap_signaling(&mut rx_chunk)?;
            if bytes_read < 2 {
                return Err(STATUS_DEVICE_PROTOCOL_ERROR);
            }

            let rx_hdr = rx_chunk[0];
            let rx_label = (rx_hdr >> 4) & 0x0F;
            let pkt_type = (rx_hdr >> 2) & 0x03;
            let msg_type = rx_hdr & 0x03;

            // If the headphone sent an asynchronous command (like DelayReport 0x0D)
            // while we are waiting for our reply, auto-ACK it and keep reading.
            if msg_type == AVDTP_MSG_COMMAND {
                self.handle_incoming_headphone_command(&rx_chunk[..bytes_read]);
                continue;
            }

            if rx_label != expected_label {
                println!(
                    "[AVDTP] Warning: Mismatched TX label (expected {}, got {})",
                    expected_label, rx_label
                );
            }

            if msg_type == AVDTP_MSG_GENERAL_REJECT || msg_type == AVDTP_MSG_RESPONSE_REJECT {
                let err_code = if bytes_read >= 3 { rx_chunk[bytes_read - 1] } else { 0xFF };
                println!(
                    "[AVDTP] Signal {:#x} REJECTED by headphones! AVDTP Error Code: {:#x}",
                    sig_id as u8, err_code
                );
                return Err(STATUS_UNSUCCESSFUL);
            }

            match pkt_type {
                AVDTP_PKT_SINGLE => {
                    // Header is 2 bytes: [Hdr, SigID]
                    let data_len = bytes_read - 2;
                    if total_copied + data_len > out_payload.len() {
                        return Err(STATUS_BUFFER_TOO_SMALL);
                    }
                    out_payload[total_copied..total_copied + data_len]
                        .copy_from_slice(&rx_chunk[2..bytes_read]);
                    total_copied += data_len;
                    packets_remaining = 0;
                }
                AVDTP_PKT_START => {
                    // Header is 3 bytes: [Hdr, NumOfSignalPackets, SigID]
                    if bytes_read < 3 {
                        return Err(STATUS_DEVICE_PROTOCOL_ERROR);
                    }
                    packets_remaining = rx_chunk[1].saturating_sub(1);
                    let data_len = bytes_read - 3;
                    if total_copied + data_len > out_payload.len() {
                        return Err(STATUS_BUFFER_TOO_SMALL);
                    }
                    out_payload[total_copied..total_copied + data_len]
                        .copy_from_slice(&rx_chunk[3..bytes_read]);
                    total_copied += data_len;
                }
                AVDTP_PKT_CONTINUE | AVDTP_PKT_END => {
                    // Header is 1 byte: [Hdr]
                    let data_len = bytes_read - 1;
                    if total_copied + data_len > out_payload.len() {
                        return Err(STATUS_BUFFER_TOO_SMALL);
                    }
                    out_payload[total_copied..total_copied + data_len]
                        .copy_from_slice(&rx_chunk[1..bytes_read]);
                    total_copied += data_len;
                    if pkt_type == AVDTP_PKT_END {
                        packets_remaining = 0;
                    } else {
                        packets_remaining = packets_remaining.saturating_sub(1);
                    }
                }
                _ => return Err(STATUS_DEVICE_PROTOCOL_ERROR),
            }

            is_first_chunk = false;
            if is_first_chunk {} // suppress unused warning
        }

        Ok(total_copied)
    }

    /// Handles unsolicited commands sent FROM the headphones TO Windows
    /// (most commonly `AVDTP_DELAYREPORT` `0x0D` when LHDC v5 Low-Latency mode is active).
    unsafe fn handle_incoming_headphone_command(&mut self, cmd_pkt: &[u8]) {
        if cmd_pkt.len() < 2 {
            return;
        }
        let rx_label = cmd_pkt[0] & 0xF0;
        let sig_id = cmd_pkt[1] & 0x3F;

        if sig_id == (AvdtpSignalId::DelayReport as u8) && cmd_pkt.len() >= 5 {
            let delay_tenth_ms = u16::from_be_bytes([cmd_pkt[3], cmd_pkt[4]]);
            println!(
                "[AVDTP] Headphone reported DAC latency: {}.{} ms",
                delay_tenth_ms / 10,
                delay_tenth_ms % 10
            );
            // Send Response Accept for DelayReport
            let ack = [rx_label | AVDTP_MSG_RESPONSE_ACCEPT, sig_id];
            let _ = self.conn.send_l2cap_packet(self.conn.signaling_channel, &ack);
        }
    }

    // ========================================================================
    // STEP 1 & 2: DISCOVER SEPs + QUERY & PARSE ALL CODEC CAPABILITIES
    // ========================================================================

    /// Executes Step 1 (`AVDTP_DISCOVER`) and Step 2 (`AVDTP_GET_ALL_CAPABILITIES` with
    /// automatic fallback to `AVDTP_GET_CAPABILITIES`) for every Audio Sink SEP on the headphones.
    pub unsafe fn step_1_and_2_discover_and_probe_seps(
        &mut self,
        out_seps: &mut [RemoteSepDescriptor],
    ) -> Result<usize, NTSTATUS> {
        let mut disc_payload = [0u8; 64];
        println!("[AVDTP] Step 1: Sending AVDTP_DISCOVER...");
        let disc_len = self.transact(AvdtpSignalId::Discover, &[], &mut disc_payload)?;

        let mut sep_count = 0usize;

        for pair in disc_payload[..disc_len].chunks_exact(2) {
            if sep_count >= out_seps.len() {
                break;
            }

            let seid = (pair[0] >> 2) & 0x3F;
            let in_use = ((pair[0] >> 1) & 0x01) != 0;
            let media_type = (pair[1] >> 4) & 0x0F;
            let tsep = (pair[1] >> 3) & 0x01; // 0 = Sink, 1 = Source

            // Only probe Audio Sinks (Headphones/Speakers)
            if media_type != 0x00 || tsep != 0x00 || seid == 0 {
                continue;
            }

            let mut sep = RemoteSepDescriptor::empty();
            sep.seid = seid;
            sep.in_use = in_use;
            sep.media_type = media_type;
            sep.is_sink = true;

            // Step 2: Query capabilities for this SEID
            if self.probe_single_sep_capabilities(&mut sep).is_ok() {
                out_seps[sep_count] = sep;
                sep_count += 1;
            }
        }

        self.state = AvdtpState::Discovered;
        Ok(sep_count)
    }

    /// Queries a single SEID using `AVDTP_GET_ALL_CAPABILITIES` (`0x0C`), falling back to
    /// `AVDTP_GET_CAPABILITIES` (`0x02`) if the headphones only implement AVDTP v1.2.
    unsafe fn probe_single_sep_capabilities(
        &mut self,
        sep: &mut RemoteSepDescriptor,
    ) -> Result<(), NTSTATUS> {
        let param = [sep.seid << 2];
        let mut raw_tlv = [0u8; 128];

        let tlv_len = match self.transact(AvdtpSignalId::GetAllCapabilities, &param, &mut raw_tlv) {
            Ok(len) => len,
            Err(_) => {
                println!(
                    "[AVDTP] SEID {} rejected GetAllCapabilities (0x0C), falling back to GetCapabilities (0x02)",
                    sep.seid
                );
                self.transact(AvdtpSignalId::GetCapabilities, &param, &mut raw_tlv)?
            }
        };

        // Walk the Service Category TLV list: [CategoryID, Length, ...Value...]
        let mut cursor = 0usize;
        while cursor + 2 <= tlv_len {
            let category = raw_tlv[cursor];
            let length = raw_tlv[cursor + 1] as usize;
            let val_start = cursor + 2;
            let val_end = val_start + length;

            if val_end > tlv_len {
                break;
            }

            let val_slice = &raw_tlv[val_start..val_end];

            match category {
                AVDTP_CAT_DELAY_REPORTING => {
                    sep.supports_delay_report = true;
                }
                AVDTP_CAT_MEDIA_CODEC => {
                    if val_slice.len() >= 2 {
                        sep.codec_type = val_slice[1];
                        let copy_len = val_slice.len().min(sep.media_codec_cie.len());
                        sep.media_codec_cie[..copy_len].copy_from_slice(&val_slice[..copy_len]);
                        sep.media_codec_cie_len = copy_len as u8;

                        // If Vendor Specific (0xFF), extract 32-bit Vendor ID and 16-bit Codec ID
                        if sep.codec_type == 0xFF && val_slice.len() >= 8 {
                            sep.vendor_id = u32::from_le_bytes([
                                val_slice[2],
                                val_slice[3],
                                val_slice[4],
                                val_slice[5],
                            ]);
                            sep.codec_id = u16::from_le_bytes([val_slice[6], val_slice[7]]);

                            println!(
                                "[AVDTP] SEID {} -> Vendor Codec (VendorID: {:#010x}, CodecID: {:#06x})",
                                sep.seid, sep.vendor_id, sep.codec_id
                            );
                        } else {
                            println!(
                                "[AVDTP] SEID {} -> Standard Codec Type {:#x}",
                                sep.seid, sep.codec_type
                            );
                        }
                    }
                }
                _ => {}
            }

            cursor = val_end;
        }

        Ok(())
    }

    // ========================================================================
    // STEP 3: SET CONFIGURATION (LHDC v5 / LDAC / aptX HD + Delay Reporting)
    // ========================================================================

    /// Sends `AVDTP_SET_CONFIGURATION` (`0x03`).
    /// Accepts either a full Service Category TLV buffer (`[0x01, 0x00, 0x07, len, ...]`)
    /// or raw Media Codec CIE bytes and automatically wraps them with Media Transport (`0x01`)
    /// and Delay Reporting (`0x08`) if the headphone SEP supports it.
    pub unsafe fn step_3_set_configuration(
        &mut self,
        acp_seid: u8,
        int_seid: u8,
        codec_config_bytes: &[u8],
        enable_delay_reporting: bool,
    ) -> Result<(), NTSTATUS> {
        if codec_config_bytes.is_empty() {
            return Err(STATUS_INVALID_PARAMETER);
        }

        let mut params = [0u8; 48];
        params[0] = acp_seid << 2;
        params[1] = int_seid << 2;
        let mut idx = 2usize;

        // Check if caller already prepended AVDTP_CAT_MEDIA_TRANSPORT (0x01, 0x00)
        if codec_config_bytes.len() >= 4
            && codec_config_bytes[0] == AVDTP_CAT_MEDIA_TRANSPORT
            && codec_config_bytes[2] == AVDTP_CAT_MEDIA_CODEC
        {
            let copy_len = codec_config_bytes.len();
            if idx + copy_len + 2 > params.len() {
                return Err(STATUS_BUFFER_TOO_SMALL);
            }
            params[idx..idx + copy_len].copy_from_slice(codec_config_bytes);
            idx += copy_len;
        } else {
            // Auto-wrap raw CIE bytes in Media Transport (0x01, 0x00) + Media Codec (0x07, len)
            params[idx] = AVDTP_CAT_MEDIA_TRANSPORT;
            params[idx + 1] = 0x00;
            idx += 2;

            params[idx] = AVDTP_CAT_MEDIA_CODEC;
            params[idx + 1] = codec_config_bytes.len() as u8;
            idx += 2;

            params[idx..idx + codec_config_bytes.len()].copy_from_slice(codec_config_bytes);
            idx += codec_config_bytes.len();
        }

        // Enable AVDTP v1.3 Delay Reporting if supported by the LHDC v5 / LDAC Sink
        if enable_delay_reporting && idx + 2 <= params.len() {
            params[idx] = AVDTP_CAT_DELAY_REPORTING;
            params[idx + 1] = 0x00;
            idx += 2;
        }

        println!(
            "[AVDTP] Step 3: Sending AVDTP_SET_CONFIGURATION (ACP SEID={}, INT SEID={})...",
            acp_seid, int_seid
        );

        let mut resp = [0u8; 16];
        self.transact(AvdtpSignalId::SetConfiguration, &params[..idx], &mut resp)?;

        self.state = AvdtpState::Configured { acp_seid, int_seid };
        println!("[AVDTP] Step 3 Complete: Headphone accepted codec configuration!");
        Ok(())
    }

    // ========================================================================
    // STEP 4: OPEN STREAM + ESTABLISH SECOND L2CAP MEDIA TRANSPORT CHANNEL
    // ========================================================================

    /// Sends `AVDTP_OPEN` (`0x06`) on the signaling channel, and immediately opens the
    /// second L2CAP channel (`PSM 0x0019`) dedicated to high-MTU media transport (`1005` bytes).
    pub unsafe fn step_4_open_and_connect_media_channel(
        &mut self,
        acp_seid: u8,
        preferred_media_mtu: u16,
    ) -> Result<u16, NTSTATUS> {
        let param = [acp_seid << 2];
        let mut resp = [0u8; 16];

        println!("[AVDTP] Step 4a: Sending AVDTP_OPEN for SEID {}...", acp_seid);
        self.transact(AvdtpSignalId::Open, &param, &mut resp)?;

        // Per Bluetooth A2DP Specification Section 4.1:
        // Once AVDTP_OPEN succeeds, the Initiator MUST open a second L2CAP channel on PSM 0x0019
        // which the Acceptor automatically binds as the Media Transport bearer.
        println!(
            "[AVDTP] Step 4b: Opening second L2CAP Media Transport socket (Preferred MTU={})...",
            preferred_media_mtu
        );

        let (media_handle, negotiated_mtu) = L2capA2dpConnection::open_avdtp_channel(
            self.conn.io_target,
            self.conn.bt_address,
            preferred_media_mtu,
        )?;

        self.conn.media_channel = media_handle;
        self.conn.negotiated_out_mtu = negotiated_mtu;

        let int_seid = match self.state {
            AvdtpState::Configured { int_seid, .. } => int_seid,
            _ => 1,
        };
        self.state = AvdtpState::Open { acp_seid, int_seid };

        println!(
            "[AVDTP] Step 4 Complete: Media Transport Channel ready (Negotiated MTU: {} bytes).",
            negotiated_mtu
        );
        Ok(negotiated_mtu)
    }

    // ========================================================================
    // STEP 5: START STREAMING
    // ========================================================================

    /// Sends `AVDTP_START` (`0x07`). Once this returns `Ok(())`, the headphones' DAC is active
    /// and expecting 5ms LHDC v5 / LDAC RTP media frames on `conn.media_channel`.
    pub unsafe fn step_5_start(&mut self, acp_seid: u8) -> Result<(), NTSTATUS> {
        let param = [acp_seid << 2];
        let mut resp = [0u8; 16];

        println!("[AVDTP] Step 5: Sending AVDTP_START for SEID {}...", acp_seid);
        self.transact(AvdtpSignalId::Start, &param, &mut resp)?;

        let int_seid = match self.state {
            AvdtpState::Open { int_seid, .. } => int_seid,
            _ => 1,
        };
        self.state = AvdtpState::Streaming { acp_seid, int_seid };

        println!("[AVDTP] Step 5 Complete: LHDC v5 / Hi-Res Audio Stream is LIVE!");
        Ok(())
    }

    // ========================================================================
    // LIVE STREAM MANAGEMENT (SUSPEND, RECONFIGURE, CLOSE, ABORT)
    // ========================================================================

    /// Pauses the active stream (`AVDTP_SUSPEND` `0x09`) without dropping the L2CAP media socket.
    /// Required before calling `reconfigure_codec_params` (e.g. switching 48kHz <-> 96kHz).
    pub unsafe fn suspend_stream(&mut self, acp_seid: u8) -> Result<(), NTSTATUS> {
        let param = [acp_seid << 2];
        let mut resp = [0u8; 16];
        self.transact(AvdtpSignalId::Suspend, &param, &mut resp)?;

        let int_seid = match self.state {
            AvdtpState::Streaming { int_seid, .. } => int_seid,
            _ => 1,
        };
        self.state = AvdtpState::Open { acp_seid, int_seid };
        Ok(())
    }

    /// Reconfigures the active SEP (`AVDTP_RECONFIGURE` `0x05`) while in the `Open` state
    /// (e.g. toggling LHDC v5 Low-Latency mode or switching sample rates on the fly).
    pub unsafe fn reconfigure_stream(
        &mut self,
        acp_seid: u8,
        new_media_codec_cie: &[u8],
    ) -> Result<(), NTSTATUS> {
        if let AvdtpState::Streaming { .. } = self.state {
            self.suspend_stream(acp_seid)?;
        }

        let mut params = [0u8; 32];
        params[0] = acp_seid << 2;
        params[1] = AVDTP_CAT_MEDIA_CODEC;
        params[2] = new_media_codec_cie.len() as u8;

        let total_len = 3 + new_media_codec_cie.len();
        if total_len > params.len() {
            return Err(STATUS_BUFFER_TOO_SMALL);
        }
        params[3..total_len].copy_from_slice(new_media_codec_cie);

        let mut resp = [0u8; 16];
        self.transact(AvdtpSignalId::Reconfigure, &params[..total_len], &mut resp)?;

        // Resume streaming immediately
        self.step_5_start(acp_seid)
    }

    /// Cleanly tears down the stream (`AVDTP_CLOSE` `0x08`) and closes the L2CAP media channel.
    pub unsafe fn close_stream(&mut self, acp_seid: u8) -> Result<(), NTSTATUS> {
        let param = [acp_seid << 2];
        let mut resp = [0u8; 16];
        let _ = self.transact(AvdtpSignalId::Close, &param, &mut resp);

        if !self.conn.media_channel.is_null() {
            let _ = self.conn.close_channel(self.conn.media_channel);
            self.conn.media_channel = core::ptr::null_mut();
        }

        self.state = AvdtpState::Idle;
        Ok(())
    }
}