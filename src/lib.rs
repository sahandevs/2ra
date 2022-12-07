use std::{
    ffi::{CStr, CString},
    sync::Arc,
};

use color_eyre::Result;
use instance::Instance;

pub mod client;
pub mod config;
pub mod gate;
pub mod instance;
pub mod message;
pub mod server;

#[no_mangle]
pub extern "C" fn lib2ra_version() -> *const i8 {
    lazy_static::lazy_static! {
        static ref VERSION: CString = CString::new(env!("CARGO_PKG_VERSION")).unwrap();
    }

    VERSION.as_ptr()
}

#[no_mangle]
pub extern "C" fn lib2ra_set_dart_send_port(
    ptr: allo_isolate::ffi::DartPostCObjectFnType,
    send_port: i64,
) {
    println!("send port {}", send_port);
    unsafe { allo_isolate::store_dart_post_cobject(ptr) };
    pretty_env_logger::formatted_builder()
        .format(move |_, record| {
            let level = CString::new(record.level().as_str())?;
            let message = CString::new(record.args().as_str().unwrap_or_default())?;
            if !allo_isolate::Isolate::new(send_port).post([level, message]) {
                println!("failed!");
            }
            return Ok(());
        })
        .parse_filters("DEBUG")
        .try_init()
        .unwrap();
    log::debug!("logger set!");
}

#[no_mangle]
pub extern "C" fn lib2ra_new_instance(config: *const char) -> *const Instance {
    fn wrapped(config: *const char) -> Result<Arc<Instance>> {
        let config = unsafe { CStr::from_ptr(config as _) };
        let cfg: config::Config = toml::from_str(config.to_str()?)?;

        Instance::new(cfg)
    }
    match wrapped(config) {
        Ok(x) => Arc::into_raw(x),
        Err(_) => 0 as _,
    }
}

#[no_mangle]
pub extern "C" fn lib2ra_start_instance(instance: *const Instance) -> u8 {
    let instance = unsafe { Arc::from_raw(instance) };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let instance_ref = instance.clone();
    rt.spawn(async move {
        instance_ref.start().await;
    });
    std::mem::forget(rt);

    let _ = Arc::into_raw(instance);
    1
}

#[no_mangle]
pub extern "C" fn lib2ra_stop_instance(instance: *const Instance) -> u8 {
    let instance = unsafe { Arc::from_raw(instance) };
    instance.shutdown_signal.notify_waiters();
    1
}
