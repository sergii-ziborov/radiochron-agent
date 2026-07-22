//! One-time foreground Location Services authorization for CoreWLAN identity.

#![allow(clashing_extern_declarations)]

use core::ffi::{c_char, c_void};
use std::time::{Duration, Instant};

type Id = *mut c_void;
type Sel = *mut c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_autoreleasePoolPush() -> Id;
    fn objc_autoreleasePoolPop(pool: Id);
    #[link_name = "objc_msgSend"]
    fn send_id(receiver: Id, selector: Sel) -> Id;
    #[link_name = "objc_msgSend"]
    fn send_void(receiver: Id, selector: Sel);
    #[link_name = "objc_msgSend"]
    fn send_isize(receiver: Id, selector: Sel) -> isize;
}

#[link(name = "CoreLocation", kind = "framework")]
extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: bool) -> i32;
}

pub fn request_authorization() -> anyhow::Result<()> {
    let pool = unsafe { objc_autoreleasePoolPush() };
    let outcome = request_inner();
    unsafe { objc_autoreleasePoolPop(pool) };
    outcome
}

fn request_inner() -> anyhow::Result<()> {
    let class = unsafe { objc_getClass(c"CLLocationManager".as_ptr()) };
    if class.is_null() {
        anyhow::bail!("CoreLocation CLLocationManager is unavailable");
    }
    let allocated = unsafe { send_id(class, selector(b"alloc\0")) };
    let manager = unsafe { send_id(allocated, selector(b"init\0")) };
    if manager.is_null() {
        anyhow::bail!("could not create CLLocationManager");
    }

    unsafe {
        send_void(manager, selector(b"requestWhenInUseAuthorization\0"));
        send_void(manager, selector(b"startUpdatingLocation\0"));
    }
    let started = Instant::now();
    let status = loop {
        let status = unsafe { send_isize(manager, selector(b"authorizationStatus\0")) };
        if status != 0 || started.elapsed() >= Duration::from_secs(60) {
            break status;
        }
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, true);
        }
    };
    unsafe {
        send_void(manager, selector(b"stopUpdatingLocation\0"));
        send_void(manager, selector(b"release\0"));
    }

    match status {
        3 | 4 => {
            println!("RadioChron has macOS Location authorization for CoreWLAN SSID/BSSID access");
            Ok(())
        }
        1 => anyhow::bail!("macOS Location Services restricted RadioChron"),
        2 => anyhow::bail!(
            "macOS Location access was denied; enable RadioChron in System Settings > Privacy & Security > Location Services"
        ),
        _ => anyhow::bail!("macOS Location authorization was not completed within 60 seconds"),
    }
}

fn selector(name: &'static [u8]) -> Sel {
    unsafe { sel_registerName(name.as_ptr().cast()) }
}
