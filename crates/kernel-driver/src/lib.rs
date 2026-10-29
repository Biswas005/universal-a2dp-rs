#![no_std]
#![allow(non_camel_case_types, non_snake_case, dead_code)]

extern crate alloc;

#[cfg(not(test))]
extern crate wdk_panic;

#[cfg(not(test))]
use wdk_alloc::WdkAllocator;

#[cfg(not(test))]
#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

pub mod avdtp;
pub mod bluetooth;

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};

use wdk::println;
use wdk_sys::{
    call_unsafe_wdf_function_binding,
    ntddk::IoGetDeviceProperty,
    DRIVER_OBJECT, FALSE, NTSTATUS, PCUNICODE_STRING, PDEVICE_OBJECT, PVOID,
    STATUS_BUFFER_TOO_SMALL, STATUS_DEVICE_NOT_READY, STATUS_INVALID_DEVICE_REQUEST,
    STATUS_INVALID_PARAMETER, STATUS_SUCCESS, STATUS_UNSUCCESSFUL, ULONG, UNICODE_STRING,
    WDFCMRESLIST, WDFDEVICE, WDFDRIVER, WDFIOTARGET, WDFOBJECT, WDFQUEUE, WDFREQUEST,
    WDF_DRIVER_CONFIG, WDF_IO_QUEUE_CONFIG, WDF_MEMORY_DESCRIPTOR, WDF_NO_HANDLE,
    WDF_NO_OBJECT_ATTRIBUTES, WDF_OBJECT_ATTRIBUTES, WDF_OBJECT_CONTEXT_TYPE_INFO,
    WDF_PNPPOWER_EVENT_CALLBACKS, WDF_REQUEST_SEND_OPTIONS,
    WDF_REQUEST_SEND_OPTIONS_FLAGS::WDF_REQUEST_SEND_OPTION_SYNCHRONOUS,
    _DEVICE_REGISTRY_PROPERTY::DevicePropertyHardwareID,
    _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchSequential,
    _WDF_MEMORY_DESCRIPTOR_TYPE::WdfMemoryDescriptorTypeBuffer,
};

use avdtp::{AvdtpSession, RemoteSepDescriptor};
use bluetooth::{BTH_ADDR, L2capA2dpConnection};
use ipc_protocol::{
    AvdtpSetConfigRequest, EncodedL2capFrame, ExportedSepCapability,
    HeadphoneCapabilitiesResponse, IOCTL_A2DP_FETCH_PCM_CHUNK, IOCTL_A2DP_GET_HEADPHONE_CAPS,
    IOCTL_A2DP_PUSH_L2CAP_FRAME, IOCTL_A2DP_SET_CODEC_CONFIG,
};

// ============================================================================
// 1. BLUETOOTH DDI CONSTANTS FOR BTH_ADDR DISCOVERY
// ============================================================================

/// CTL_CODE(FILE_DEVICE_BLUETOOTH (0x41), 0x05, METHOD_BUFFERED (0), FILE_ANY_ACCESS (0))
/// Queries Bthport.sys for the remote Bluetooth device's MAC address and CoD.
const IOCTL_INTERNAL_BTH_GET_DEVICE_INFO: u32 = 0x0041_0014;

/// Exact C ABI layout of `_BTH_DEVICE_INFO` (from `bthdef.h`)
#[repr(C)]
struct BTH_DEVICE_INFO {
    pub flags: ULONG,
    pub _pad0: ULONG,
    pub address: BTH_ADDR,        // 64-bit integer holding the 48-bit Bluetooth MAC
    pub classOfDevice: ULONG,
    pub name: [u8; 248],          // BTH_MAX_NAME_SIZE
}

/// UTF-16 Device Naming & Win32 Symbolic Link (`\\.\UniversalA2dp`)
const NT_DEVICE_NAME_UTF16: &[u16] = &[
    b'\\' as u16, b'D' as u16, b'e' as u16, b'v' as u16, b'i' as u16, b'c' as u16, b'e' as u16,
    b'\\' as u16, b'U' as u16, b'n' as u16, b'i' as u16, b'v' as u16, b'e' as u16, b'r' as u16,
    b's' as u16, b'a' as u16, b'l' as u16, b'A' as u16, b'2' as u16, b'd' as u16, b'p' as u16, 0,
];

const DOS_SYMLINK_NAME_UTF16: &[u16] = &[
    b'\\' as u16, b'D' as u16, b'o' as u16, b's' as u16, b'D' as u16, b'e' as u16, b'v' as u16,
    b'i' as u16, b'c' as u16, b'e' as u16, b's' as u16, b'\\' as u16, b'U' as u16, b'n' as u16,
    b'i' as u16, b'v' as u16, b'e' as u16, b'r' as u16, b's' as u16, b'a' as u16, b'l' as u16,
    b'A' as u16, b'2' as u16, b'd' as u16, b'p' as u16, 0,
];

// ============================================================================
// 2. LOCK-FREE CYCLIC PCM RING BUFFER (96kHz / 24-bit Stereo = 576 KB/sec)
// ============================================================================

const PCM_RING_CAPACITY: usize = 65536; // 64 KB cyclic DMA staging buffer

#[repr(C)]
pub struct KernelPcmRingBuffer {
    pub write_pos: AtomicUsize,
    pub read_pos: AtomicUsize,
    pub data: [u8; PCM_RING_CAPACITY],
}

impl KernelPcmRingBuffer {
    pub fn read_chunk(&self, out_slice: &mut [u8]) -> usize {
        let write = self.write_pos.load(Ordering::Acquire);
        let read = self.read_pos.load(Ordering::Relaxed);
        let available = write.wrapping_sub(read);

        if available == 0 {
            return 0;
        }

        let to_read = available.min(out_slice.len());
        for (i, byte_slot) in out_slice[..to_read].iter_mut().enumerate() {
            let idx = (read.wrapping_add(i)) & (PCM_RING_CAPACITY - 1);
            *byte_slot = self.data[idx];
        }

        self.read_pos
            .store(read.wrapping_add(to_read), Ordering::Release);
        to_read
    }

    pub fn write_chunk(&mut self, in_slice: &[u8]) -> usize {
        let write = self.write_pos.load(Ordering::Relaxed);
        let read = self.read_pos.load(Ordering::Acquire);
        let used = write.wrapping_sub(read);
        let free = PCM_RING_CAPACITY.saturating_sub(used);

        let to_write = free.min(in_slice.len());
        for (i, &byte_val) in in_slice[..to_write].iter().enumerate() {
            let idx = (write.wrapping_add(i)) & (PCM_RING_CAPACITY - 1);
            self.data[idx] = byte_val;
        }

        self.write_pos
            .store(write.wrapping_add(to_write), Ordering::Release);
        to_write
    }
}

// ============================================================================
// 3. PER-DEVICE KMDF CONTEXT STRUCTURE
// ============================================================================

#[repr(C)]
pub struct A2dpDeviceContext {
    pub wdf_device: WDFDEVICE,
    pub io_target: WDFIOTARGET,
    pub bt_address: BTH_ADDR,
    pub l2cap_conn: L2capA2dpConnection,
    pub discovered_seps: [RemoteSepDescriptor; 8],
    pub sep_count: usize,
    pub active_seid: u8,
    pub stream_active: bool,
    pub pcm_ring: KernelPcmRingBuffer,
}

/// KMDF Type Registration for `A2dpDeviceContext`
static DEVICE_CONTEXT_TYPE_INFO: WDF_OBJECT_CONTEXT_TYPE_INFO = WDF_OBJECT_CONTEXT_TYPE_INFO {
    Size: size_of::<WDF_OBJECT_CONTEXT_TYPE_INFO>() as ULONG,
    ContextName: b"A2dpDeviceContext\0".as_ptr() as *const i8,
    ContextSize: size_of::<A2dpDeviceContext>(),
    UniqueType: &DEVICE_CONTEXT_TYPE_INFO as *const WDF_OBJECT_CONTEXT_TYPE_INFO,
    EvtDriverGetUniqueContextType: None,
};

#[inline]
unsafe fn get_device_context(device: WDFDEVICE) -> *mut A2dpDeviceContext {
    call_unsafe_wdf_function_binding!(
        WdfObjectGetTypedContextWorker,
        device as WDFOBJECT,
        &DEVICE_CONTEXT_TYPE_INFO as *const WDF_OBJECT_CONTEXT_TYPE_INFO
    ) as *mut A2dpDeviceContext
}

// ============================================================================
// 4. DRIVER ENTRY & KMDF DEVICE INITIALIZATION
// ============================================================================

#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver: &mut DRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    println!("[UniversalA2DP] Loading Rust Multi-Codec Bluetooth A2DP Kernel Driver...");

    let mut driver_config = WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as ULONG,
        EvtDriverDeviceAdd: Some(evt_driver_device_add),
        ..Default::default()
    };

    let mut wdf_driver: WDFDRIVER = WDF_NO_HANDLE as WDFDRIVER;

    call_unsafe_wdf_function_binding!(
        WdfDriverCreate,
        driver as *mut DRIVER_OBJECT,
        registry_path,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut driver_config,
        &mut wdf_driver
    )
}

unsafe extern "C" fn evt_driver_device_add(
    _driver: WDFDRIVER,
    mut device_init: *mut wdk_sys::_WDFDEVICE_INIT,
) -> NTSTATUS {
    println!("[UniversalA2DP] EvtDriverDeviceAdd: Binding to Bluetooth A2DP Sink...");

    // 1. Register PnP Hardware Prepare / Release callbacks
    let mut pnp_callbacks: WDF_PNPPOWER_EVENT_CALLBACKS = zeroed();
    pnp_callbacks.Size = size_of::<WDF_PNPPOWER_EVENT_CALLBACKS>() as ULONG;
    pnp_callbacks.EvtDevicePrepareHardware = Some(evt_device_prepare_hardware);
    pnp_callbacks.EvtDeviceReleaseHardware = Some(evt_device_release_hardware);

    call_unsafe_wdf_function_binding!(
        WdfDeviceInitSetPnpPowerEventCallbacks,
        device_init,
        &mut pnp_callbacks
    );

    // 2. Assign NT Device Name (`\Device\UniversalA2dp`)
    let nt_name = make_unicode_string(NT_DEVICE_NAME_UTF16);
    let _ = call_unsafe_wdf_function_binding!(
        WdfDeviceInitAssignName,
        device_init,
        &nt_name as *const UNICODE_STRING
    );

    // 3. Allocate `A2dpDeviceContext` alongside the WDFDEVICE
    let mut obj_attrs: WDF_OBJECT_ATTRIBUTES = zeroed();
    obj_attrs.Size = size_of::<WDF_OBJECT_ATTRIBUTES>() as ULONG;
    obj_attrs.ContextTypeInfo = &DEVICE_CONTEXT_TYPE_INFO as *const WDF_OBJECT_CONTEXT_TYPE_INFO;

    let mut wdf_device: WDFDEVICE = WDF_NO_HANDLE as WDFDEVICE;
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreate,
        &mut device_init,
        &mut obj_attrs,
        &mut wdf_device
    );
    if status < 0 {
        println!("[UniversalA2DP] WdfDeviceCreate failed: {:#x}", status);
        return status;
    }

    // 4. Create Win32 Symbolic Link (`\\.\UniversalA2dp`) for `universal_a2dp_service.exe`
    let dos_name = make_unicode_string(DOS_SYMLINK_NAME_UTF16);
    let _ = call_unsafe_wdf_function_binding!(
        WdfDeviceCreateSymbolicLink,
        wdf_device,
        &dos_name as *const UNICODE_STRING
    );

    // 5. Initialize Device Context fields
    let ctx = &mut *get_device_context(wdf_device);
    ctx.wdf_device = wdf_device;
    ctx.io_target = call_unsafe_wdf_function_binding!(WdfDeviceGetIoTarget, wdf_device);
    ctx.bt_address = 0;
    ctx.l2cap_conn = L2capA2dpConnection {
        io_target: ctx.io_target,
        bt_address: 0,
        signaling_channel: null_mut(),
        media_channel: null_mut(),
        negotiated_out_mtu: 672,
    };
    ctx.discovered_seps = [RemoteSepDescriptor::empty(); 8];
    ctx.sep_count = 0;
    ctx.active_seid = 0;
    ctx.stream_active = false;
    ctx.pcm_ring.write_pos.store(0, Ordering::Relaxed);
    ctx.pcm_ring.read_pos.store(0, Ordering::Relaxed);

    // 6. Create Default Sequential IOCTL Queue
    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: size_of::<WDF_IO_QUEUE_CONFIG>() as ULONG,
        DispatchType: WdfIoQueueDispatchSequential,
        PowerManaged: wdk_sys::_WDF_TRI_STATE::WdfTrue,
        EvtIoDeviceControl: Some(evt_io_device_control),
        ..Default::default()
    };

    let mut default_queue: WDFQUEUE = WDF_NO_HANDLE as WDFQUEUE;
    call_unsafe_wdf_function_binding!(
        WdfIoQueueCreate,
        wdf_device,
        &mut queue_config,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut default_queue
    )
}

// ============================================================================
// 5. PNP PREPARE HARDWARE: QUERY BTH_ADDR + RUN 5-STEP AVDTP HANDSHAKE
// ============================================================================

unsafe extern "C" fn evt_device_prepare_hardware(
    device: WDFDEVICE,
    _resources_raw: WDFCMRESLIST,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    let ctx = &mut *get_device_context(device);

    // 1. Discover the connected headphone's 48-bit Bluetooth MAC address (BTH_ADDR)
    let bt_addr = match query_remote_bth_addr(device, ctx.io_target) {
        Ok(addr) if addr != 0 => addr,
        _ => {
            println!("[UniversalA2DP] Failed to resolve headphone BTH_ADDR!");
            return STATUS_DEVICE_NOT_READY;
        }
    };

    ctx.bt_address = bt_addr;
    ctx.l2cap_conn.bt_address = bt_addr;
    println!(
        "[UniversalA2DP] Resolved Headphone BTH_ADDR: {:012X}",
        bt_addr & 0x0000_FFFF_FFFF_FFFF
    );

    // 2. Open L2CAP Signaling Channel on PSM 0x0019 (AVDTP)
    let (sig_handle, sig_mtu) =
        match L2capA2dpConnection::open_avdtp_channel(ctx.io_target, bt_addr, 672) {
            Ok(res) => res,
            Err(status) => {
                println!("[UniversalA2DP] Failed to open AVDTP signaling L2CAP: {:#x}", status);
                return status;
            }
        };

    ctx.l2cap_conn.signaling_channel = sig_handle;
    ctx.l2cap_conn.negotiated_out_mtu = sig_mtu;

    // 3. Run AVDTP Step 1 (DISCOVER) & Step 2 (GET_ALL_CAPABILITIES)
    let mut avdtp = AvdtpSession::new(&mut ctx.l2cap_conn);
    let sep_count = match avdtp.step_1_and_2_discover_and_probe_seps(&mut ctx.discovered_seps) {
        Ok(c) => c,
        Err(status) => {
            println!("[UniversalA2DP] AVDTP SEP discovery failed: {:#x}", status);
            return status;
        }
    };
    ctx.sep_count = sep_count;

    // 4. Auto-Start Savitech LHDC v5 immediately if the headphone advertises it
    //    (User-Space service can also reconfigure to LDAC / aptX HD via IOCTL_A2DP_SET_CODEC_CONFIG)
    if let Some(lhdc_sep) = ctx.discovered_seps[..sep_count]
        .iter()
        .copied()
        .find(|s| s.is_lhdc_v5())
    {
        println!(
            "[UniversalA2DP] Auto-negotiating Savitech LHDC v5 on SEID {}...",
            lhdc_sep.seid
        );

        let lhdc_default_cie: [u8; 11] = [
            0x00, 0xFF,                   // Audio, Vendor Specific
            0x3A, 0x05, 0x00, 0x00,       // Vendor ID: Savitech (0x0000053A)
            0x35, 0x4C,                   // Codec ID: LHDC v5 (0x4C35)
            0x04,                         // 96kHz sample rate
            0x02,                         // 24-bit depth, 1000kbps max
            0x11,                         // Version 5, 5ms frame interval
        ];

        if avdtp
            .step_3_set_configuration(
                lhdc_sep.seid,
                1,
                &lhdc_default_cie,
                lhdc_sep.supports_delay_report,
            )
            .is_ok()
            && avdtp
                .step_4_open_and_connect_media_channel(lhdc_sep.seid, 1005)
                .is_ok()
            && avdtp.step_5_start(lhdc_sep.seid).is_ok()
        {
            ctx.active_seid = lhdc_sep.seid;
            ctx.stream_active = true;
        }
    }

    STATUS_SUCCESS as NTSTATUS
}

unsafe extern "C" fn evt_device_release_hardware(
    device: WDFDEVICE,
    _resources_translated: WDFCMRESLIST,
) -> NTSTATUS {
    let ctx = &mut *get_device_context(device);
    println!("[UniversalA2DP] Releasing Bluetooth L2CAP channels...");

    if ctx.stream_active && ctx.active_seid != 0 {
        let mut avdtp = AvdtpSession::new(&mut ctx.l2cap_conn);
        let _ = avdtp.close_stream(ctx.active_seid);
        ctx.stream_active = false;
    }

    if !ctx.l2cap_conn.media_channel.is_null() {
        let _ = ctx.l2cap_conn.close_channel(ctx.l2cap_conn.media_channel);
        ctx.l2cap_conn.media_channel = null_mut();
    }

    if !ctx.l2cap_conn.signaling_channel.is_null() {
        let _ = ctx.l2cap_conn.close_channel(ctx.l2cap_conn.signaling_channel);
        ctx.l2cap_conn.signaling_channel = null_mut();
    }

    STATUS_SUCCESS as NTSTATUS
}

// ============================================================================
// 6. BTH_ADDR RESOLVER (IOCTL_INTERNAL_BTH_GET_DEVICE_INFO + PNP FALLBACK)
// ============================================================================

unsafe fn query_remote_bth_addr(
    device: WDFDEVICE,
    io_target: WDFIOTARGET,
) -> Result<BTH_ADDR, NTSTATUS> {
    // Primary Method: Query Bthport.sys directly via IOCTL_INTERNAL_BTH_GET_DEVICE_INFO
    let mut dev_info: BTH_DEVICE_INFO = zeroed();
    let mut mem_desc: WDF_MEMORY_DESCRIPTOR = zeroed();
    mem_desc.Type = WdfMemoryDescriptorTypeBuffer;
    mem_desc.u.BufferType.Buffer = (&mut dev_info as *mut BTH_DEVICE_INFO) as PVOID;
    mem_desc.u.BufferType.Length = size_of::<BTH_DEVICE_INFO>() as ULONG;

    let mut send_opts = WDF_REQUEST_SEND_OPTIONS {
        Size: size_of::<WDF_REQUEST_SEND_OPTIONS>() as ULONG,
        Flags: WDF_REQUEST_SEND_OPTION_SYNCHRONOUS.0 as ULONG,
        Timeout: 0,
    };

    let mut bytes_returned: usize = 0;
    let status = call_unsafe_wdf_function_binding!(
        WdfIoTargetSendInternalIoctlSynchronously,
        io_target,
        WDF_NO_HANDLE as WDFREQUEST,
        IOCTL_INTERNAL_BTH_GET_DEVICE_INFO,
        null_mut(),
        &mut mem_desc,
        &mut send_opts,
        &mut bytes_returned
    );

    if status >= 0 && dev_info.address != 0 {
        return Ok(dev_info.address);
    }

    // Fallback Method: Parse the 12-hex-digit MAC out of the PnP PDO Hardware ID
    // Format: `BTHENUM\{0000110b-0000-1000-8000-00805f9b34fb}_LOCALMFG&xxxx\8&xxxx&0&AABBCCDDEEFF_C00000000`
    let pdo: PDEVICE_OBJECT =
        call_unsafe_wdf_function_binding!(WdfDeviceWdmGetPhysicalDevice, device);
    if pdo.is_null() {
        return Err(STATUS_UNSUCCESSFUL);
    }

    let mut hw_id_buf = [0u16; 256];
    let mut result_len: ULONG = 0;
    let prop_status = IoGetDeviceProperty(
        pdo,
        DevicePropertyHardwareID,
        (hw_id_buf.len() * 2) as ULONG,
        hw_id_buf.as_mut_ptr() as PVOID,
        &mut result_len,
    );

    if prop_status >= 0 {
        if let Some(parsed_mac) = parse_mac_from_utf16_hwid(&hw_id_buf) {
            return Ok(parsed_mac);
        }
    }

    Err(STATUS_UNSUCCESSFUL)
}

/// Scans a UTF-16 Hardware ID buffer for a 12-character contiguous hexadecimal Bluetooth MAC
fn parse_mac_from_utf16_hwid(buf: &[u16]) -> Option<BTH_ADDR> {
    let mut hex_run = 0usize;
    let mut acc: u64 = 0;

    for &ch in buf {
        let nibble = match ch as u8 {
            b'0'..=b'9' => Some((ch as u8) - b'0'),
            b'a'..=b'f' => Some((ch as u8) - b'a' + 10),
            b'A'..=b'F' => Some((ch as u8) - b'A' + 10),
            _ => None,
        };

        if let Some(val) = nibble {
            acc = (acc << 4) | (val as u64);
            hex_run += 1;
            if hex_run == 12 && acc != 0 {
                return Some(acc & 0x0000_FFFF_FFFF_FFFF);
            }
        } else {
            hex_run = 0;
            acc = 0;
        }
    }
    None
}

// ============================================================================
// 7. COMPLETE 4-IOCTL DISPATCHER FOR USER-SPACE RUST SERVICE
// ============================================================================

unsafe extern "C" fn evt_io_device_control(
    queue: WDFQUEUE,
    request: WDFREQUEST,
    output_buffer_length: usize,
    input_buffer_length: usize,
    io_control_code: u32,
) {
    let device: WDFDEVICE = call_unsafe_wdf_function_binding!(WdfIoQueueGetDevice, queue);
    let ctx = &mut *get_device_context(device);

    let mut status: NTSTATUS = STATUS_SUCCESS as NTSTATUS;
    let mut bytes_transferred: usize = 0;

    match io_control_code {
        // --------------------------------------------------------------------
        // IOCTL 1: Return discovered Headphone SEPs, BTH_ADDR, and L2CAP MTU
        // --------------------------------------------------------------------
        IOCTL_A2DP_GET_HEADPHONE_CAPS => {
            if output_buffer_length < size_of::<HeadphoneCapabilitiesResponse>() {
                status = STATUS_BUFFER_TOO_SMALL;
            } else {
                let mut out_ptr: PVOID = null_mut();
                let mut out_len: usize = 0;
                status = call_unsafe_wdf_function_binding!(
                    WdfRequestRetrieveOutputBuffer,
                    request,
                    size_of::<HeadphoneCapabilitiesResponse>(),
                    &mut out_ptr,
                    &mut out_len
                );

                if status >= 0 && !out_ptr.is_null() {
                    let resp = &mut *(out_ptr as *mut HeadphoneCapabilitiesResponse);
                    resp.bt_address = ctx.bt_address;
                    resp.negotiated_mtu = ctx.l2cap_conn.negotiated_out_mtu;
                    resp.active_seid = ctx.active_seid;
                    resp.sep_count = ctx.sep_count as u8;

                    for i in 0..8 {
                        let src = &ctx.discovered_seps[i];
                        resp.seps[i] = ExportedSepCapability {
                            seid: src.seid,
                            codec_type: src.codec_type,
                            supports_delay_report: u8::from(src.supports_delay_report),
                            cie_len: src.media_codec_cie_len,
                            vendor_id: src.vendor_id,
                            codec_id: src.codec_id,
                            _pad: 0,
                            cie_bytes: src.media_codec_cie,
                        };
                    }

                    bytes_transferred = size_of::<HeadphoneCapabilitiesResponse>();
                }
            }
        }

        // --------------------------------------------------------------------
        // IOCTL 2: Configure or Reconfigure Active Codec (LHDC v5 / LDAC / aptX HD)
        // --------------------------------------------------------------------
        IOCTL_A2DP_SET_CODEC_CONFIG => {
            if input_buffer_length < size_of::<AvdtpSetConfigRequest>() {
                status = STATUS_BUFFER_TOO_SMALL;
            } else {
                let mut in_ptr: PVOID = null_mut();
                let mut in_len: usize = 0;
                status = call_unsafe_wdf_function_binding!(
                    WdfRequestRetrieveInputBuffer,
                    request,
                    size_of::<AvdtpSetConfigRequest>(),
                    &mut in_ptr,
                    &mut in_len
                );

                if status >= 0 && !in_ptr.is_null() {
                    let cfg = &*(in_ptr as *const AvdtpSetConfigRequest);
                    let cap_len = (cfg.capability_len as usize).min(cfg.capability_bytes.len());
                    let cap_slice = &cfg.capability_bytes[..cap_len];

                    let mut avdtp = AvdtpSession::new(&mut ctx.l2cap_conn);

                    let result = if ctx.stream_active && ctx.active_seid == cfg.acp_seid {
                        // Live Reconfigure on the same SEP (e.g. toggling LHDC v5 Low Latency)
                        avdtp.reconfigure_stream(cfg.acp_seid, cap_slice)
                    } else {
                        // Full Tear-Down & Re-Open if switching SEIDs (e.g. LDAC <-> aptX HD)
                        if ctx.stream_active && ctx.active_seid != 0 {
                            let _ = avdtp.close_stream(ctx.active_seid);
                            ctx.stream_active = false;
                        }
                        avdtp
                            .step_3_set_configuration(cfg.acp_seid, cfg.int_seid, cap_slice, true)
                            .and_then(|_| {
                                avdtp.step_4_open_and_connect_media_channel(cfg.acp_seid, 1005)
                            })
                            .and_then(|_| avdtp.step_5_start(cfg.acp_seid))
                    };

                    match result {
                        Ok(()) => {
                            ctx.active_seid = cfg.acp_seid;
                            ctx.stream_active = true;
                            status = STATUS_SUCCESS as NTSTATUS;
                        }
                        Err(err_code) => {
                            status = err_code;
                        }
                    }
                }
            }
        }

        // --------------------------------------------------------------------
        // IOCTL 3: Fetch Next 5ms Raw PCM Chunk for User-Space Encoding
        // --------------------------------------------------------------------
        IOCTL_A2DP_FETCH_PCM_CHUNK => {
            if output_buffer_length == 0 {
                status = STATUS_INVALID_PARAMETER;
            } else {
                let mut out_ptr: PVOID = null_mut();
                let mut out_len: usize = 0;
                status = call_unsafe_wdf_function_binding!(
                    WdfRequestRetrieveOutputBuffer,
                    request,
                    1,
                    &mut out_ptr,
                    &mut out_len
                );

                if status >= 0 && !out_ptr.is_null() {
                    let out_slice =
                        core::slice::from_raw_parts_mut(out_ptr as *mut u8, output_buffer_length);
                    bytes_transferred = ctx.pcm_ring.read_chunk(out_slice);
                }
            }
        }

        // --------------------------------------------------------------------
        // IOCTL 4: Push Encoded LHDC v5 / LDAC / aptX HD Frame to L2CAP Media Socket
        // --------------------------------------------------------------------
        IOCTL_A2DP_PUSH_L2CAP_FRAME => {
            if !ctx.stream_active || ctx.l2cap_conn.media_channel.is_null() {
                status = STATUS_DEVICE_NOT_READY;
            } else if input_buffer_length < size_of::<EncodedL2capFrame>() {
                status = STATUS_BUFFER_TOO_SMALL;
            } else {
                let mut in_ptr: PVOID = null_mut();
                let mut in_len: usize = 0;
                status = call_unsafe_wdf_function_binding!(
                    WdfRequestRetrieveInputBuffer,
                    request,
                    size_of::<EncodedL2capFrame>(),
                    &mut in_ptr,
                    &mut in_len
                );

                if status >= 0 && !in_ptr.is_null() {
                    let frame = &*(in_ptr as *const EncodedL2capFrame);
                    let payload_len = (frame.length as usize).min(frame.data.len());
                    let payload = &frame.data[..payload_len];

                    status = ctx
                        .l2cap_conn
                        .send_l2cap_packet(ctx.l2cap_conn.media_channel, payload);
                }
            }
        }

        _ => {
            status = STATUS_INVALID_DEVICE_REQUEST;
        }
    }

    call_unsafe_wdf_function_binding!(
        WdfRequestCompleteWithInformation,
        request,
        status,
        bytes_transferred as u64
    );
}

/// Helper to construct a Windows `UNICODE_STRING` from a null-terminated UTF-16 slice
#[inline]
const fn make_unicode_string(utf16_with_null: &'static [u16]) -> UNICODE_STRING {
    let byte_len = (utf16_with_null.len().saturating_sub(1)) * 2;
    UNICODE_STRING {
        Length: byte_len as u16,
        MaximumLength: (utf16_with_null.len() * 2) as u16,
        Buffer: utf16_with_null.as_ptr() as *mut u16,
    }
}