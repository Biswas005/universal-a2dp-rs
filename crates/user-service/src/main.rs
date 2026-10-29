mod codecs;

use codecs::{select_best_codec_engine, DiscoveredSep};
use ipc_protocol::{EncodedL2capFrame, IOCTL_A2DP_FETCH_PCM_CHUNK, IOCTL_A2DP_PUSH_L2CAP_FRAME};
use std::ptr;
use windows_sys::Win32::{
    Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    System::Ioctl::DeviceIoControl,
    System::Threading::{AvSetMmThreadCharacteristicsW, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("[UniversalA2DP Service] Starting User-Space Multi-Codec Engine...");

    // 1. Open handle to our kernel driver (universal_a2dp.sys)
    let driver_path: Vec<u16> = "\\\\.\\UniversalA2dp\0".encode_utf16().collect();
    let h_driver = unsafe {
        CreateFileW(
            driver_path.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null_mut(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };

    if h_driver == INVALID_HANDLE_VALUE {
        eprintln!("[Error] Failed to connect to universal_a2dp.sys. Is the driver running?");
        return Err("Driver handle creation failed".into());
    }

    println!("[UniversalA2DP Service] Connected to Kernel Driver successfully.");

    // 2. Query discovered headphone SEPs from kernel (simulated discovery payload for illustration)
    let mock_seps = vec![DiscoveredSep {
        seid: 1,
        // Mock Savitech LHDC v5 CIE bytes advertised by headphones
        raw_capabilities: vec![0x07, 0x09, 0x00, 0xFF, 0x3A, 0x05, 0x00, 0x00, 0x35, 0x4C, 0x04, 0x02, 0x11],
    }];

    // Negotiate best codec (LHDC v5 selected as top priority)
    let negotiated_mtu = 1005; // Optimal for Bluetooth 3-DH5 packets
    let (seid, mut encoder) = select_best_codec_engine(&mock_seps, negotiated_mtu, false, false)?;

    println!("[Active Codec] Using: {}", encoder.codec_status_name());
    println!("[A2DP Stream] Bound to Headphone SEID: {}", seid);

    // 3. Elevate thread priority using MMCSS (Multimedia Class Scheduler Service)
    // This prevents Windows 11 from starving the 5ms encoding timer during heavy CPU loads.
    let task_name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut task_index: u32 = 0;
    unsafe {
        let h_task = AvSetMmThreadCharacteristicsW(task_name.as_ptr(), &mut task_index);
        if !h_task.is_null() {
            SetThreadPriority(windows_sys::Win32::System::Threading::GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            println!("[MMCSS] Thread elevated to Pro Audio / TIME_CRITICAL priority.");
        }
    }

    // 4. Real-Time 5ms Encoding Loop
    let pcm_chunk_size = encoder.required_pcm_chunk_bytes();
    let mut pcm_buffer = vec![0u8; pcm_chunk_size];
    let mut l2cap_buffer = vec![0u8; negotiated_mtu];

    println!("[Stream] Entering 5ms real-time audio loop...");

    loop {
        // Fetch raw PCM chunk from kernel driver (fed by WASAPI loopback or WaveRT)
        let mut bytes_returned: u32 = 0;
        let success = unsafe {
            DeviceIoControl(
                h_driver,
                IOCTL_A2DP_FETCH_PCM_CHUNK,
                ptr::null_mut(),
                0,
                pcm_buffer.as_mut_ptr() as *mut _,
                pcm_buffer.len() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            )
        };

        if success == 0 || bytes_returned == 0 {
            // Sleep briefly if no audio is playing to yield CPU
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }

        // Encode raw PCM into compressed LHDC v5 / LDAC packet (with RTP headers)
        match encoder.encode_pcm(&pcm_buffer[..bytes_returned as usize], &mut l2cap_buffer) {
            Ok(packet_len) if packet_len > 0 => {
                // Wrap packet for kernel IOCTL transmission
                let mut frame = EncodedL2capFrame {
                    length: packet_len as u16,
                    data: [0; 1024],
                };
                frame.data[..packet_len].copy_from_slice(&l2cap_buffer[..packet_len]);

                // Push encoded L2CAP packet down to universal_a2dp.sys for BRB_L2CA_ACL_TRANSFER dispatch
                let mut push_bytes: u32 = 0;
                unsafe {
                    DeviceIoControl(
                        h_driver,
                        IOCTL_A2DP_PUSH_L2CAP_FRAME,
                        &frame as *const _ as *const _,
                        std::mem::size_of::<EncodedL2capFrame>() as u32,
                        ptr::null_mut(),
                        0,
                        &mut push_bytes,
                        ptr::null_mut(),
                    );
                }
            }
            Ok(_) => {
                // Encoder is buffering frames for the 5ms window
            }
            Err(e) => {
                eprintln!("[Encoding Error]: {}", e);
            }
        }
    }

    unsafe { CloseHandle(h_driver) };
    Ok(())
}