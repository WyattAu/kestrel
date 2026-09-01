//! Hello World plugin for Kestrel.
//!
//! Demonstrates the plugin API by logging a greeting on init
//! and echoing event types. This plugin compiles to `wasm32-wasi`
//! and is loaded by the Kestrel host at runtime.
//!
//! # Host imports
//!
//! The host provides these functions in the `"host"` namespace:
//! - `host_log(level, ptr, len)` — write a log message
//! - `host_alloc(len) -> ptr` — allocate memory in the plugin
//! - `host_dealloc(ptr, len)` — free allocated memory

// Host function imports (provided by the Kestrel runtime)
extern "C" {
    fn host_log(level: i32, ptr: *const u8, len: usize);
}

// Plugin exports

/// One-time initialization called by the host after loading.
///
/// # Safety
///
/// FFI export — no pointer dereferencing occurs.
#[no_mangle]
pub extern "C" fn plugin_init() {
    log(0, "Hello World plugin initialized");
}

/// Returns the plugin name as a null-terminated UTF-8 string pointer.
///
/// # Safety
///
/// Returns a pointer to a static byte string. The host must not free it.
#[no_mangle]
pub extern "C" fn plugin_name() -> *const u8 {
    b"hello-world\0".as_ptr()
}

/// Called by the host when an event occurs.
///
/// # Arguments
///
/// * `event_type` — numeric event type identifier
/// * `event_ptr` — pointer to event JSON in plugin memory
/// * `event_len` — length of the event JSON
///
/// # Safety
///
/// `event_ptr` and `event_len` are validated by the host before the call.
#[no_mangle]
pub extern "C" fn plugin_handle_event(event_type: u32, _event_ptr: u32, _event_len: u32) {
    log(0, &format!("Received event type: {event_type}"));
}

/// Graceful shutdown called by the host before unloading.
///
/// # Safety
///
/// FFI export — no pointer dereferencing occurs.
#[no_mangle]
pub extern "C" fn plugin_shutdown() {
    log(0, "Hello World plugin shutting down");
}

fn log(level: i32, message: &str) {
    // SAFETY: host_log is a host-provided function that reads the buffer
    // at (ptr, len) and does not retain the pointer after returning.
    unsafe {
        host_log(level, message.as_ptr(), message.len());
    }
}
