//! Creating, connecting, and tearing down one component/controller pair, and negotiating its
//! buses, in the order of the SDK's audio processor call sequence.

use std::collections::HashMap;
use std::ffi::c_void;

use vst3::Steinberg::Vst::ControllerNumbers_::kCtrlProgramChange;
use vst3::Steinberg::Vst::{
    ControllerNumbers_, IAudioProcessor, IComponent, IComponentHandler, IComponentTrait,
    IConnectionPoint, IConnectionPointTrait, IEditController, IEditControllerTrait,
    IHostApplication, IMidiMapping, IMidiMappingTrait, ParamID,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IPluginBaseTrait, IPluginFactoryTrait, TUID, kResultOk, kResultTrue,
};
use vst3::{ComPtr, ComWrapper, Interface};

use super::errors::Vst3Error;
use super::host::{ComponentHandler, ConnectionProxy, HostApplication, MemoryStream, stream_ptr};
use super::module::Module;
use super::parameter_cache::ParameterCache;

/// MIDI channels VST3 controllers can map, and the controller numbers: the 128 MIDI controllers
/// and the SDK's aftertouch (128), pitch bend (129), and program change (130).
const MIDI_CHANNELS: i16 = 16;
const MIDI_CONTROLLERS: i16 = kCtrlProgramChange as i16 + 1;

/// A parameter a controller assigns to a MIDI controller: the parameter and the controller value
/// that maps to 1.0.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MidiAssignment {
    pub id: ParamID,
    pub full_scale: f64,
}

pub(crate) struct Instance {
    pub component: ComPtr<IComponent>,
    pub processor: ComPtr<IAudioProcessor>,
    pub controller: ComPtr<IEditController>,
    pub handler: ComWrapper<ComponentHandler>,
    separate_controller: bool,
    connection: Option<Connection>,
}

struct Connection {
    component: ComPtr<IConnectionPoint>,
    controller: ComPtr<IConnectionPoint>,
    to_controller: ComWrapper<ConnectionProxy>,
    to_component: ComWrapper<ConnectionProxy>,
}

fn create<I: Interface>(module: &Module, cid: &TUID) -> Option<ComPtr<I>> {
    let mut object: *mut c_void = std::ptr::null_mut();
    let result = unsafe {
        module.factory().createInstance(
            cid.as_ptr() as FIDString,
            I::IID.as_ptr() as FIDString,
            &mut object,
        )
    };
    if result != kResultOk {
        return None;
    }
    // SAFETY: createInstance returns an owned reference to the requested interface.
    unsafe { ComPtr::from_raw(object as *mut I) }
}

// SAFETY: the instance moves into the engine mutex so the processing thread can call the processor
// and component, one call at a time. The controller is called only on the thread that created the
// plugin (the plugin's own handle to it, and prepare and drop, which run there). Pointer clones on
// both threads only change the atomic COM reference count.
unsafe impl Send for Instance {}
// SAFETY: besides the calls the engine mutex orders, the thread that created the plugin shares the
// instance only for `getState`, which VST3 lets the UI thread call while the processor processes,
// and, while the plugin is not prepared and nothing processes, for the bus, event bus and sample
// size queries. Prepared, it answers those from what it read before activating, and reads the
// tail under the engine mutex.
unsafe impl Sync for Instance {}

impl Instance {
    pub fn create(
        module: &Module,
        cid: &TUID,
        host: &ComWrapper<HostApplication>,
    ) -> Result<Instance, Vst3Error> {
        let context = host
            .as_com_ref::<IHostApplication>()
            .map_or(std::ptr::null_mut(), |h| h.as_ptr() as *mut FUnknown);
        let component: ComPtr<IComponent> =
            create(module, cid).ok_or(Vst3Error::CreateComponent)?;
        let result = unsafe { component.initialize(context) };
        if result != kResultOk {
            return Err(Vst3Error::InitializeComponent(result));
        }
        let processor = match component.cast::<IAudioProcessor>() {
            Some(processor) => processor,
            None => {
                unsafe { component.terminate() };
                return Err(Vst3Error::NoAudioProcessor);
            }
        };

        let (controller, separate_controller) = match component.cast::<IEditController>() {
            Some(controller) => (controller, false),
            None => match create_controller(module, &component, context) {
                Ok(controller) => (controller, true),
                Err(error) => {
                    unsafe { component.terminate() };
                    return Err(error);
                }
            },
        };

        let connection = separate_controller
            .then(|| {
                let component_point = component.cast::<IConnectionPoint>()?;
                let controller_point = controller.cast::<IConnectionPoint>()?;
                let to_controller = ConnectionProxy::new(controller_point.clone());
                let to_component = ConnectionProxy::new(component_point.clone());
                unsafe {
                    component_point.connect(ConnectionProxy::ptr(&to_controller));
                    controller_point.connect(ConnectionProxy::ptr(&to_component));
                }
                Some(Connection {
                    component: component_point,
                    controller: controller_point,
                    to_controller,
                    to_component,
                })
            })
            .flatten();

        // From here on, dropping the instance disconnects and terminates what was created.
        let instance = Instance {
            component,
            processor,
            controller,
            handler: ComWrapper::new(ComponentHandler::default()),
            separate_controller,
            connection,
        };
        let stream = MemoryStream::new();
        let saved = unsafe { instance.component.getState(stream_ptr(&stream)) };
        if stream.exceeded() {
            return Err(Vst3Error::StateTooLarge);
        }
        if saved == kResultOk {
            stream.rewind();
            unsafe { instance.controller.setComponentState(stream_ptr(&stream)) };
        }

        let handler_ptr = instance
            .handler
            .as_com_ref::<IComponentHandler>()
            .map_or(std::ptr::null_mut(), |h| h.as_ptr());
        unsafe { instance.controller.setComponentHandler(handler_ptr) };
        Ok(instance)
    }
}

/// The controller's MIDI controller assignments by (event bus, channel, controller number) for
/// the active event input buses. VST3 has no MIDI controller events; hosts send these as changes
/// of the assigned parameters. Call on the thread that owns the controller.
pub(crate) fn midi_assignments(
    controller: &ComPtr<IEditController>,
    parameters: &ParameterCache,
    inputs: &[bool],
) -> HashMap<(u8, u8, u8), MidiAssignment> {
    let Some(mapping) = controller.cast::<IMidiMapping>() else {
        return HashMap::new();
    };
    let mut assignments = HashMap::new();
    let buses = (0..inputs.len()).filter(|&bus| inputs[bus]);
    for (bus, channel, number) in buses.flat_map(|bus| {
        (0..MIDI_CHANNELS).flat_map(move |channel| {
            (0..MIDI_CONTROLLERS).map(move |number| (bus, channel, number))
        })
    }) {
        {
            let mut id: ParamID = 0;
            let assigned = unsafe {
                mapping.getMidiControllerAssignment(bus as i32, channel, number, &mut id)
            };
            if assigned != kResultTrue {
                continue;
            }
            // A program change maps program numbers to the steps of a discrete parameter.
            let steps = parameters
                .find(u64::from(id))
                .map_or(0, |info| info.step_count);
            let full_scale = if number == ControllerNumbers_::kPitchBend as i16 {
                16383.0
            } else if number == kCtrlProgramChange as i16 && steps > 0 {
                f64::from(steps)
            } else {
                127.0
            };
            assignments.insert(
                (bus as u8, channel as u8, number as u8),
                MidiAssignment { id, full_scale },
            );
        }
    }
    assignments
}

fn create_controller(
    module: &Module,
    component: &ComPtr<IComponent>,
    context: *mut FUnknown,
) -> Result<ComPtr<IEditController>, Vst3Error> {
    let mut controller_cid: TUID = [0; 16];
    if unsafe { component.getControllerClassId(&mut controller_cid) } != kResultOk {
        return Err(Vst3Error::NoControllerClass);
    }
    let controller: ComPtr<IEditController> =
        create(module, &controller_cid).ok_or(Vst3Error::CreateController)?;
    let result = unsafe { controller.initialize(context) };
    if result != kResultOk {
        return Err(Vst3Error::InitializeController(result));
    }
    Ok(controller)
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            self.controller.setComponentHandler(std::ptr::null_mut());
            if let Some(connection) = &self.connection {
                connection
                    .component
                    .disconnect(ConnectionProxy::ptr(&connection.to_controller));
                connection
                    .controller
                    .disconnect(ConnectionProxy::ptr(&connection.to_component));
            }
            if self.separate_controller {
                self.controller.terminate();
            }
            self.component.terminate();
        }
    }
}
