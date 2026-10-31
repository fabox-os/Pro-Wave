//! Which graphics API draws the window. By default eframe starts every API it knows (Vulkan,
//! DirectX 12 and OpenGL) and takes the fastest GPU, which costs about 100 MB of memory more
//! than needed for a 2D interface like Pro Wave's. Here:
//!
//! - macOS uses Metal, its only choice;
//! - Windows and Linux use OpenGL when this machine has a working OpenGL driver (checked before
//!   the window opens), and otherwise the system's own API (DirectX 12 / Vulkan), so a broken
//!   OpenGL driver still gets a window. Starting both would cost the memory saved;
//! - a real GPU is preferred over a software renderer;
//! - OpenGL can pass that check and still be unable to draw to the window, which only shows once
//!   the window exists (NVIDIA on Wayland, it seems). The window system can't be started twice
//!   in one process, so Pro Wave then relaunches itself on the system's own API, and on Linux,
//!   if that fails too, on X11 (under Wayland) and then on software OpenGL ([`next_relaunch`]).
//!
//! `WGPU_BACKEND` (`gl`, `dx12`, `vulkan`, `metal`) overrides the choice, for troubleshooting.
use eframe::egui_wgpu::{WgpuConfiguration, WgpuError, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;
use std::sync::Arc;

/// The system's own graphics API: the fallback when OpenGL does not work.
fn native() -> wgpu::Backends {
    if cfg!(target_os = "macos") {
        wgpu::Backends::METAL
    } else if cfg!(windows) {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::VULKAN
    }
}

/// The graphics API used on this machine: Metal on macOS; elsewhere OpenGL when `gl_works`,
/// else the system's own API, with software OpenGL kept as the last resort (a virtual machine
/// without a GPU), which [`pick`] only takes when nothing else can draw.
pub fn backends(gl_works: bool) -> wgpu::Backends {
    if cfg!(target_os = "macos") {
        native()
    } else if gl_works {
        wgpu::Backends::GL
    } else {
        native() | wgpu::Backends::GL
    }
}

fn instance_descriptor(backends: wgpu::Backends) -> wgpu::InstanceDescriptor {
    wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::from_build_config().with_env(),
        backend_options: wgpu::BackendOptions::from_env_or_default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    }
}

/// Whether an OpenGL adapter that is a real GPU can be created on this machine.
fn opengl_works() -> bool {
    let instance = wgpu::Instance::new(instance_descriptor(wgpu::Backends::GL));
    let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::GL));
    let found = adapters
        .iter()
        .any(|a| a.get_info().device_type != wgpu::DeviceType::Cpu);
    if !found {
        tracing::info!("no hardware OpenGL driver; using the system's own graphics API");
    }
    found
}

/// Runs a future that completes without waiting, as wgpu's adapter listing does on desktops.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(out) = future.as_mut().poll(&mut cx) {
            return out;
        }
        std::thread::yield_now();
    }
}

/// Sort key for an adapter: lower is better. A real GPU before a software renderer, then
/// OpenGL before the other API.
fn rank(backend: wgpu::Backend, device: wgpu::DeviceType) -> (bool, bool) {
    (
        device == wgpu::DeviceType::Cpu,
        backend != wgpu::Backend::Gl,
    )
}

fn pick(
    adapters: &[wgpu::Adapter],
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, String> {
    adapters
        .iter()
        .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
        .min_by_key(|a| {
            let info = a.get_info();
            rank(info.backend, info.device_type)
        })
        .cloned()
        .ok_or_else(|| "no graphics adapter can draw to this window".to_string())
}

/// eframe's wgpu settings for Pro Wave (see the module docs).
pub fn config() -> WgpuConfiguration {
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    if wgpu::Backends::from_env().is_none() {
        let gl_works = !cfg!(target_os = "macos") && opengl_works();
        setup.instance_descriptor.backends = backends(gl_works);
        setup.native_adapter_selector = Some(Arc::new(pick));
    }
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        ..Default::default()
    }
}

/// The APIs that `config` starts.
pub fn started_backends(config: &WgpuConfiguration) -> wgpu::Backends {
    match &config.wgpu_setup {
        WgpuSetup::CreateNew(setup) => setup.instance_descriptor.backends,
        WgpuSetup::Existing(_) => wgpu::Backends::empty(),
    }
}

/// A new run of Pro Wave with another way of reaching the screen.
#[derive(Debug, PartialEq, Eq)]
pub struct Relaunch {
    /// How many relaunches this run is, passed on in [`STEP_VAR`].
    pub step: u8,
    pub set: &'static [(&'static str, &'static str)],
    pub remove: &'static [&'static str],
}

/// Counts the relaunches, so each way is tried once and the chain ends.
pub const STEP_VAR: &str = "PROWAVE_GRAPHICS_STEP";

/// What to try after `error` stopped the window from opening, `step` relaunches in:
///
/// 1. OpenGL was refused alone: the system's own API (Vulkan on Linux);
/// 2. on Linux under Wayland: the X11 window system (XWayland), where NVIDIA's drivers are
///    the most tested, with Pro Wave choosing the API again;
/// 3. on Linux: software OpenGL, slow but it draws on any machine.
///
/// A `WGPU_BACKEND` set by the user is theirs: no step 1 then.
pub fn next_relaunch(
    step: u8,
    started: wgpu::Backends,
    error: &eframe::Error,
    wayland: bool,
    linux: bool,
) -> Option<Relaunch> {
    let wgpu_error = matches!(error, eframe::Error::Wgpu(_));
    let rejected = matches!(
        error,
        eframe::Error::Wgpu(WgpuError::CustomNativeAdapterSelectionError(_))
    );
    match step {
        0 if rejected && started == wgpu::Backends::GL => Some(Relaunch {
            step: 1,
            set: if cfg!(target_os = "macos") {
                &[("WGPU_BACKEND", "metal")]
            } else if cfg!(windows) {
                &[("WGPU_BACKEND", "dx12")]
            } else {
                &[("WGPU_BACKEND", "vulkan")]
            },
            remove: &[],
        }),
        1 if linux && wgpu_error && wayland => Some(Relaunch {
            step: 2,
            set: &[],
            remove: &["WAYLAND_DISPLAY", "WGPU_BACKEND"],
        }),
        1 | 2 if linux && wgpu_error => Some(Relaunch {
            step: 3,
            set: &[("WGPU_BACKEND", "gl"), ("LIBGL_ALWAYS_SOFTWARE", "1")],
            remove: &[],
        }),
        _ => None,
    }
}

/// Replaces this program with a new run as `how` says, with the same arguments. Returns the
/// exit code to leave with, if the new run was started in a way that needs waiting for.
pub fn relaunch(how: &Relaunch) -> std::io::Result<i32> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args(std::env::args_os().skip(1))
        .env(STEP_VAR, how.step.to_string());
    for (name, value) in how.set {
        command.env(name, value);
    }
    for name in how.remove {
        command.env_remove(name);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Only returns if it failed.
        Err(command.exec())
    }
    #[cfg(not(unix))]
    {
        Ok(command.status()?.code().unwrap_or(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::{Backend, DeviceType};

    #[test]
    fn opengl_on_a_real_gpu_is_preferred() {
        let mut options = [
            (Backend::Dx12, DeviceType::DiscreteGpu),
            (Backend::Gl, DeviceType::Cpu),
            (Backend::Gl, DeviceType::IntegratedGpu),
            (Backend::Vulkan, DeviceType::Cpu),
        ];
        options.sort_by_key(|(b, d)| rank(*b, *d));
        assert_eq!(options[0], (Backend::Gl, DeviceType::IntegratedGpu));
        assert_eq!(
            options[1],
            (Backend::Dx12, DeviceType::DiscreteGpu),
            "then the GPU"
        );
        assert_eq!(options[2].1, DeviceType::Cpu, "software rendering last");
    }

    #[test]
    fn each_system_starts_one_api_and_falls_back_without_opengl() {
        if cfg!(target_os = "macos") {
            assert_eq!(backends(true), wgpu::Backends::METAL);
            assert_eq!(backends(false), wgpu::Backends::METAL);
        } else {
            assert_eq!(backends(true), wgpu::Backends::GL);
            assert_eq!(backends(false), native() | wgpu::Backends::GL);
            assert!(!native().contains(wgpu::Backends::GL));
        }
    }

    fn rejected() -> eframe::Error {
        eframe::Error::Wgpu(WgpuError::CustomNativeAdapterSelectionError(
            "no graphics adapter can draw to this window".to_string(),
        ))
    }

    #[test]
    fn opengl_that_cannot_draw_relaunches_on_the_system_api() {
        let how = next_relaunch(0, wgpu::Backends::GL, &rejected(), true, true).unwrap();
        assert_eq!(how.step, 1);
        assert_eq!(how.set[0].0, "WGPU_BACKEND");
        assert_eq!(
            wgpu::Backends::from_comma_list(how.set[0].1),
            native(),
            "the value names the system's API"
        );
    }

    #[test]
    fn no_first_relaunch_once_the_system_api_was_tried_or_for_other_errors() {
        let all = native() | wgpu::Backends::GL;
        assert_eq!(next_relaunch(0, all, &rejected(), true, true), None);
        assert_eq!(next_relaunch(0, native(), &rejected(), true, true), None);
        let other = eframe::Error::Wgpu(WgpuError::NoSurfaceFormatsAvailable);
        assert_eq!(
            next_relaunch(0, wgpu::Backends::GL, &other, true, true),
            None
        );
    }

    #[test]
    fn linux_under_wayland_then_tries_x11_and_then_software_opengl() {
        let gl = wgpu::Backends::GL;
        let x11 = next_relaunch(1, gl, &rejected(), true, true).unwrap();
        assert_eq!(x11.step, 2);
        assert!(x11.remove.contains(&"WAYLAND_DISPLAY"));
        assert!(x11.remove.contains(&"WGPU_BACKEND"));
        let soft = next_relaunch(2, gl, &rejected(), true, true).unwrap();
        assert_eq!(soft.step, 3);
        assert!(soft.set.contains(&("LIBGL_ALWAYS_SOFTWARE", "1")));
        assert_eq!(next_relaunch(3, gl, &rejected(), true, true), None);
    }

    #[test]
    fn without_wayland_the_system_api_failing_goes_straight_to_software() {
        let soft = next_relaunch(1, wgpu::Backends::GL, &rejected(), false, true).unwrap();
        assert_eq!(soft.step, 3);
    }

    #[test]
    fn only_linux_goes_past_the_first_relaunch() {
        let none = next_relaunch(1, wgpu::Backends::GL, &rejected(), true, false);
        assert_eq!(none, None);
    }

    #[test]
    fn a_ready_future_is_returned_at_once() {
        assert_eq!(block_on(async { 7 }), 7);
    }
}
