#![no_std]
extern crate alloc;

#[cfg(not(test))]
extern crate wdk_panic;

#[cfg(not(test))]
use wdk_alloc::WdkAllocator;

#[cfg(not(test))]
#[global_allocator]
static GLOBAL_ALLOCATOR: WdkAllocator = WdkAllocator;

use wdk::println;
use wdk_sys::{
    ntddk::IoBuildDeviceIoControlRequest,
    call_unsafe_wdf_function_binding,
    DRIVER_OBJECT, NTSTATUS, PCUNICODE_STRING, PDEVICE_OBJECT,
    STATUS_SUCCESS, WDFDEVICE, WDFDRIVER, WDFQUEUE, WDFREQUEST,
    WDF_DRIVER_CONFIG, WDF_IO_QUEUE_CONFIG, WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES,
    _WDF_IO_QUEUE_DISPATCH_TYPE::WdfIoQueueDispatchSequential,
};
use ipc_protocol::{
    IOCTL_A2DP_FETCH_PCM_CHUNK, IOCTL_A2DP_GET_HEADPHONE_CAPS,
    IOCTL_A2DP_PUSH_L2CAP_FRAME, IOCTL_A2DP_SET_CODEC_CONFIG,
};

/// Bluetooth Internal IOCTL to submit a BRB (Bluetooth Request Block) to Bthport.sys
/// CTL_CODE(FILE_DEVICE_BLUETOOTH, 0x00, METHOD_NEITHER, FILE_ANY_ACCESS)
const IOCTL_INTERNAL_BTH_SUBMIT_BRB: u32 = 0x0041_0003;
const PSM_AVDTP: u16 = 0x0019;

/// Entry point required by Windows Kernel (DriverEntry)
#[export_name = "DriverEntry"]
pub unsafe extern "system" fn driver_entry(
    driver: &mut DRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    println!("[UniversalA2DP] Loading Rust Kernel Driver...");

    let mut driver_config = WDF_DRIVER_CONFIG {
        Size: core::mem::size_of::<WDF_DRIVER_CONFIG>() as u32,
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

/// Called by Windows PnP Manager when an A2DP Bluetooth Headphone connects
unsafe extern "C" fn evt_driver_device_add(
    _driver: WDFDRIVER,
    mut device_init: *mut wdk_sys::_WDFDEVICE_INIT,
) -> NTSTATUS {
    println!("[UniversalA2DP] A2DP Headphone detected! Initializing device...");

    let mut wdf_device: WDFDEVICE = WDF_NO_HANDLE as WDFDEVICE;
    let status = call_unsafe_wdf_function_binding!(
        WdfDeviceCreate,
        &mut device_init,
        WDF_NO_OBJECT_ATTRIBUTES,
        &mut wdf_device
    );
    if status < 0 {
        return status;
    }

    // Create Default IOCTL Queue for the User-Space Rust Codec Service
    let mut queue_config = WDF_IO_QUEUE_CONFIG {
        Size: core::mem::size_of::<WDF_IO_QUEUE_CONFIG>() as u32,
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

/// Handles IOCTLs from universal_a2dp_svc.exe (LHDC v5 / LDAC / aptX HD frames)
unsafe extern "C" fn evt_io_device_control(
    _queue: WDFQUEUE,
    request: WDFREQUEST,
    _output_buffer_length: usize,
    _input_buffer_length: usize,
    io_control_code: u32,
) {
    match io_control_code {
        IOCTL_A2DP_GET_HEADPHONE_CAPS => {
            // 1. Return discovered AVDTP SEPs from PSM 0x0019 to user service
        }
        IOCTL_A2DP_SET_CODEC_CONFIG => {
            // 2. Send AVDTP_SET_CONFIGURATION + AVDTP_OPEN + AVDTP_START over L2CAP
        }
        IOCTL_A2DP_FETCH_PCM_CHUNK => {
            // 3. Copy next 5ms of 24-bit/96kHz PCM from WaveRT cyclic DMA buffer
        }
        IOCTL_A2DP_PUSH_L2CAP_FRAME => {
            // 4. Receive encoded LHDC v5 / LDAC / aptX packet and dispatch BRB_L2CA_ACL_TRANSFER
        }
        _ => {}
    }

    call_unsafe_wdf_function_binding!(WdfRequestComplete, request, STATUS_SUCCESS as NTSTATUS);
}

IOCTL_A2DP_PUSH_L2CAP_FRAME => {
    let mut in_buf_ptr: PVOID = core::ptr::null_mut();
    let mut in_len: usize = 0;

    let status = call_unsafe_wdf_function_binding!(
        WdfRequestRetrieveInputBuffer,
        request,
        core::mem::size_of::<ipc_protocol::EncodedL2capFrame>(),
        &mut in_buf_ptr,
        &mut in_len
    );

    if status >= 0 && !in_buf_ptr.is_null() {
        let frame = &*(in_buf_ptr as *const ipc_protocol::EncodedL2capFrame);
        let payload_len = (frame.length as usize).min(frame.data.len());
        let slice = &frame.data[..payload_len];

        // Dispatch directly to Bthport.sys on the active Media L2CAP Channel
        let _ = active_connection.send_l2cap_packet(active_connection.media_channel, slice);
    }
}