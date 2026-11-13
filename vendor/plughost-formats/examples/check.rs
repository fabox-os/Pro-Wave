//! Compatibility check for one installed plugin class, run once per plugin by
//! `scripts/check-plugins.sh` so a crash stays isolated.
//!
//! usage: check <bundle.vst3|bundle.clap> [class index]
//!        check <bundle.vst3|bundle.clap> --classes
//!        check au:<Audio Unit class ID>
//!
//! `--classes` prints the number of classes in the bundle, for checking each in its own process.
//! Instruments play a note wherever effects get a test signal. The main buses keep the layouts the
//! plugin declares; an undeclared layout is tried as stereo.
//!
//! Reports: processing at six sample rates from 44.1 to 192 kHz, an edit saved immediately and
//! restored into a fresh instance, the output of that fresh instance, reset cost, and whether
//! reset clears internal audio down to the plugin's own idle output.

use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

use plughost_core::PluginRef;
use plughost_core::render::{Process, RenderOptions, Tail, TailPolicy, render};
use plughost_core::{
    AudioBusRole, AudioDirection, Event, Events, Layout, ParameterInfo, PluginFormat, PluginInfo,
    PluginKind, ProcessConfig, SampleFormat,
};
use plughost_formats::{BlockProcessor, Error, HostedPlugin};

const USAGE: &str =
    "usage: check <bundle.vst3|bundle.clap> [class index | --classes] | check au:<class ID>";
const NO_CLASS: &str = "no plugin class found for that bundle and index or class ID";
const RATES: [f64; 6] = [44_100.0, 48_000.0, 88_200.0, 96_000.0, 176_400.0, 192_000.0];
const RATE: f64 = 48_000.0;
const BLOCK: usize = 1024;
/// Plugins may store normalized values with reduced precision.
const TOLERANCE: f64 = 1e-3;
/// Output peak after reset that still counts as cleared, unless the plugin's idle output is
/// louder.
const RESIDUE_LIMIT: f64 = 1e-4;

macro_rules! out {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [bundle, flag] = args.as_slice()
        && flag == "--classes"
    {
        out!(
            "{}",
            classes(bundle).map_or(0, |(_, classes)| classes.len())
        );
        return ExitCode::SUCCESS;
    }
    let target = match args.as_slice() {
        [target] if target.starts_with("au:") => find_audio_unit(&target[3..]),
        [bundle, rest @ ..] => find_class(
            bundle,
            rest.first().and_then(|i| i.parse().ok()).unwrap_or(0),
        ),
        [] => {
            out!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some((plugin, info)) = target else {
        out!("result: FAILED {NO_CLASS}");
        return ExitCode::FAILURE;
    };
    out!(
        "class: {} | {} | {} | {} | {}",
        info.name,
        info.vendor,
        info.categories.join("|"),
        info.version,
        info.sdk_version
    );
    match check(&plugin, &info) {
        Ok(true) => {
            out!("result: PASS");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            out!("result: FAIL");
            ExitCode::FAILURE
        }
        Err(error) => {
            out!("result: FAILED {error}");
            ExitCode::FAILURE
        }
    }
}

fn classes(bundle: &str) -> Option<(PluginFormat, Vec<PluginInfo>)> {
    let path = std::path::Path::new(bundle);
    Some(if bundle.ends_with(".clap") {
        (
            PluginFormat::Clap,
            // SAFETY: this tool checks plugins in its own process; it is run on plugins its user
            // trusts.
            unsafe { plughost_formats::clap::Entry::load(path) }
                .and_then(|entry| entry.classes())
                .ok()?,
        )
    } else {
        // SAFETY: this tool checks plugins in its own process; it is run on plugins its user
        // trusts.
        let module = unsafe { plughost_formats::vst3::Module::load(path) }.ok()?;
        (PluginFormat::Vst3, module.classes())
    })
}

fn find_class(bundle: &str, index: usize) -> Option<(PluginRef, PluginInfo)> {
    let (format, classes) = classes(bundle)?;
    let info = classes.into_iter().nth(index)?;
    let class_id = info.id.class_id().as_str();
    let plugin = match format {
        PluginFormat::Vst3 => PluginRef::vst3(bundle, class_id),
        _ => PluginRef::clap(bundle, class_id),
    };
    Some((plugin, info))
}

#[cfg(target_os = "macos")]
fn find_audio_unit(class_id: &str) -> Option<(PluginRef, PluginInfo)> {
    let plugin = PluginRef::audio_unit(class_id);
    let info = plughost_formats::au::components()
        .into_iter()
        .find(|info| &info.id == plugin.id())?;
    Some((plugin, info))
}

#[cfg(not(target_os = "macos"))]
fn find_audio_unit(_class_id: &str) -> Option<(PluginRef, PluginInfo)> {
    None
}

/// Drives a hosted plugin through the shared render rules.
struct Driver {
    plugin: Box<dyn HostedPlugin>,
    processor: Box<dyn BlockProcessor>,
    config: ProcessConfig,
}

impl Driver {
    fn load(plugin: &PluginRef) -> Result<Driver, Error> {
        // SAFETY: this tool checks plugins in its own process; it is run on plugins its user
        // trusts.
        let plugin =
            unsafe { plughost_formats::load(plugin, &plughost_core::HostIdentity::default()) }?;
        let processor = plugin.processor();
        Ok(Driver {
            plugin,
            processor,
            config: config(RATE, Layout::None, Layout::Stereo),
        })
    }

    fn prepare(&mut self, config: ProcessConfig) -> Result<(), Error> {
        self.plugin.prepare(&config)?;
        self.config = config;
        Ok(())
    }
}

impl Process<f32> for Driver {
    type Error = Error;

    fn sample_rate(&self) -> f64 {
        self.config.sample_rate
    }

    fn max_block_size(&self) -> usize {
        self.config.max_block_size
    }

    fn input_channels(&self) -> usize {
        self.config.input.channels()
    }

    fn output_channels(&self) -> usize {
        self.config.output.channels()
    }

    fn latency(&self) -> Result<u32, Error> {
        self.plugin.timing().map(|timing| timing.latency)
    }

    fn tail(&self) -> Result<Tail, Error> {
        self.plugin.timing().map(|timing| timing.tail)
    }

    fn validate_automation(
        &mut self,
        automation: &[plughost_core::AutomationEvent],
    ) -> Result<(), Error> {
        let parameters = self.plugin.parameters();
        for event in automation {
            let error = |error| Error::from(plughost_core::RenderError::Input(error));
            if event.slot != 0 {
                return Err(error(plughost_core::InputError::Slot));
            }
            parameters
                .iter()
                .find(|(p, _)| p.id == event.change.id)
                .ok_or_else(|| {
                    error(plughost_core::InputError::UnknownParameter {
                        id: event.change.id,
                    })
                })?
                .0
                .automation_value(event.change.value)
                .map_err(error)?;
        }
        Ok(())
    }

    fn process(
        &mut self,
        context: &plughost_core::BlockContext,
        input: &[&[f32]],
        output: &mut [&mut [f32]],
        automation: &[plughost_core::AutomationEvent],
        events: &Events,
        produced: &mut Events,
    ) -> Result<(), Error> {
        if automation.iter().any(|event| event.slot != 0) {
            return Err(plughost_formats::Error::from(
                plughost_core::RenderError::Input(plughost_core::InputError::Slot),
            ));
        }
        let changes: Vec<_> = automation.iter().map(|event| event.change).collect();
        self.processor
            .process_audio_f32(context, input, output, &changes, events, produced)
    }
}

/// Main input and output layouts.
type Buses = (Layout, Layout);

fn config(sample_rate: f64, input: Layout, output: Layout) -> ProcessConfig {
    ProcessConfig::new(sample_rate, BLOCK, SampleFormat::F32, input, output)
}

/// The layouts of the plugin's main buses, as it declares them before preparation.
fn main_buses(plugin: &mut dyn HostedPlugin, info: &PluginInfo) -> Result<Buses, Error> {
    let buses = plugin.audio_buses()?;
    let main = |direction| {
        buses
            .iter()
            .find(|bus| bus.direction == direction && bus.role == AudioBusRole::Main)
            .map(|bus| bus.layout.unwrap_or(Layout::Stereo))
    };
    let input = match (main(AudioDirection::Input), info.kind) {
        (_, PluginKind::Instrument) => Layout::None,
        (Some(layout), _) => layout,
        (None, _) => Layout::Stereo,
    };
    Ok((
        input,
        main(AudioDirection::Output).unwrap_or(Layout::Stereo),
    ))
}

fn check(plugin: &PluginRef, info: &PluginInfo) -> Result<bool, Error> {
    let start = Instant::now();
    let mut driver = Driver::load(plugin)?;
    out!("create: {:.0} ms", start.elapsed().as_secs_f64() * 1000.0);
    let buses = main_buses(driver.plugin.as_mut(), info)?;
    let (input, output) = buses;
    out!("main buses: {input:?} in, {output:?} out");
    let mut passed = true;
    // Some plugins make sound of their own with no input (analog noise emulation); a fresh
    // instance's output of silence is the floor that reset residue is judged against.
    driver.prepare(config(RATE, input, output))?;
    let idle = peak(&render_silence(&mut driver, input.channels(), 0.5)?);
    out!("idle output peak {idle:.3e}");
    for rate in RATES {
        let outcome = driver
            .prepare(config(rate, input, output))
            .and_then(|()| render_signal(&mut driver, rate, 1.0));
        match outcome {
            // Rendering fails on NaN or infinite output, so what it returns is finite.
            Ok(output) => {
                out!(
                    "rate {rate}: ok | latency {} | tail {:?} | peak {:.4}",
                    driver.plugin.timing()?.latency,
                    driver.plugin.timing()?.tail,
                    peak(&output),
                );
            }
            Err(error) => {
                passed = false;
                out!("rate {rate}: FAILED {error}");
            }
        }
    }

    driver.prepare(config(RATE, input, output))?;
    render_signal(&mut driver, RATE, 0.25)?;
    passed &= edit_round_trip(plugin, &mut driver, buses)?;

    render_signal(&mut driver, RATE, 1.0)?;
    let start = Instant::now();
    driver.plugin.reset()?;
    let reset_ms = start.elapsed().as_secs_f64() * 1000.0;
    let residue = peak(&render_silence(&mut driver, input.channels(), 0.5)?);
    let cleared = residue <= RESIDUE_LIMIT.max(2.0 * idle);
    out!(
        "reset: {reset_ms:.0} ms | residue after reset {residue:.3e}{}",
        if cleared { "" } else { " | NOT CLEARED" }
    );
    passed &= cleared;
    Ok(passed)
}

/// Edits the first parameters a user could edit, the way an editor does, saves right away, and
/// restores into a fresh instance. Parameters the plugin does not persist are skipped.
fn edit_round_trip(plugin: &PluginRef, driver: &mut Driver, buses: Buses) -> Result<bool, Error> {
    let (input, output) = buses;
    let parameters = driver.plugin.parameters();
    let mut editable: Vec<&(ParameterInfo, Option<f64>)> = parameters
        .iter()
        .filter(|(p, _)| {
            p.flags.automatable
                && !p.flags.read_only
                && !p.flags.hidden
                && !p.flags.bypass
                && !p.flags.program_change
        })
        .collect();
    editable.sort_by_key(|(p, _)| p.step_count != 0);
    if editable.is_empty() {
        out!("edit: no editable parameter");
        return Ok(true);
    }
    for (parameter, before) in editable.into_iter().take(5) {
        let requested = if before.is_some_and(|before| before > 0.5) {
            0.2
        } else {
            0.8
        };
        let expected = match parameter.step_count {
            0 => requested,
            steps => (requested * f64::from(steps)).round() / f64::from(steps),
        };
        let unedited = driver
            .plugin
            .save_state(plughost_core::StatePurpose::Project)?;
        driver.plugin.set_parameter(parameter.id, expected)?;
        let state = driver
            .plugin
            .save_state(plughost_core::StatePurpose::Project)?;
        let mut fresh = Driver::load(plugin)?;
        fresh
            .plugin
            .restore_state(&state, plughost_core::StatePurpose::Project)?;
        // Some Audio Units list their parameters only once prepared.
        fresh.prepare(config(RATE, input, output))?;
        let restored = value(fresh.plugin.as_mut(), parameter.id);
        let kept = (restored - expected).abs() < TOLERANCE;
        driver.plugin.reset()?;
        let original = render_signal(driver, RATE, 1.0)?;
        let again = render_signal(&mut fresh, RATE, 1.0)?;
        let mut baseline = Driver::load(plugin)?;
        baseline
            .plugin
            .restore_state(&unedited, plughost_core::StatePurpose::Project)?;
        baseline.prepare(config(RATE, input, output))?;
        let unedited_output = render_signal(&mut baseline, RATE, 1.0)?;
        let audible = difference(&again, &unedited_output, 0) > 0.0;
        out!(
            "edit: \"{}\" {before:.3?} -> {expected:.3} | fresh instance value {} | output difference {:.3e} (last half {:.3e}) | {}",
            parameter.title,
            if kept { "kept" } else { "differs" },
            difference(&original, &again, 0),
            difference(&original, &again, original.first().map_or(0, Vec::len) / 2),
            if audible {
                "differs from unedited"
            } else {
                "same as unedited"
            },
        );
        // The controller value can lag; output that matches the original and differs from the
        // unedited plugin also shows the edit was restored.
        if kept || (audible && difference(&original, &again, 0) == 0.0) {
            return Ok(true);
        }
    }
    out!("edit: no edit verified");
    Ok(false)
}

fn value(plugin: &mut dyn HostedPlugin, id: u64) -> f64 {
    plugin
        .parameters()
        .into_iter()
        .find(|(p, _)| p.id == id)
        .and_then(|(_, value)| value)
        .unwrap_or(f64::NAN)
}

fn render_signal(driver: &mut Driver, rate: f64, seconds: f64) -> Result<Vec<Vec<f32>>, Error> {
    let frames = (rate * seconds) as usize;
    let signal: Vec<Vec<f32>> = (0..driver.config.input.channels())
        .map(|ch| {
            (0..frames)
                .map(|i| {
                    0.25 * ((i as f64) * (440.0 + 110.0 * ch as f64) / rate * std::f64::consts::TAU)
                        .sin() as f32
                })
                .collect()
        })
        .collect();
    // Instruments play middle C for the first half instead.
    let notes = if signal.is_empty() {
        Events::from_iter([
            Event::note_on(0, 0, 60, 100),
            Event::note_off(frames / 2, 0, 60, 0),
        ])
    } else {
        Events::new()
    };
    render_input(driver, &signal, frames, &notes)
}

fn render_silence(
    driver: &mut Driver,
    channels: usize,
    seconds: f64,
) -> Result<Vec<Vec<f32>>, Error> {
    let frames = (RATE * seconds) as usize;
    render_input(
        driver,
        &vec![vec![0.0; frames]; channels],
        frames,
        &Events::new(),
    )
}

fn render_input(
    driver: &mut Driver,
    input: &[Vec<f32>],
    frames: usize,
    events: &Events,
) -> Result<Vec<Vec<f32>>, Error> {
    let slices: Vec<&[f32]> = input.iter().map(Vec::as_slice).collect();
    let options = RenderOptions::new(TailPolicy::Reported, 0.0);
    Ok(render(driver, &slices, frames, events, &options)?.channels)
}

fn peak(channels: &[Vec<f32>]) -> f64 {
    channels
        .iter()
        .flatten()
        .map(|s| f64::from(s.abs()))
        .fold(0.0, f64::max)
}

fn difference(a: &[Vec<f32>], b: &[Vec<f32>], from: usize) -> f64 {
    a.iter()
        .zip(b)
        .flat_map(|(x, y)| {
            x[from..]
                .iter()
                .zip(&y[from..])
                .map(|(p, q)| f64::from((p - q).abs()))
        })
        .fold(0.0, f64::max)
}
