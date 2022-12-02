pub mod client;
pub mod config;
pub mod gate;
pub mod message;
pub mod server;
pub mod start;

#[no_mangle]
pub extern "C" fn lib2ra_test() -> i32 {
    println!("test stdout");
    17
}
