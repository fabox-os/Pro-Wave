//! Scans installed plug-ins and prints them, for checking a machine by hand.
//!
//! `cargo run -p prowave-plugin-host --example scan`

fn main() {
    println!("helper: {:?}", prowave_plugin_host::helper_path());
    match prowave_plugin_host::scan(&[]) {
        Ok(catalog) => {
            for (format, count) in catalog.counts() {
                println!("{}: {count}", format.label());
            }
            for plugin in catalog.plugins() {
                println!(
                    "{:<4} {:<10} {:<40} {}",
                    plugin.format.badge(),
                    plugin.kind.label(),
                    plugin.name,
                    plugin.vendor
                );
            }
        }
        Err(error) => println!("scan failed: {error}"),
    }
}
