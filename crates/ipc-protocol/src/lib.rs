#![no_std]

pub const IOCTL_A2DP_GET_HEADPHONE_CAPS: u32 = (0x0000001d << 16) | (0x800 << 2);
pub const IOCTL_A2DP_SET_CODEC_CONFIG: u32   = (0x0000001d << 16) | (0x801 << 2);
pub const IOCTL_A2DP_FETCH_PCM_CHUNK: u32    = (0x0000001d << 16) | (0x802 << 2);
pub const IOCTL_A2DP_PUSH_L2CAP_FRAME: u32   = (0x0000001d << 16) | (0x803 << 2);

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NegotiatedCodec {
    SbcXq      = 0,
    Aptx       = 1,
    AptxHd     = 2,
    Ldac       = 3,
    LhdcV5     = 4,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ExportedSepCapability {
    pub seid: u8,
    pub codec_type: u8,
    pub supports_delay_report: u8,
    pub cie_len: u8,
    pub vendor_id: u32,
    pub codec_id: u16,
    pub _pad: u16,
    pub cie_bytes: [u8; 24],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HeadphoneCapabilitiesResponse {
    pub bt_address: u64,
    pub negotiated_mtu: u16,
    pub active_seid: u8,
    pub sep_count: u8,
    pub seps: [ExportedSepCapability; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AvdtpSetConfigRequest {
    pub acp_seid: u8,
    pub int_seid: u8,
    pub codec: NegotiatedCodec,
    pub sample_rate: u32,
    pub bits_per_sample: u8,
    pub capability_len: u8,
    pub capability_bytes: [u8; 16],
}

#[repr(C)]
pub struct EncodedL2capFrame {
    pub length: u16,
    pub data: [u8; 1024],
}