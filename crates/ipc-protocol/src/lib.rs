#![no_std]

/// Custom IOCTL codes for communicating between universal_a2dp_svc.exe and universal_a2dp.sys
/// CTL_CODE(FILE_DEVICE_SOUND, Function, METHOD_BUFFERED, FILE_ANY_ACCESS)
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

/// Sent from User Service -> Kernel Driver to configure the AVDTP stream
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AvdtpSetConfigRequest {
    pub acp_seid: u8,              // Target Headphone Stream End Point ID
    pub int_seid: u8,              // Local Initiator SEID
    pub codec: NegotiatedCodec,
    pub sample_rate: u32,          // 44100, 48000, 96000, 192000
    pub bits_per_sample: u8,       // 16, 24, 32
    pub capability_len: u8,        // Length of the capability payload below
    pub capability_bytes: [u8; 16],// Raw A2DP Vendor-Specific CIE bytes
}

/// Sent from User Service -> Kernel Driver containing an encoded L2CAP media packet
#[repr(C)]
pub struct EncodedL2capFrame {
    pub length: u16,
    pub data: [u8; 1024],          // Max L2CAP MTU payload (e.g., 672-1005 bytes)
}