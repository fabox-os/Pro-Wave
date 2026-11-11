# plughost-formats

In-process VST3, Audio Unit, and CLAP hosting for [plughost](https://crates.io/crates/plughost).
The `vst3`, `au` (macOS only), and `clap` features are enabled by default.

This crate runs plugin code in the caller's process: a crashing or hanging plugin takes the
caller with it, and the caller owns native lifecycle calls and the OS event loop on the
appropriate thread. `plughost-helper` uses it inside an isolated helper process. Plugins of every
format are used through the `HostedPlugin` and `BlockProcessor` traits.

Applications normally depend on [`plughost`](https://crates.io/crates/plughost), which isolates
each plugin chain in that helper process.

Licensed under either the Apache License, Version 2.0 or the MIT license, at your option.
