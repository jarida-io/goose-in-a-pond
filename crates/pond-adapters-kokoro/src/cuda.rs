//! A CUDA stream of the least priority for Kokoro's work, so the language model's kernels are
//! scheduled first. The runtime is opened at run time: CPU builds and Macs never link it.

use anyhow::{anyhow, Result};
use std::ffi::c_void;

/// `cudaStreamNonBlocking`: no implicit sync with the legacy default stream llama.cpp may use.
const NON_BLOCKING: u32 = 0x01;

/// Where the CUDA runtime is found, in order; JetPack puts it under /usr/local/cuda.
const RUNTIME_NAMES: &[&str] = &[
    "libcudart.so",
    "libcudart.so.12",
    "/usr/local/cuda/lib64/libcudart.so",
];

type PriorityRange = unsafe extern "C" fn(*mut i32, *mut i32) -> i32;
type CreateWithPriority = unsafe extern "C" fn(*mut *mut c_void, u32, i32) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void) -> i32;

/// A stream owned by Kokoro's session; destroyed on drop, after the session that queues on it.
pub struct Stream {
    ptr: *mut c_void,
    destroy: Destroy,
    _runtime: libloading::Library,
    /// CUDA's priority range: larger numbers are lower priority.
    pub least: i32,
    pub greatest: i32,
}

// SAFETY: a CUDA stream handle may be used from any thread; the session serialises its runs.
unsafe impl Send for Stream {}
unsafe impl Sync for Stream {}

impl Stream {
    pub fn as_ptr(&self) -> *mut () {
        self.ptr.cast()
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: made by `cudaStreamCreateWithPriority` and destroyed once.
        unsafe { (self.destroy)(self.ptr) };
    }
}

/// A non-blocking stream at the least priority the device offers. When the least is CUDA's
/// default (0), it ties with a default-priority stream rather than yielding to it.
pub fn least_priority_stream() -> Result<Stream> {
    let runtime = RUNTIME_NAMES
        .iter()
        // SAFETY: loading the CUDA runtime runs only its own initialisers.
        .find_map(|name| unsafe { libloading::Library::new(*name) }.ok())
        .ok_or_else(|| anyhow!("no CUDA runtime library found ({RUNTIME_NAMES:?})"))?;
    // SAFETY: the signatures are the CUDA runtime API's.
    unsafe {
        let range: libloading::Symbol<PriorityRange> =
            runtime.get(b"cudaDeviceGetStreamPriorityRange\0")?;
        let create: libloading::Symbol<CreateWithPriority> =
            runtime.get(b"cudaStreamCreateWithPriority\0")?;
        let destroy: Destroy = *runtime.get::<Destroy>(b"cudaStreamDestroy\0")?;
        let (mut least, mut greatest) = (0i32, 0i32);
        let status = range(&mut least, &mut greatest);
        if status != 0 {
            return Err(anyhow!(
                "cudaDeviceGetStreamPriorityRange failed ({status})"
            ));
        }
        let mut ptr = std::ptr::null_mut();
        let status = create(&mut ptr, NON_BLOCKING, least);
        if status != 0 || ptr.is_null() {
            return Err(anyhow!("cudaStreamCreateWithPriority failed ({status})"));
        }
        tracing::info!(
            least,
            greatest,
            "Kokoro's CUDA stream made at the least priority"
        );
        Ok(Stream {
            ptr,
            destroy,
            _runtime: runtime,
            least,
            greatest,
        })
    }
}
