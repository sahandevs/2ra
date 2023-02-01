use std::ffi::OsStr;

pub fn run<I, S>(cmd: &str, args: I) -> Result<std::process::Output, std::io::Error>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    std::process::Command::new(cmd).args(args).output()
}

fn check_cargo_patch() {
    let r = run("cargo", ["patch", "--version"]).unwrap();

    if format!("{:?}", r).contains("no such subcommand: `patch`") {
        println!("cargo:warning=cargo-patch not found. installing cargo-patch");
        run("cargo", ["install", "cargo-patch"]).unwrap();
    }
}

pub fn main() {
    check_cargo_patch();
    run("cargo", ["patch"]).unwrap();
}
