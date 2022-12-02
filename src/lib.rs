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

pub const TEST_CFG: &str = include_str!("../my-config.toml");

#[no_mangle]
pub extern "C" fn lib2ra_start_test_server() -> i32 {
    fn wrapped() -> color_eyre::Result<tokio::runtime::Runtime> {
        let cfg: config::Config = toml::from_str(TEST_CFG)?;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        rt.spawn(async {
            start::start(cfg).await;
        });

        Ok(rt)
    }
    match wrapped() {
        Ok(rt) => {
            let rt = Box::new(rt);
            std::mem::forget(rt);
            0
        }
        Err(_) => 1,
    }
}
