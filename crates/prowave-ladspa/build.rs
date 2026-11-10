//! Builds the tiny LADSPA plug-in the tests load, so the C ABI is exercised for real.
//!
//! `cc` only builds static libraries now, and a LADSPA plug-in must be a shared object, so this
//! invokes the platform compiler directly: clang's `-dynamiclib` on macOS, `-shared -fPIC` on
//! Linux, and `cl /LD` on MSVC. The resulting path is passed to the tests.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=tests/fixture.c");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let source = Path::new("tests/fixture.c");
    let library = if os == "windows" {
        out.join("ladspa_fixture.dll")
    } else if os == "macos" {
        out.join("libladspa_fixture.dylib")
    } else {
        out.join("libladspa_fixture.so")
    };

    let mut command = if os == "windows" && env == "msvc" {
        let mut command = Command::new(std::env::var("CC").unwrap_or_else(|_| "cl".to_string()));
        command
            .arg("/nologo")
            .arg("/LD")
            .arg("/O2")
            .arg(format!("/Fo{}", out.join("fixture.obj").display()))
            .arg(format!("/Fe{}", library.display()))
            .arg(source);
        command
    } else {
        let mut command = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".to_string()));
        if os == "macos" {
            command.arg("-dynamiclib");
        } else {
            command.arg("-shared").arg("-fPIC");
        }
        command.arg("-O2").arg("-o").arg(&library).arg(source);
        command
    };
    let compiled = command.status();
    let built = matches!(&compiled, Ok(status) if status.success());
    if built {
        println!("cargo:rustc-env=LADSPA_FIXTURE={}", library.display());
    } else if os == "windows" {
        // GitHub's Windows image has MSVC but not `cl` on PATH for a plain build script.
        // The ABI test is skipped there; the crate itself still compiles.
        println!("cargo:warning=no C compiler for the LADSPA test fixture; ABI tests are skipped");
    } else {
        panic!("the C fixture did not build: {:?}", compiled);
    }
}
