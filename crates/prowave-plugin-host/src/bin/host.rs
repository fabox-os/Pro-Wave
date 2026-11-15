//! A plug-in's own process, for the crate's tests. The app does the same from its own program.
fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    prowave_plugin_host::serve_if_requested(&args);
    eprintln!("usage: prowave-plugin-host --plugin-host <arguments>");
    std::process::exit(2);
}
