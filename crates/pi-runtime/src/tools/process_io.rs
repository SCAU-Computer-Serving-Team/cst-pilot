//! Bounded post-exit pipe drain, shared by local shell and configuration commands.
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(windows)]
pub(crate) fn read_pipe<T: Read + std::os::windows::io::AsRawHandle>(
    mut pipe: T,
    stop: Arc<AtomicBool>,
    mut on_data: impl FnMut(&[u8]),
) {
    unsafe extern "system" {
        fn PeekNamedPipe(
            handle: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            remaining: *mut u32,
        ) -> i32;
    }
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::Acquire) {
        let mut available = 0u32;
        // SAFETY: the pipe remains owned here. Peek does not consume data or wait for a writer.
        let ok = unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            break;
        }
        if available == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        let take = (available as usize).min(buf.len());
        match pipe.read(&mut buf[..take]) {
            Ok(0) | Err(_) => break,
            Ok(n) => on_data(&buf[..n]),
        }
    }
}

#[cfg(unix)]
pub(crate) fn read_pipe<T: Read + std::os::fd::AsRawFd>(
    mut pipe: T,
    stop: Arc<AtomicBool>,
    mut on_data: impl FnMut(&[u8]),
) {
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::Acquire) {
        let mut pollfd = libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized descriptor, owned by pipe for the duration of poll/read.
        let ready = unsafe { libc::poll(&mut pollfd, 1, 10) };
        if ready < 0 {
            break;
        }
        if ready == 0 {
            continue;
        }
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => on_data(&buf[..n]),
        }
    }
}

pub(crate) fn finish_pipe_readers<T>(
    stop: &AtomicBool,
    handles: Vec<std::thread::JoinHandle<T>>,
) -> Vec<std::thread::Result<T>> {
    let deadline = Instant::now() + Duration::from_millis(100);
    while handles.iter().any(|h| !h.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    stop.store(true, Ordering::Release);
    handles
        .into_iter()
        .map(std::thread::JoinHandle::join)
        .collect()
}
