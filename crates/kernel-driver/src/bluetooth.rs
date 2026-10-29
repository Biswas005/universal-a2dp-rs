#![allow(non_camel_case_types, non_snake_case, dead_code)]

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr::null_mut;

use wdk::println;
use wdk_sys::{
    call_unsafe_wdf_function_binding,
    ntddk::{ExAllocatePool2, ExFreePoolWithTag, IoGetNextIrpStackLocation, KeWaitForSingleObject},
    DO_BUFFERED_IO, EVENT_INCREMENT, FALSE, IO_NO_INCREMENT, KEVENT, LIST_ENTRY, MDL, NTSTATUS,
    PIRP, POOL_FLAG_NON_PAGED, PVOID, STATUS_INSUFFICIENT_RESOURCES, STATUS_PENDING,
    STATUS_SUCCESS, ULONG, USHORT, WDFDEVICE, WDFIOTARGET, WDFREQUEST, WDF_NO_HANDLE,
    WDF_NO_OBJECT_ATTRIBUTES, WDF_REQUEST_SEND_OPTIONS,
    WDF_REQUEST_SEND_OPTIONS_FLAGS::WDF_REQUEST_SEND_OPTION_SYNCHRONOUS,
    _EVENT_TYPE::NotificationEvent,
    _KWAIT_REASON::Executive,
    _MODE::KernelMode,
};

/// Pool tag 'BthR' (Bluetooth Rust) for Non-Paged BRB allocations
const BTH_POOL_TAG: u32 = u32::from_le_bytes(*b"BthR");

/// CTL_CODE(FILE_DEVICE_BLUETOOTH (0x41), 0x00, METHOD_NEITHER (3), FILE_ANY_ACCESS (0))
pub const IOCTL_INTERNAL_BTH_SUBMIT_BRB: u32 = 0x0041_0003;

/// Protocol/Service Multiplexer (PSM) for AVDTP Signaling & Media Transport
pub const PSM_AVDTP: u16 = 0x0019;

/// L2CAP Channel Configuration Flags (from bthddi.h)
pub const CF_MTU: u32 = 0x0000_0001;
pub const CF_FLUSH_TIMEOUT: u32 = 0x0000_0002;
pub const CF_LINK_TIMEOUT: u32 = 0x0000_0010;

/// ACL Transfer Direction Flags
pub const ACL_TRANSFER_DIRECTION_OUT: u32 = 0x0000_0000;
pub const ACL_TRANSFER_DIRECTION_IN: u32 = 0x0000_0001;
pub const ACL_SHORT_TRANSFER_OK: u32 = 0x0000_0002;

/// Bluetooth Address (6-byte MAC stored in lower 48 bits of u64)
pub type BTH_ADDR = u64;
pub type L2CAP_CHANNEL_HANDLE = *mut c_void;

/// _BRB_TYPE Enumeration from bthddi.h
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BRB_TYPE {
    BRB_HCI_GET_LOCAL_BD_ADDR     = 0x0001,
    BRB_L2CA_REGISTER_SERVER      = 0x0100,
    BRB_L2CA_UNREGISTER_SERVER    = 0x0101,
    BRB_L2CA_OPEN_CHANNEL         = 0x0102,
    BRB_L2CA_OPEN_CHANNEL_RESPONSE= 0x0103,
    BRB_L2CA_CLOSE_CHANNEL        = 0x0104,
    BRB_L2CA_ACL_TRANSFER         = 0x0105,
    BRB_L2CA_UPDATE_CHANNEL       = 0x0106,
    BRB_L2CA_PING                 = 0x0107,
}

/// Callback invoked by Bthport.sys on remote disconnect or config change
pub type PFN_BTHPORT_INDICATION_CALLBACK = Option<
    unsafe extern "C" fn(
        Context: PVOID,
        Indication: u32,
        Parameters: PVOID,
    ),
>;

/// Standard header at the start of every Bluetooth Request Block (_BRB_HEADER)
#[repr(C)]
pub struct BRB_HEADER {
    pub ListEntry: LIST_ENTRY,
    pub Length: ULONG,
    pub Version: USHORT,       // BTH_DDI_VERSION (0x0200 for Windows Vista+)
    pub Type: USHORT,          // BRB_TYPE cast to u16
    pub BthportFlags: ULONG,
    pub Status: NTSTATUS,
    pub BtStatus: u8,          // BTH_ERROR (0x00 = Success)
    pub _padding: [u8; 3],
    pub ObjectReference: PVOID,
    pub ClientContext: [PVOID; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct L2CAP_CONFIG_VALUE_RANGE {
    pub Min: USHORT,
    pub Preferred: USHORT,
    pub Max: USHORT,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct L2CAP_FLOWSPEC {
    pub Flags: u8,
    pub ServiceType: u8,
    pub TokenRate: ULONG,
    pub TokenBucketSize: ULONG,
    pub PeakBandwidth: ULONG,
    pub Latency: ULONG,
    pub DelayVariation: ULONG,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct L2CAP_CONFIG_OPTION {
    pub Header: USHORT,
    pub DynamicBuffer: PVOID,
    pub FixedBuffer: [u8; 4],
    pub Flags: USHORT,
}

/// Exact x64 ABI layout for _BRB_L2CA_OPEN_CHANNEL (from bthddi.h)
#[repr(C)]
pub struct BRB_L2CA_OPEN_CHANNEL {
    pub Hdr: BRB_HEADER,
    pub ChannelHandle: L2CAP_CHANNEL_HANDLE,
    pub Response: USHORT,
    pub ResponseStatus: USHORT,
    pub ChannelFlags: ULONG,
    pub BtAddress: BTH_ADDR,
    pub Psm: USHORT,
    pub _pad0: USHORT,
    pub ConfigOut: L2CAP_CONFIG_OUT,
    pub ConfigIn: L2CAP_CONFIG_IN,
    pub CallbackFlags: ULONG,
    pub Callback: PFN_BTHPORT_INDICATION_CALLBACK,
    pub CallbackContext: PVOID,
    pub ReferenceObject: PVOID,
    pub OutResults: L2CAP_CONFIG_RESULTS,
    pub InResults: L2CAP_CONFIG_RESULTS,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct L2CAP_CONFIG_OUT {
    pub Flags: ULONG,
    pub Mtu: L2CAP_CONFIG_VALUE_RANGE,
    pub FlushTO: L2CAP_CONFIG_VALUE_RANGE,
    pub Flow: L2CAP_FLOWSPEC,
    pub LinkTO: USHORT,
    pub NumExtraOptions: ULONG,
    pub ExtraOptions: *mut L2CAP_CONFIG_OPTION,
    pub LocalQos: L2CAP_FLOWSPEC,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct L2CAP_CONFIG_IN {
    pub Flags: ULONG,
    pub Mtu: L2CAP_CONFIG_VALUE_RANGE,
    pub FlushTO: L2CAP_CONFIG_VALUE_RANGE,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct L2CAP_CONFIG_RESULTS {
    pub Flags: ULONG,
    pub Mtu: USHORT,
    pub FlushTO: USHORT,
    pub Flow: L2CAP_FLOWSPEC,
    pub LinkTO: USHORT,
    pub NumExtraOptions: ULONG,
    pub ExtraOptions: *mut L2CAP_CONFIG_OPTION,
}

/// Exact x64 ABI layout for _BRB_L2CA_ACL_TRANSFER (from bthddi.h)
#[repr(C)]
pub struct BRB_L2CA_ACL_TRANSFER {
    pub Hdr: BRB_HEADER,
    pub BtAddress: BTH_ADDR,
    pub ChannelHandle: L2CAP_CHANNEL_HANDLE,
    pub TransferFlags: ULONG,
    pub BufferSize: ULONG,
    pub Buffer: PVOID,
    pub BufferMDL: *mut MDL,
    pub Timeout: i64,
    pub RemainingBufferSize: ULONG,
}

/// Exact x64 ABI layout for _BRB_L2CA_CLOSE_CHANNEL
#[repr(C)]
pub struct BRB_L2CA_CLOSE_CHANNEL {
    pub Hdr: BRB_HEADER,
    pub BtAddress: BTH_ADDR,
    pub ChannelHandle: L2CAP_CHANNEL_HANDLE,
}

/// Helper to initialize the common _BRB_HEADER for any BRB structure
#[inline]
unsafe fn init_brb_header<T>(brb_ptr: *mut T, brb_type: BRB_TYPE) {
    core::ptr::write_bytes(brb_ptr as *mut u8, 0, size_of::<T>());
    let hdr = brb_ptr as *mut BRB_HEADER;
    (*hdr).Length = size_of::<T>() as ULONG;
    (*hdr).Version = 0x0200; // BTH_DDI_VERSION = 2.0 (Windows Vista through Windows 11)
    (*hdr).Type = brb_type as USHORT;
}

/// Submits any initialized BRB synchronously to the underlying Bthport.sys IO Target.
///
/// Key WDM/KMDF detail: Bthport.sys expects the BRB pointer inside
/// `IoGetNextIrpStackLocation(Irp)->Parameters.Others.Argument1`.
pub unsafe fn submit_brb_synchronous<T>(
    io_target: WDFIOTARGET,
    brb: &mut T,
) -> NTSTATUS {
    let mut request: WDFREQUEST = WDF_NO_HANDLE as WDFREQUEST;

    // 1. Create a KMDF Request targeting Bthport.sys
    let status = call_unsafe_wdf_function_binding!(
        WdfRequestCreate,
        WDF_NO_OBJECT_ATTRIBUTES,
        io_target,
        &mut request
    );
    if status < 0 {
        return status;
    }

    // 2. Extract the underlying WDM PIRP and populate NextStackLocation
    let irp: PIRP = call_unsafe_wdf_function_binding!(WdfRequestWdmGetIrp, request);
    let next_stack = IoGetNextIrpStackLocation(irp);

    (*next_stack).MajorFunction = wdk_sys::IRP_MJ_INTERNAL_DEVICE_CONTROL as u8;
    (*next_stack).Parameters.DeviceIoControl.IoControlCode = IOCTL_INTERNAL_BTH_SUBMIT_BRB;
    (*next_stack).Parameters.Others.Argument1 = (brb as *mut T) as PVOID;

    // 3. Format & send synchronously to Bthport.sys
    call_unsafe_wdf_function_binding!(
        WdfRequestWdmFormatUsingStackLocation,
        request,
        next_stack
    );

    let mut send_options = WDF_REQUEST_SEND_OPTIONS {
        Size: size_of::<WDF_REQUEST_SEND_OPTIONS>() as ULONG,
        Flags: WDF_REQUEST_SEND_OPTION_SYNCHRONOUS.0 as ULONG,
        Timeout: 0,
    };

    let sent = call_unsafe_wdf_function_binding!(
        WdfRequestSend,
        request,
        io_target,
        &mut send_options
    );

    let final_status = if sent == (FALSE as u8) {
        call_unsafe_wdf_function_binding!(WdfRequestGetStatus, request)
    } else {
        let hdr = (brb as *const T) as *const BRB_HEADER;
        (*hdr).Status
    };

    // 4. Clean up the WDFREQUEST handle
    call_unsafe_wdf_function_binding!(WdfObjectDelete, request as _);

    final_status
}

/// High-level wrapper to manage the two L2CAP channels required for A2DP:
/// 1. `signaling_channel` (PSM 0x0019): Used for AVDTP Discover / SetConfig / Start
/// 2. `media_channel` (PSM 0x0019): Opened immediately after AVDTP_OPEN succeeds; carries LHDC v5 / LDAC / aptX frames
pub struct L2capA2dpConnection {
    pub io_target: WDFIOTARGET,
    pub bt_address: BTH_ADDR,
    pub signaling_channel: L2CAP_CHANNEL_HANDLE,
    pub media_channel: L2CAP_CHANNEL_HANDLE,
    pub negotiated_out_mtu: u16,
}

impl L2capA2dpConnection {
    /// Opens an outbound L2CAP channel on PSM 0x0019 (AVDTP).
    /// Call once for the AVDTP Signaling Channel (e.g. `preferred_mtu = 672`),
    /// and a second time after `AVDTP_OPEN` for the high-bitrate Media Transport Channel (`preferred_mtu = 1005`).
    pub unsafe fn open_avdtp_channel(
        io_target: WDFIOTARGET,
        bt_address: BTH_ADDR,
        preferred_mtu: u16,
    ) -> Result<(L2CAP_CHANNEL_HANDLE, u16), NTSTATUS> {
        let brb_ptr = ExAllocatePool2(
            POOL_FLAG_NON_PAGED,
            size_of::<BRB_L2CA_OPEN_CHANNEL>() as u64,
            BTH_POOL_TAG,
        ) as *mut BRB_L2CA_OPEN_CHANNEL;

        if brb_ptr.is_null() {
            return Err(STATUS_INSUFFICIENT_RESOURCES);
        }

        init_brb_header(brb_ptr, BRB_TYPE::BRB_L2CA_OPEN_CHANNEL);

        let brb = &mut *brb_ptr;
        brb.BtAddress = bt_address;
        brb.Psm = PSM_AVDTP;

        // Configure Outbound MTU for high-bitrate codecs (LDAC 990kbps / LHDC v5 1000kbps)
        brb.ConfigOut.Flags = CF_MTU;
        brb.ConfigOut.Mtu = L2CAP_CONFIG_VALUE_RANGE {
            Min: 672,                 // Minimum mandatory A2DP MTU
            Preferred: preferred_mtu, // 1005 bytes for 3-DH5 optimal packing
            Max: 1024,
        };

        // Configure Inbound MTU (Signaling replies)
        brb.ConfigIn.Flags = CF_MTU;
        brb.ConfigIn.Mtu = L2CAP_CONFIG_VALUE_RANGE {
            Min: 48,
            Preferred: 672,
            Max: 1024,
        };

        brb.Callback = Some(l2cap_indication_callback);
        brb.CallbackFlags = 0xFFFF_FFFF; // Subscribe to disconnect/reconfig events

        let status = submit_brb_synchronous(io_target, brb);

        if status < 0 {
            println!("[UniversalA2DP] BRB_L2CA_OPEN_CHANNEL failed: {:#x}", status);
            ExFreePoolWithTag(brb_ptr as PVOID, BTH_POOL_TAG);
            return Err(status);
        }

        let handle = brb.ChannelHandle;
        let out_mtu = brb.OutResults.Mtu;

        println!(
            "[UniversalA2DP] Opened L2CAP PSM 0x0019! Handle: {:p}, Negotiated Outbound MTU: {}",
            handle, out_mtu
        );

        ExFreePoolWithTag(brb_ptr as PVOID, BTH_POOL_TAG);
        Ok((handle, out_mtu))
    }

    /// Transmits an encoded LHDC v5 / LDAC / aptX HD media frame (or AVDTP signaling packet)
    /// over an active L2CAP channel using `BRB_L2CA_ACL_TRANSFER`.
    pub unsafe fn send_l2cap_packet(
        &self,
        channel_handle: L2CAP_CHANNEL_HANDLE,
        payload: &[u8],
    ) -> NTSTATUS {
        if channel_handle.is_null() || payload.is_empty() {
            return wdk_sys::STATUS_INVALID_PARAMETER;
        }

        // Allocate Non-Paged Pool buffer for both the BRB and the payload so Bthport DMA is safe
        let total_alloc = size_of::<BRB_L2CA_ACL_TRANSFER>() + payload.len();
        let raw_mem = ExAllocatePool2(
            POOL_FLAG_NON_PAGED,
            total_alloc as u64,
            BTH_POOL_TAG,
        ) as *mut u8;

        if raw_mem.is_null() {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        let brb_ptr = raw_mem as *mut BRB_L2CA_ACL_TRANSFER;
        let dma_buf_ptr = raw_mem.add(size_of::<BRB_L2CA_ACL_TRANSFER>());

        init_brb_header(brb_ptr, BRB_TYPE::BRB_L2CA_ACL_TRANSFER);
        core::ptr::copy_nonoverlapping(payload.as_ptr(), dma_buf_ptr, payload.len());

        let brb = &mut *brb_ptr;
        brb.BtAddress = self.bt_address;
        brb.ChannelHandle = channel_handle;
        brb.TransferFlags = ACL_TRANSFER_DIRECTION_OUT;
        brb.BufferSize = payload.len() as ULONG;
        brb.Buffer = dma_buf_ptr as PVOID;
        brb.BufferMDL = null_mut();
        brb.Timeout = 0;

        let status = submit_brb_synchronous(self.io_target, brb);

        ExFreePoolWithTag(raw_mem as PVOID, BTH_POOL_TAG);
        status
    }

    /// Reads an incoming AVDTP signaling response (e.g. `AVDTP_DISCOVER` or `AVDTP_GET_ALL_CAPABILITIES`)
    /// from the headphones on the signaling channel.
    pub unsafe fn recv_l2cap_signaling(
        &self,
        out_buf: &mut [u8],
    ) -> Result<usize, NTSTATUS> {
        let total_alloc = size_of::<BRB_L2CA_ACL_TRANSFER>() + out_buf.len();
        let raw_mem = ExAllocatePool2(
            POOL_FLAG_NON_PAGED,
            total_alloc as u64,
            BTH_POOL_TAG,
        ) as *mut u8;

        if raw_mem.is_null() {
            return Err(STATUS_INSUFFICIENT_RESOURCES);
        }

        let brb_ptr = raw_mem as *mut BRB_L2CA_ACL_TRANSFER;
        let rx_buf_ptr = raw_mem.add(size_of::<BRB_L2CA_ACL_TRANSFER>());

        init_brb_header(brb_ptr, BRB_TYPE::BRB_L2CA_ACL_TRANSFER);

        let brb = &mut *brb_ptr;
        brb.BtAddress = self.bt_address;
        brb.ChannelHandle = self.signaling_channel;
        brb.TransferFlags = ACL_TRANSFER_DIRECTION_IN | ACL_SHORT_TRANSFER_OK;
        brb.BufferSize = out_buf.len() as ULONG;
        brb.Buffer = rx_buf_ptr as PVOID;
        brb.BufferMDL = null_mut();

        let status = submit_brb_synchronous(self.io_target, brb);
        if status < 0 {
            ExFreePoolWithTag(raw_mem as PVOID, BTH_POOL_TAG);
            return Err(status);
        }

        let bytes_read = brb.BufferSize as usize;
        core::ptr::copy_nonoverlapping(rx_buf_ptr, out_buf.as_mut_ptr(), bytes_read);

        ExFreePoolWithTag(raw_mem as PVOID, BTH_POOL_TAG);
        Ok(bytes_read)
    }

    /// Cleanly closes an L2CAP channel when headphones disconnect or driver unloads.
    pub unsafe fn close_channel(&self, channel_handle: L2CAP_CHANNEL_HANDLE) -> NTSTATUS {
        if channel_handle.is_null() {
            return STATUS_SUCCESS as NTSTATUS;
        }

        let brb_ptr = ExAllocatePool2(
            POOL_FLAG_NON_PAGED,
            size_of::<BRB_L2CA_CLOSE_CHANNEL>() as u64,
            BTH_POOL_TAG,
        ) as *mut BRB_L2CA_CLOSE_CHANNEL;

        if brb_ptr.is_null() {
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        init_brb_header(brb_ptr, BRB_TYPE::BRB_L2CA_CLOSE_CHANNEL);
        let brb = &mut *brb_ptr;
        brb.BtAddress = self.bt_address;
        brb.ChannelHandle = channel_handle;

        let status = submit_brb_synchronous(self.io_target, brb);
        ExFreePoolWithTag(brb_ptr as PVOID, BTH_POOL_TAG);
        status
    }
}

/// Callback invoked at DISPATCH_LEVEL by Bthport.sys when the headphones disconnect remotely
unsafe extern "C" fn l2cap_indication_callback(
    _context: PVOID,
    indication: u32,
    _parameters: PVOID,
) {
    println!("[UniversalA2DP] L2CAP Remote Indication Event: {:#x}", indication);
}