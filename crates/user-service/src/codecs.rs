use ipc_protocol::{AvdtpSetConfigRequest, NegotiatedCodec};

// Raw FFI bindings to AOSP liblhdcv5 & libldac C libraries
#[allow(non_camel_case_types)]
mod ffi {
    use std::os::raw::{c_int, c_uchar, c_uint, c_void};

    pub type HANDLE_LHDC_BT = *mut c_void;
    pub type HANDLE_LDAC_BT = *mut c_void;

    extern "C" {
        // --- Savitech LHDC v5 (from AUR liblhdcv5 / AOSP) ---
        pub fn lhdcBT_get_handle(version: c_int) -> HANDLE_LHDC_BT;
        pub fn lhdcBT_init_encoder(
            handle: HANDLE_LHDC_BT,
            sampling_freq: c_int,
            bit_per_sample: c_int,
            bitrate_inx: c_int,
            dual_channel: c_int,
            need_padding: c_int,
            mtu_size: c_int,
            interval: c_int,
        ) -> c_int;
        pub fn lhdcBT_encode(
            handle: HANDLE_LHDC_BT,
            in_pcm: *mut c_void,
            out_buf: *mut c_uchar,
            out_len: *mut c_uint,
            out_frames: *mut c_uint,
        ) -> c_int;
        pub fn lhdcBT_free_handle(handle: HANDLE_LHDC_BT);

        // --- Sony LDAC (from AOSP platform/external/libldac) ---
        pub fn ldacBT_get_handle() -> HANDLE_LDAC_BT;
        pub fn ldacBT_init_handle_encode(
            handle: HANDLE_LDAC_BT,
            mtu: c_int,
            eqmid: c_int,
            cm: c_int,
            fmt: c_int,
            sf: c_int,
        ) -> c_int;
        pub fn ldacBT_encode(
            handle: HANDLE_LDAC_BT,
            p_pcm: *mut c_void,
            pcm_used: *mut c_int,
            p_stream: *mut c_uchar,
            stream_sz: *mut c_int,
            frame_num: *mut c_int,
        ) -> c_int;
        pub fn ldacBT_free_handle(handle: HANDLE_LDAC_BT);
    }
}

pub trait CodecEngine: Send {
    fn build_avdtp_config(&self, acp_seid: u8) -> AvdtpSetConfigRequest;
    fn encode_pcm(&mut self, pcm_bytes: &[u8], out_l2cap: &mut [u8]) -> usize;
}

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use ipc_protocol::{AvdtpSetConfigRequest, NegotiatedCodec};
use std::os::raw::{c_int, c_uchar, c_uint, c_void};
use std::ptr::null_mut;

// ============================================================================
// 1. RAW C FFI BINDINGS (liblhdcv5, libldac, libopenaptx)
// ============================================================================
pub mod ffi {
    use super::*;

    pub type HANDLE_LHDC_BT = *mut c_void;
    pub type HANDLE_LDAC_BT = *mut c_void;

    /// Exact bitrate index enum from AOSP `lhdcBT.h`
    #[repr(i32)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LhdcBitrateIndex {
        LhdcBitrateAuto    = 0, // Auto Bitrate (ABR)
        LhdcBitrate1000K   = 1, // 1000 kbps (LHDC v5 Max Quality / Lossless base)
        LhdcBitrate900K    = 2, // 900 kbps
        LhdcBitrate600K    = 3, // 600 kbps
        LhdcBitrate500K    = 4, // 500 kbps
        LhdcBitrate400K    = 5, // 400 kbps
        LhdcBitrate320K    = 6, // 320 kbps
        LhdcBitrate256K    = 7, // 256 kbps
        LhdcBitrate192K    = 8, // 192 kbps
        LhdcBitrate128K    = 9, // 128 kbps (High RF congestion fallback)
    }

    #[repr(i32)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LhdcFeatureMeta {
        MetaNone     = 0,
        MetaJas      = 1, // Japan Audio Society Hi-Res Wireless
        MetaAr       = 2, // Audio Reality / Spatial 3D
        MetaLl       = 4, // Low Latency Gaming Mode
        MetaLossless = 8, // Lossless 16-bit/44.1kHz or 24-bit/48kHz
    }

    extern "C" {
        // --------------------------------------------------------------------
        // Savitech LHDC v5 API (from AUR `liblhdcv5` / AOSP `lhdcBT.h`)
        // --------------------------------------------------------------------
        pub fn lhdcBT_get_handle(version: c_int) -> HANDLE_LHDC_BT;
        pub fn lhdcBT_free_handle(handle: HANDLE_LHDC_BT);

        pub fn lhdcBT_init_encoder(
            handle: HANDLE_LHDC_BT,
            sampling_freq: c_int,   // 44100, 48000, 96000, 192000
            bit_per_sample: c_int,  // 16, 24, 32
            bitrate_inx: c_int,     // LhdcBitrateIndex
            dual_channel: c_int,    // 0 = Stereo, 1 = Dual Mono
            need_padding: c_int,    // 0
            mtu_size: c_int,        // Negotiated L2CAP MTU (e.g. 672 - 1005)
            interval: c_int,        // Frame interval in ms (typically 5 or 10)
        ) -> c_int;

        pub fn lhdcBT_encode(
            handle: HANDLE_LHDC_BT,
            in_pcm: *mut c_void,
            out_buf: *mut c_uchar,
            out_len: *mut c_uint,
            out_frames: *mut c_uint,
        ) -> c_int;

        pub fn lhdcBT_set_bitrate(handle: HANDLE_LHDC_BT, bitrate_inx: c_int) -> c_int;
        pub fn lhdcBT_get_bitrate(handle: HANDLE_LHDC_BT) -> c_int;
        pub fn lhdcBT_get_block_Size(handle: HANDLE_LHDC_BT) -> c_int; // Samples per channel per block
        pub fn lhdcBT_adjust_bitrate(handle: HANDLE_LHDC_BT, queue_len: c_uint) -> c_int;
        pub fn lhdcBT_set_hasMinBitrateLimit(handle: HANDLE_LHDC_BT, enabled: c_int) -> c_int;
        pub fn lhdcBT_set_max_bitrate(handle: HANDLE_LHDC_BT, max_rate_inx: c_int) -> c_int;
        pub fn lhdcBT_set_lossless_enable(handle: HANDLE_LHDC_BT, enabled: c_int) -> c_int;

        // --------------------------------------------------------------------
        // Sony LDAC API (from AOSP `platform/external/libldac`)
        // --------------------------------------------------------------------
        pub fn ldacBT_get_handle() -> HANDLE_LDAC_BT;
        pub fn ldacBT_free_handle(handle: HANDLE_LDAC_BT);
        pub fn ldacBT_init_handle_encode(
            handle: HANDLE_LDAC_BT,
            mtu: c_int,
            eqmid: c_int, // 0 = HQ (990k), 1 = SQ (660k), 2 = MQ (330k)
            cm: c_int,    // 0x01 = Stereo
            fmt: c_int,   // 0x04 = S24_LE, 0x02 = S16_LE
            sf: c_int,    // 44100, 48000, 96000
        ) -> c_int;
        pub fn ldacBT_encode(
            handle: HANDLE_LDAC_BT,
            p_pcm: *mut c_void,
            pcm_used: *mut c_int,
            p_stream: *mut c_uchar,
            stream_sz: *mut c_int,
            frame_num: *mut c_int,
        ) -> c_int;
        pub fn ldacBT_alter_eqmid_priority(handle: HANDLE_LDAC_BT, priority: c_int) -> c_int;

        // --------------------------------------------------------------------
        // Qualcomm aptX / aptX HD API (from `libopenaptx`)
        // --------------------------------------------------------------------
        pub fn aptx_init(hd: c_int) -> *mut c_void;
        pub fn aptx_finish(ctx: *mut c_void);
        pub fn aptx_encode(
            ctx: *mut c_void,
            input: *const c_uchar,
            input_size: usize,
            output: *mut c_uchar,
            output_size: usize,
            written: *mut usize,
        ) -> usize;
    }
}

// ============================================================================
// 2. UNIFIED CODEC ENGINE TRAIT
// ============================================================================
pub trait CodecEngine: Send {
    /// Name and current operating parameters for System Tray telemetry
    fn codec_status_name(&self) -> String;

    /// Generates the AVDTP_SET_CONFIGURATION struct sent to `universal_a2dp.sys`
    fn build_avdtp_config(&self, acp_seid: u8) -> AvdtpSetConfigRequest;

    /// Returns the exact PCM byte chunk size required per `encode_pcm` invocation
    fn required_pcm_chunk_bytes(&self) -> usize;

    /// Encodes a slice of interleaved PCM into a ready-to-send L2CAP payload (with RTP + Codec headers)
    fn encode_pcm(&mut self, pcm_bytes: &[u8], out_l2cap: &mut [u8]) -> Result<usize, &'static str>;

    /// Feeds kernel L2CAP transmit queue depth back into the encoder for Auto-Bitrate (ABR)
    fn report_tx_queue_depth(&mut self, pending_packets: u32);

    /// Allows real-time bitrate/mode switching from user space
    fn set_quality_mode(&mut self, tier: u8) -> Result<(), &'static str>;
}

// ============================================================================
// 3. PRIMARY ENGINE: SAVITECH LHDC v5 (24-bit / up to 192kHz + LL + Lossless)
// ============================================================================

/// Parsed capability flags advertised by LHDC v5 headphones during `AVDTP_GET_ALL_CAPABILITIES`
#[derive(Debug, Clone, Copy)]
pub struct LhdcV5SinkCapabilities {
    pub supports_44_1khz: bool,
    pub supports_48khz: bool,
    pub supports_96khz: bool,
    pub supports_192khz: bool,
    pub supports_16bit: bool,
    pub supports_24bit: bool,
    pub supports_low_latency: bool,
    pub supports_lossless: bool,
    pub supports_jas_hires: bool,
    pub supports_ar_3d: bool,
    pub max_bitrate_index: ffi::LhdcBitrateIndex,
}

impl LhdcV5SinkCapabilities {
    /// Parses the 11-byte Savitech LHDC v5 Vendor-Specific Capability Information Element (CIE)
    /// Byte layout inside AVDTP Media Codec Capability:
    /// [0]: Media Type (0x00)
    /// [1]: Codec Type (0xFF = Vendor)
    /// [2..6]: Vendor ID (0x3A, 0x05, 0x00, 0x00 = Savitech 0x0000053A)
    /// [6..8]: Codec ID  (0x35, 0x4C = LHDC v5 0x4C35)
    /// [8]: Sample Rate Bitmask (Bit0: 192k, Bit2: 96k, Bit4: 48k, Bit5: 44.1k)
    /// [9]: Bit Depth & Max/Min Bitrate Bitmask (Bit0: 32b, Bit1: 24b, Bit2: 16b, Bits4..6: MaxRate)
    /// [10]: Version & Feature Flags (Bit4: 5ms frame, Bit6: LL, Bit7: Lossless)
    pub fn parse_from_avdtp_cie(cie: &[u8]) -> Option<Self> {
        if cie.len() < 11 {
            return None;
        }
        let vendor_id = u32::from_le_bytes([cie[2], cie[3], cie[4], cie[5]]);
        let codec_id = u16::from_le_bytes([cie[6], cie[7]]);

        // Verify Savitech Ltd. (0x0000053A) + LHDC v5 (0x4C35)
        if vendor_id != 0x0000_053A || codec_id != 0x4C35 {
            return None;
        }

        let sr_byte = cie[8];
        let depth_rate_byte = cie[9];
        let feat_byte = cie[10];

        Some(Self {
            supports_192khz: (sr_byte & 0b0000_0001) != 0,
            supports_96khz:  (sr_byte & 0b0000_0100) != 0,
            supports_48khz:  (sr_byte & 0b0001_0000) != 0,
            supports_44_1khz:(sr_byte & 0b0010_0000) != 0,
            supports_24bit:  (depth_rate_byte & 0b0000_0010) != 0,
            supports_16bit:  (depth_rate_byte & 0b0000_0100) != 0,
            supports_low_latency: (feat_byte & 0b0100_0000) != 0,
            supports_lossless:    (feat_byte & 0b1000_0000) != 0,
            supports_jas_hires:   (feat_byte & 0b0000_0010) != 0,
            supports_ar_3d:       (feat_byte & 0b0000_0100) != 0,
            max_bitrate_index: ffi::LhdcBitrateIndex::LhdcBitrate1000K,
        })
    }
}

pub struct LhdcV5Engine {
    handle: ffi::HANDLE_LHDC_BT,
    pub sample_rate: u32,
    pub bits_per_sample: u8,
    pub mtu: usize,
    pub low_latency_mode: bool,
    pub lossless_mode: bool,
    pub current_bitrate: ffi::LhdcBitrateIndex,
    samples_per_block: usize,
    rtp_seq: u16,
    rtp_timestamp: u32,
    lhdc_latency_seq: u8,
}

unsafe impl Send for LhdcV5Engine {}

impl LhdcV5Engine {
    /// Creates and initializes the AOSP/AUR `liblhdcv5` encoder tailored to the headphone's capabilities
    pub fn new(
        caps: &LhdcV5SinkCapabilities,
        negotiated_mtu: usize,
        prefer_lossless: bool,
        prefer_low_latency: bool,
    ) -> Result<Self, &'static str> {
        // 1. Pick highest mutually supported Sample Rate & Bit Depth
        let sample_rate = if prefer_lossless && caps.supports_lossless && caps.supports_48khz {
            48000 // LHDC v5 Lossless runs at 24-bit/48kHz or 16-bit/44.1kHz
        } else if caps.supports_96khz {
            96000 // Sweet spot for LHDC v5 900/1000kbps Hi-Res
        } else if caps.supports_48khz {
            48000
        } else {
            44100
        };

        let bits_per_sample: u8 = if caps.supports_24bit { 24 } else { 16 };
        let lossless_active = prefer_lossless && caps.supports_lossless;
        let ll_active = prefer_low_latency && caps.supports_low_latency;

        // Reserve 14 bytes for RTP Header (12B) + LHDC v5 Payload Header (2B)
        let effective_mtu = negotiated_mtu.clamp(672, 1024) as c_int;

        unsafe {
            // Version 5 = LHDC v5 (AUR liblhdcv5)
            let handle = ffi::lhdcBT_get_handle(5);
            if handle.is_null() {
                return Err("lhdcBT_get_handle(5) returned NULL");
            }

            if lossless_active {
                ffi::lhdcBT_set_lossless_enable(handle, 1);
            }

            let initial_bitrate = ffi::LhdcBitrateIndex::LhdcBitrate1000K;

            // LHDC v5 uses a 5ms frame interval (interval = 5)
            let rc = ffi::lhdcBT_init_encoder(
                handle,
                sample_rate as c_int,
                bits_per_sample as c_int,
                initial_bitrate as c_int,
                0, // Stereo
                0, // No padding
                effective_mtu,
                5, // 5ms frame interval
            );

            if rc != 0 {
                ffi::lhdcBT_free_handle(handle);
                return Err("lhdcBT_init_encoder failed to initialize LHDC v5 state");
            }

            ffi::lhdcBT_set_hasMinBitrateLimit(handle, 1);
            ffi::lhdcBT_set_max_bitrate(handle, ffi::LhdcBitrateIndex::LhdcBitrate1000K as c_int);

            let block_samples = ffi::lhdcBT_get_block_Size(handle);
            let samples_per_block = if block_samples > 0 {
                block_samples as usize
            } else {
                // 5ms block = sample_rate / 200 (e.g., 480 samples per channel at 96kHz)
                (sample_rate as usize) / 200
            };

            Ok(Self {
                handle,
                sample_rate,
                bits_per_sample,
                mtu: effective_mtu as usize,
                low_latency_mode: ll_active,
                lossless_mode: lossless_active,
                current_bitrate: initial_bitrate,
                samples_per_block,
                rtp_seq: 1,
                rtp_timestamp: 0,
                lhdc_latency_seq: 0,
            })
        }
    }
}

impl CodecEngine for LhdcV5Engine {
    fn codec_status_name(&self) -> String {
        let mode_str = if self.lossless_mode {
            "Lossless"
        } else if self.low_latency_mode {
            "Low-Latency"
        } else {
            "Hi-Res Quality"
        };
        let kbps = unsafe { ffi::lhdcBT_get_bitrate(self.handle) };
        format!(
            "Savitech LHDC v5 ({}kHz / {}-bit | {} kbps | {})",
            self.sample_rate / 1000,
            self.bits_per_sample,
            kbps,
            mode_str
        )
    }

    fn build_avdtp_config(&self, acp_seid: u8) -> AvdtpSetConfigRequest {
        // Construct the exact 11-byte AVDTP Media Codec Capability for LHDC v5
        let sr_flag: u8 = match self.sample_rate {
            192000 => 0b0000_0001,
            96000  => 0b0000_0100,
            48000  => 0b0001_0000,
            _      => 0b0010_0000, // 44.1kHz
        };

        let depth_flag: u8 = match self.bits_per_sample {
            24 => 0b0000_0010,
            _  => 0b0000_0100, // 16-bit
        };
        // Max bitrate 1000kbps (0b0000_0000 in upper nibble = unlimited/max)
        let byte9 = depth_flag;

        // Byte 10: Version 5 (0x01) | 5ms frame (0x10) | Optional LL (0x40) | Optional Lossless (0x80)
        let mut byte10: u8 = 0b0001_0001;
        if self.low_latency_mode {
            byte10 |= 0b0100_0000;
        }
        if self.lossless_mode {
            byte10 |= 0b1000_0000;
        }

        let mut cap = [0u8; 16];
        cap[0..13].copy_from_slice(&[
            0x01, 0x00,                   // Media Transport Category (0x01), Length (0)
            0x07, 0x09,                   // Media Codec Category (0x07), Length (9 bytes)
            0x00,                         // Media Type: Audio (0x00)
            0xFF,                         // Codec Type: Vendor Specific (0xFF)
            0x3A, 0x05, 0x00, 0x00,       // Vendor ID: Savitech (0x0000053A LE)
            0x35, 0x4C,                   // Codec ID: LHDC v5 (0x4C35 LE)
            sr_flag,                      // Sample Rate
        ]);
        cap[13] = byte9;
        cap[14] = byte10;

        AvdtpSetConfigRequest {
            acp_seid,
            int_seid: 1,
            codec: NegotiatedCodec::LhdcV5,
            sample_rate: self.sample_rate,
            bits_per_sample: self.bits_per_sample,
            capability_len: 15,
            capability_bytes: cap,
        }
    }

    fn required_pcm_chunk_bytes(&self) -> usize {
        let bytes_per_sample = if self.bits_per_sample == 24 { 3 } else { 2 };
        // Stereo (2 channels) * samples_per_block * bytes_per_sample
        self.samples_per_block * 2 * bytes_per_sample
    }

    fn encode_pcm(
        &mut self,
        pcm_bytes: &[u8],
        out_l2cap: &mut [u8],
    ) -> Result<usize, &'static str> {
        if out_l2cap.len() < self.mtu {
            return Err("Output L2CAP buffer smaller than negotiated MTU");
        }

        let mut encoded_len: c_uint = 0;
        let mut out_frames: c_uint = 0;

        // Leave 14 bytes at the front: 12-byte RTP Header + 2-byte LHDC v5 Frame Header
        const RTP_AND_LHDC_HDR_LEN: usize = 14;

        unsafe {
            let status = ffi::lhdcBT_encode(
                self.handle,
                pcm_bytes.as_ptr() as *mut c_void,
                out_l2cap[RTP_AND_LHDC_HDR_LEN..].as_mut_ptr(),
                &mut encoded_len,
                &mut out_frames,
            );

            if status < 0 {
                return Err("lhdcBT_encode returned internal DSP error");
            }
        }

        if encoded_len == 0 || out_frames == 0 {
            // Encoder is accumulating samples for the 5ms window
            return Ok(0);
        }

        // 1. Populate 12-byte RTP v2 Media Header
        self.rtp_seq = self.rtp_seq.wrapping_add(1);
        self.rtp_timestamp = self
            .rtp_timestamp
            .wrapping_add((self.samples_per_block as u32) * out_frames);

        out_l2cap[0] = 0x80; // RTP v2, No Padding, No Extension
        out_l2cap[1] = 0x60; // Payload Type 96 (Dynamic A2DP Vendor Payload)
        out_l2cap[2..4].copy_from_slice(&self.rtp_seq.to_be_bytes());
        out_l2cap[4..8].copy_from_slice(&self.rtp_timestamp.to_be_bytes());
        out_l2cap[8..12].copy_from_slice(&0x5341_5649u32.to_be_bytes()); // SSRC ("SAVI")

        // 2. Populate 2-byte Savitech LHDC v5 A2DP Payload Header
        // Byte 12: [Bits 7..6: Latency Flag] [Bits 5..2: Frame Count] [Bits 1..0: Fragmentation]
        // Byte 13: [Sequence / Reserved 0x00]
        let frame_cnt_bits = ((out_frames as u8) & 0x0F) << 2;
        let ll_bits = if self.low_latency_mode { 0b0100_0000 } else { 0 };
        out_l2cap[12] = ll_bits | frame_cnt_bits;
        out_l2cap[13] = self.lhdc_latency_seq;
        self.lhdc_latency_seq = self.lhdc_latency_seq.wrapping_add(1);

        Ok(RTP_AND_LHDC_HDR_LEN + (encoded_len as usize))
    }

    fn report_tx_queue_depth(&mut self, pending_packets: u32) {
        // Call Savitech's native ABR (Auto Bitrate) controller based on L2CAP ACL backlog
        unsafe {
            let _ = ffi::lhdcBT_adjust_bitrate(self.handle, pending_packets);
        }
    }

    fn set_quality_mode(&mut self, tier: u8) -> Result<(), &'static str> {
        let target = match tier {
            0 => ffi::LhdcBitrateIndex::LhdcBitrateAuto,
            1 => ffi::LhdcBitrateIndex::LhdcBitrate400K,
            2 => ffi::LhdcBitrateIndex::LhdcBitrate600K,
            3 => ffi::LhdcBitrateIndex::LhdcBitrate900K,
            _ => ffi::LhdcBitrateIndex::LhdcBitrate1000K,
        };
        unsafe {
            if ffi::lhdcBT_set_bitrate(self.handle, target as c_int) == 0 {
                self.current_bitrate = target;
                Ok(())
            } else {
                Err("lhdcBT_set_bitrate rejected quality tier")
            }
        }
    }
}

impl Drop for LhdcV5Engine {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ffi::lhdcBT_free_handle(self.handle) };
            self.handle = null_mut();
        }
    }
}

// ============================================================================
// 4. SECONDARY ENGINE: SONY LDAC (24-bit / 96kHz @ 990kbps)
// ============================================================================
pub struct LdacEngine {
    handle: ffi::HANDLE_LDAC_BT,
    sample_rate: u32,
    mtu: usize,
    rtp_seq: u16,
    rtp_timestamp: u32,
}

unsafe impl Send for LdacEngine {}

impl LdacEngine {
    pub fn new(sample_rate: u32, mtu: usize) -> Result<Self, &'static str> {
        let clamped_mtu = mtu.clamp(679, 1024) as c_int;
        unsafe {
            let handle = ffi::ldacBT_get_handle();
            if handle.is_null() {
                return Err("ldacBT_get_handle failed");
            }
            // eqmid = 0 (990kbps HQ), cm = 0x01 (Stereo), fmt = 0x04 (S24_LE)
            let rc = ffi::ldacBT_init_handle_encode(
                handle,
                clamped_mtu,
                0,
                0x01,
                0x04,
                sample_rate as c_int,
            );
            if rc != 0 {
                ffi::ldacBT_free_handle(handle);
                return Err("ldacBT_init_handle_encode failed");
            }
            Ok(Self {
                handle,
                sample_rate,
                mtu: clamped_mtu as usize,
                rtp_seq: 1,
                rtp_timestamp: 0,
            })
        }
    }
}

impl CodecEngine for LdacEngine {
    fn codec_status_name(&self) -> String {
        format!("Sony LDAC ({}kHz / 24-bit | 990 kbps HQ)", self.sample_rate / 1000)
    }

    fn build_avdtp_config(&self, acp_seid: u8) -> AvdtpSetConfigRequest {
        let sr_flag = match self.sample_rate {
            96000 => 0x04,
            48000 => 0x10,
            _     => 0x20,
        };
        let mut cap = [0u8; 16];
        cap[0..12].copy_from_slice(&[
            0x01, 0x00,                   // Media Transport
            0x07, 0x08,                   // Media Codec (Length 8)
            0x00, 0xFF,                   // Audio, Vendor Specific
            0x2D, 0x01, 0x00, 0x00,       // Sony Corp (0x0000012D)
            0xAA, 0x00,                   // LDAC (0x00AA)
        ]);
        cap[12] = sr_flag;
        cap[13] = 0x01; // Stereo

        AvdtpSetConfigRequest {
            acp_seid,
            int_seid: 1,
            codec: NegotiatedCodec::Ldac,
            sample_rate: self.sample_rate,
            bits_per_sample: 24,
            capability_len: 14,
            capability_bytes: cap,
        }
    }

    fn required_pcm_chunk_bytes(&self) -> usize {
        // 128 samples * 2 channels * 3 bytes (24-bit) = 768 bytes
        128 * 2 * 3
    }

    fn encode_pcm(
        &mut self,
        pcm_bytes: &[u8],
        out_l2cap: &mut [u8],
    ) -> Result<usize, &'static str> {
        let mut pcm_used: c_int = 0;
        let mut stream_sz: c_int = 0;
        let mut frame_num: c_int = 0;

        // 12-byte RTP Header + 1-byte LDAC Frame Header = 13 bytes
        const HDR_LEN: usize = 13;

        unsafe {
            let rc = ffi::ldacBT_encode(
                self.handle,
                pcm_bytes.as_ptr() as *mut c_void,
                &mut pcm_used,
                out_l2cap[HDR_LEN..].as_mut_ptr(),
                &mut stream_sz,
                &mut frame_num,
            );
            if rc != 0 {
                return Err("ldacBT_encode failed");
            }
        }

        if stream_sz == 0 || frame_num == 0 {
            return Ok(0);
        }

        self.rtp_seq = self.rtp_seq.wrapping_add(1);
        self.rtp_timestamp = self.rtp_timestamp.wrapping_add(128 * (frame_num as u32));

        out_l2cap[0] = 0x80;
        out_l2cap[1] = 0x60;
        out_l2cap[2..4].copy_from_slice(&self.rtp_seq.to_be_bytes());
        out_l2cap[4..8].copy_from_slice(&self.rtp_timestamp.to_be_bytes());
        out_l2cap[8..12].copy_from_slice(&0x4C44_4143u32.to_be_bytes()); // "LDAC"
        out_l2cap[12] = (frame_num as u8) & 0x0F;

        Ok(HDR_LEN + (stream_sz as usize))
    }

    fn report_tx_queue_depth(&mut self, pending_packets: u32) {
        unsafe {
            if pending_packets > 12 {
                let _ = ffi::ldacBT_alter_eqmid_priority(self.handle, 1); // Step down to 660k SQ
            }
        }
    }

    fn set_quality_mode(&mut self, tier: u8) -> Result<(), &'static str> {
        let eqmid = match tier {
            1 => 2, // 330 kbps
            2 => 1, // 660 kbps
            _ => 0, // 990 kbps HQ
        };
        unsafe {
            ffi::ldacBT_alter_eqmid_priority(self.handle, eqmid);
        }
        Ok(())
    }
}

impl Drop for LdacEngine {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ffi::ldacBT_free_handle(self.handle) };
        }
    }
}

// ============================================================================
// 5. TERTIARY ENGINE: QUALCOMM aptX HD (24-bit / 48kHz @ 576kbps)
// ============================================================================
pub struct AptxHdEngine {
    ctx: *mut c_void,
    rtp_seq: u16,
    rtp_timestamp: u32,
}

unsafe impl Send for AptxHdEngine {}

impl AptxHdEngine {
    pub fn new() -> Result<Self, &'static str> {
        unsafe {
            let ctx = ffi::aptx_init(1); // 1 = aptX HD (24-bit, 6-byte codewords + RTP)
            if ctx.is_null() {
                return Err("aptx_init(1) failed");
            }
            Ok(Self {
                ctx,
                rtp_seq: 1,
                rtp_timestamp: 0,
            })
        }
    }
}

impl CodecEngine for AptxHdEngine {
    fn codec_status_name(&self) -> String {
        "Qualcomm aptX HD (48kHz / 24-bit | 576 kbps)".to_string()
    }

    fn build_avdtp_config(&self, acp_seid: u8) -> AvdtpSetConfigRequest {
        let mut cap = [0u8; 16];
        cap[0..15].copy_from_slice(&[
            0x01, 0x00,                   // Media Transport
            0x07, 0x0B,                   // Media Codec (Length 11)
            0x00, 0xFF,                   // Audio, Vendor Specific
            0xD7, 0x00, 0x00, 0x00,       // Qualcomm (0x000000D7)
            0x24, 0x00,                   // aptX HD (0x0024)
            0x12,                         // 48kHz (0x10) | Stereo (0x02)
            0x00, 0x00, 0x00, 0x00,       // Reserved bytes required by aptX HD spec
        ][..15]);

        AvdtpSetConfigRequest {
            acp_seid,
            int_seid: 1,
            codec: NegotiatedCodec::AptxHd,
            sample_rate: 48000,
            bits_per_sample: 24,
            capability_len: 15,
            capability_bytes: cap,
        }
    }

    fn required_pcm_chunk_bytes(&self) -> usize {
        // 400 stereo samples (4 samples per codeword * 100 codewords) * 6 bytes (S24_LE stereo) = 2400 bytes
        2400
    }

    fn encode_pcm(
        &mut self,
        pcm_bytes: &[u8],
        out_l2cap: &mut [u8],
    ) -> Result<usize, &'static str> {
        let mut written: usize = 0;
        const RTP_HDR_LEN: usize = 12;

        unsafe {
            let _ = ffi::aptx_encode(
                self.ctx,
                pcm_bytes.as_ptr(),
                pcm_bytes.len(),
                out_l2cap[RTP_HDR_LEN..].as_mut_ptr(),
                out_l2cap.len() - RTP_HDR_LEN,
                &mut written,
            );
        }

        if written == 0 {
            return Ok(0);
        }

        self.rtp_seq = self.rtp_seq.wrapping_add(1);
        self.rtp_timestamp = self.rtp_timestamp.wrapping_add(400);

        out_l2cap[0] = 0x80;
        out_l2cap[1] = 0x60;
        out_l2cap[2..4].copy_from_slice(&self.rtp_seq.to_be_bytes());
        out_l2cap[4..8].copy_from_slice(&self.rtp_timestamp.to_be_bytes());
        out_l2cap[8..12].copy_from_slice(&0x4150_5458u32.to_be_bytes()); // "APTX"

        Ok(RTP_HDR_LEN + written)
    }

    fn report_tx_queue_depth(&mut self, _pending_packets: u32) {}
    fn set_quality_mode(&mut self, _tier: u8) -> Result<(), &'static str> {
        Ok(())
    }
}

impl Drop for AptxHdEngine {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            unsafe { ffi::aptx_finish(self.ctx) };
        }
    }
}

// ============================================================================
// 6. AUTO-NEGOTIATOR: PICKS LHDC v5 FIRST, THEN FALLS BACK GRACEFULLY
// ============================================================================
pub struct DiscoveredSep {
    pub seid: u8,
    pub raw_capabilities: Vec<u8>,
}

/// Scans all Stream End Points (SEPs) returned by the headphones during `AVDTP_DISCOVER`
/// and initializes the highest-tier `CodecEngine` with **Savitech LHDC v5** as top priority.
pub fn select_best_codec_engine(
    discovered_seps: &[DiscoveredSep],
    negotiated_mtu: usize,
    prefer_lossless: bool,
    prefer_low_latency: bool,
) -> Result<(u8, Box<dyn CodecEngine>), &'static str> {
    // Pass 1: Look for Savitech LHDC v5 (0x053A / 0x4C35)
    for sep in discovered_seps {
        if let Some(lhdc_caps) = LhdcV5SinkCapabilities::parse_from_avdtp_cie(&sep.raw_capabilities) {
            let engine = LhdcV5Engine::new(
                &lhdc_caps,
                negotiated_mtu,
                prefer_lossless,
                prefer_low_latency,
            )?;
            return Ok((sep.seid, Box::new(engine)));
        }
    }

    // Pass 2: Look for Sony LDAC (0x012D / 0x00AA)
    for sep in discovered_seps {
        if sep.raw_capabilities.len() >= 10 {
            let vendor = u32::from_le_bytes([
                sep.raw_capabilities[2],
                sep.raw_capabilities[3],
                sep.raw_capabilities[4],
                sep.raw_capabilities[5],
            ]);
            let codec = u16::from_le_bytes([sep.raw_capabilities[6], sep.raw_capabilities[7]]);
            if vendor == 0x0000_012D && codec == 0x00AA {
                let engine = LdacEngine::new(96000, negotiated_mtu)?;
                return Ok((sep.seid, Box::new(engine)));
            }
        }
    }

    // Pass 3: Look for Qualcomm aptX HD (0x00D7 / 0x0024)
    for sep in discovered_seps {
        if sep.raw_capabilities.len() >= 10 {
            let vendor = u32::from_le_bytes([
                sep.raw_capabilities[2],
                sep.raw_capabilities[3],
                sep.raw_capabilities[4],
                sep.raw_capabilities[5],
            ]);
            let codec = u16::from_le_bytes([sep.raw_capabilities[6], sep.raw_capabilities[7]]);
            if vendor == 0x0000_00D7 && codec == 0x0024 {
                let engine = AptxHdEngine::new()?;
                return Ok((sep.seid, Box::new(engine)));
            }
        }
    }

    Err("No supported Hi-Res Vendor SEP found on target headphones")
}

/// Safe Rust wrapper around Savitech LHDC v5 C Encoder
pub struct LhdcV5Engine {
    handle: ffi::HANDLE_LHDC_BT,
    seq_number: u16,
    timestamp: u32,
}

unsafe impl Send for LhdcV5Engine {}

impl LhdcV5Engine {
    pub fn new_96khz_24bit(mtu: i32) -> Result<Self, &'static str> {
        unsafe {
            let handle = ffi::lhdcBT_get_handle(5); // Version 5
            if handle.is_null() {
                return Err("Failed to allocate LHDC v5 handle");
            }
            // Initialize: 96000Hz, 24-bit, Max Bitrate (900/1000kbps), 5ms interval
            let res = ffi::lhdcBT_init_encoder(handle, 96000, 24, 0, 0, 0, mtu, 5);
            if res != 0 {
                return Err("lhdcBT_init_encoder failed");
            }
            Ok(Self { handle, seq_number: 0, timestamp: 0 })
        }
    }
}

impl CodecEngine for LhdcV5Engine {
    fn build_avdtp_config(&self, acp_seid: u8) -> AvdtpSetConfigRequest {
        // 11-byte LHDC v5 Vendor Specific CIE
        let mut cap = [0u8; 16];
        cap[0..11].copy_from_slice(&[
            0x07, 0x09,                   // Media Codec Category (0x07), Length (9)
            0x00,                         // Media Type: Audio
            0xFF,                         // Codec Type: Vendor Specific
            0x3A, 0x05, 0x00, 0x00,       // Vendor ID: Savitech (0x0000053A)
            0x35, 0x4C,                   // Codec ID: LHDC v5 (0x4C35)
            0x04,                         // 96kHz + 24-bit flags
        ]);

        AvdtpSetConfigRequest {
            acp_seid,
            int_seid: 1,
            codec: NegotiatedCodec::LhdcV5,
            sample_rate: 96000,
            bits_per_sample: 24,
            capability_len: 11,
            capability_bytes: cap,
        }
    }

    fn encode_pcm(&mut self, pcm_bytes: &[u8], out_l2cap: &mut [u8]) -> usize {
        // Prepend 12-byte RTP Header + 2-byte LHDC Header
        let mut out_len: u32 = 0;
        let mut out_frames: u32 = 0;

        unsafe {
            let status = ffi::lhdcBT_encode(
                self.handle,
                pcm_bytes.as_ptr() as *mut _,
                out_l2cap[14..].as_mut_ptr(),
                &mut out_len,
                &mut out_frames,
            );
            if status != 0 || out_len == 0 {
                return 0;
            }
        }

        // Write RTP v2 Header (12 bytes)
        self.seq_number = self.seq_number.wrapping_add(1);
        self.timestamp = self.timestamp.wrapping_add(480); // 5ms @ 96kHz
        out_l2cap[0] = 0x80; // RTP v2
        out_l2cap[1] = 0x60; // Payload Type 96
        out_l2cap[2..4].copy_from_slice(&self.seq_number.to_be_bytes());
        out_l2cap[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        out_l2cap[8..12].copy_from_slice(&0x12345678u32.to_be_bytes()); // SSRC

        // Write 2-byte LHDC Payload Header
        out_l2cap[12] = (out_frames as u8 & 0x0F) << 2;
        out_l2cap[13] = 0x00;

        14 + (out_len as usize)
    }
}

impl Drop for LhdcV5Engine {
    fn drop(&mut self) {
        unsafe { ffi::lhdcBT_free_handle(self.handle) };
    }
}