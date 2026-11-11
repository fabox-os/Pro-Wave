//! Builds the tiny Vamp analyser the tests load, so the C ABI is exercised for real.
//!
//! `cc` only builds static libraries now, and a Vamp plug-in must be a shared object, so this
//! invokes the platform compiler directly: clang's `-dynamiclib` on macOS, `-shared -fPIC` on
//! Linux, and `cl /LD` on MSVC. The resulting path is passed to the tests.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=tests/fixture.c");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let host = std::env::var("HOST").unwrap_or_default();
    let target = std::env::var("TARGET").unwrap_or_default();
    if host != target {
        // A fixture for the host cannot be loaded by tests for another target; skip it.
        println!("cargo:warning=cross-compiling: the Vamp ABI fixture is skipped");
        return;
    }
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let source = Path::new("tests/fixture.c");
    let library = if os == "windows" {
        out.join("vamp_fixture.dll")
    } else if os == "macos" {
        out.join("libvamp_fixture.dylib")
    } else {
        out.join("libvamp_fixture.so")
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
    match command.status() {
        Ok(status) if status.success() => {
            println!("cargo:rustc-env=VAMP_FIXTURE={}", library.display());
        }
        Ok(status) => {
            // The compiler is there but the fixture did not build: this is a real failure.
            panic!("the C fixture did not build: {status}");
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // A build without a C toolchain is fine; the ABI test is skipped.
            println!(
                "cargo:warning=no C compiler for the Vamp test fixture; ABI tests are skipped"
            );
        }
        Err(error) => panic!("cannot run the C compiler: {error}"),
    }
}
