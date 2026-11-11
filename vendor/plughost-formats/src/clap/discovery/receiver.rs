use super::{ClapError, text};
use clap_sys::factory::preset_discovery::clap_preset_discovery_metadata_receiver as Raw;
use clap_sys::universal_plugin_id::clap_universal_plugin_id;
use plughost_core::{DiscoveredPreset, PresetFlags, PresetLocation, PresetPluginId};
use std::collections::HashSet;
use std::ffi::{CStr, c_char};
pub(super) struct Receiver {
    pub presets: Vec<DiscoveredPreset>,
    pub error: Option<ClapError>,
    location: PresetLocation,
    /// The load keys of the presets so far, and whether one had no name or no key, so each new
    /// preset is checked against them without going through the others.
    load_keys: HashSet<String>,
    unnamed: bool,
    unkeyed: bool,
}
impl Receiver {
    pub fn new(location: PresetLocation) -> Self {
        Self {
            presets: Vec::new(),
            error: None,
            location,
            load_keys: HashSet::new(),
            unnamed: false,
            unkeyed: false,
        }
    }
    pub fn raw(&mut self) -> Raw {
        Raw {
            receiver_data: (self as *mut Self).cast(),
            on_error: Some(on_error),
            begin_preset: Some(begin_preset),
            add_plugin_id: Some(add_plugin_id),
            set_soundpack_id: Some(set_soundpack_id),
            set_flags: Some(set_flags),
            add_creator: Some(add_creator),
            set_description: Some(set_description),
            set_timestamps: Some(set_timestamps),
            add_feature: Some(add_feature),
            add_extra_info: Some(add_extra_info),
        }
    }
    fn current(&mut self) -> Result<&mut DiscoveredPreset, ClapError> {
        self.presets.last_mut().ok_or(ClapError::PresetMetadata)
    }
    unsafe fn text(&mut self, ptr: *const c_char) -> Result<String, ClapError> {
        if ptr.is_null() {
            return Err(ClapError::PresetMetadata);
        }
        // SAFETY: provider callback strings must be valid nul-terminated strings for the call.
        text(unsafe { CStr::from_ptr(ptr) })
    }
    unsafe fn optional(&mut self, ptr: *const c_char) -> Result<Option<String>, ClapError> {
        if ptr.is_null() {
            Ok(None)
        } else {
            unsafe { self.text(ptr) }.map(Some)
        }
    }
}
unsafe fn receive(
    raw: *const Raw,
    action: impl FnOnce(&mut Receiver) -> Result<(), ClapError>,
) -> bool {
    // SAFETY: raw is the borrowed receiver passed to the synchronous provider callback.
    let receiver = unsafe { &mut *(*raw).receiver_data.cast::<Receiver>() };
    if receiver.error.is_some() {
        return false;
    }
    match action(receiver) {
        Ok(()) => true,
        Err(error) => {
            receiver.error = Some(error);
            false
        }
    }
}
unsafe extern "C" fn on_error(raw: *const Raw, _code: i32, _message: *const c_char) {
    // SAFETY: callback arguments obey the provider callback contract.
    unsafe {
        receive(raw, |_| Err(ClapError::PresetDiscovery));
    }
}
unsafe extern "C" fn begin_preset(
    raw: *const Raw,
    name: *const c_char,
    key: *const c_char,
) -> bool {
    // SAFETY: callback strings remain valid during this call.
    unsafe {
        receive(raw, |r| {
            let name = r.optional(name)?;
            let load_key = r.optional(key)?;
            if name.as_ref().is_some_and(String::is_empty)
                || load_key.as_ref().is_some_and(String::is_empty)
            {
                return Err(ClapError::PresetMetadata);
            }
            // Presets of the plugin location, like those of a container file, need a name and key.
            if (r.location == PresetLocation::Plugin || !r.presets.is_empty())
                && (name.is_none() || load_key.is_none() || r.unnamed)
            {
                return Err(ClapError::PresetMetadata);
            }
            if !r.presets.is_empty()
                && (r.unkeyed
                    || load_key
                        .as_ref()
                        .is_none_or(|key| r.load_keys.contains(key)))
            {
                return Err(ClapError::PresetMetadata);
            }
            r.unnamed |= name.is_none();
            match &load_key {
                Some(key) => {
                    r.load_keys.insert(key.clone());
                }
                None => r.unkeyed = true,
            }
            let mut preset = DiscoveredPreset::new(r.location.clone());
            preset.name = name;
            preset.load_key = load_key;
            r.presets.push(preset);
            Ok(())
        })
    }
}
unsafe extern "C" fn add_plugin_id(raw: *const Raw, id: *const clap_universal_plugin_id) {
    // SAFETY: the provider's ID and strings remain valid for this callback.
    unsafe {
        receive(raw, |r| {
            if id.is_null() {
                return Err(ClapError::PresetMetadata);
            }
            let id = &*id;
            let abi = r.text(id.abi)?;
            let id = r.text(id.id)?;
            if abi.is_empty() || id.is_empty() {
                return Err(ClapError::PresetMetadata);
            }
            r.current()?.plugin_ids.push(PresetPluginId::new(abi, id));
            Ok(())
        });
    }
}
macro_rules! text_callback {
    ($name:ident,$field:ident,$action:ident) => {
        unsafe extern "C" fn $name(raw:*const Raw,value:*const c_char) {
            // SAFETY: callback strings remain valid for the synchronous call.
            unsafe { receive(raw, |r| { let value = r.text(value)?; text_callback!(@set r.current()?.$field,value,$action); Ok(()) }); }
        }
    };
    (@set $field:expr,$value:expr,push) => {$field.push($value)};
    (@set $field:expr,$value:expr,set) => {$field = Some($value)};
}
text_callback!(set_soundpack_id, soundpack_id, set);
text_callback!(add_creator, creators, push);
text_callback!(set_description, description, set);
text_callback!(add_feature, features, push);
unsafe extern "C" fn set_flags(raw: *const Raw, flags: u32) {
    // SAFETY: receiver is borrowed for this callback.
    unsafe {
        receive(raw, |r| {
            r.current()?.flags = Some(PresetFlags::from_bits_retain(flags));
            Ok(())
        });
    }
}
unsafe extern "C" fn set_timestamps(raw: *const Raw, creation: u64, modification: u64) {
    // SAFETY: receiver is borrowed for this callback.
    unsafe {
        receive(raw, |r| {
            let p = r.current()?;
            p.creation_time = (creation != 0).then_some(creation);
            p.modification_time = (modification != 0).then_some(modification);
            Ok(())
        });
    }
}
unsafe extern "C" fn add_extra_info(raw: *const Raw, key: *const c_char, value: *const c_char) {
    // SAFETY: callback strings remain valid during the call.
    unsafe {
        receive(raw, |r| {
            let key = r.text(key)?;
            let value = r.text(value)?;
            r.current()?.extra_info.push((key, value));
            Ok(())
        });
    }
}
