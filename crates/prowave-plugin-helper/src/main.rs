//! The process Pro Wave loads plug-ins into: VST3, Audio Unit and CLAP code runs here, never
//! in the DAW. The app starts it through `plughost` and speaks to it over shared memory.

#[cfg(plugin_host)]
fn main() -> std::process::ExitCode {
    plughost_helper::run()
}

#[cfg(not(plugin_host))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "Pro Wave plug-in hosting needs macOS on Apple Silicon or Windows x64; this helper is for {}",
        std::env::consts::OS
    );
    std::process::ExitCode::FAILURE
}
