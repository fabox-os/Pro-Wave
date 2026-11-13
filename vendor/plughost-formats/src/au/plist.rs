//! Audio Unit state as property lists: the `fullState` dictionary, stored as a binary property
//! list in [`plughost_core::PluginState`], and as an XML property list in `.aupreset` files.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFBoolean, CFGetTypeID, CFType, ConcreteType};
use objc2_foundation::{
    NSData, NSDictionary, NSPropertyListFormat, NSPropertyListMutabilityOptions,
    NSPropertyListSerialization, NSString, NSUTF8StringEncoding,
};

use super::errors::AuError;

pub type State = NSDictionary<NSString, AnyObject>;

pub fn encode(state: &State, format: NSPropertyListFormat) -> Result<Vec<u8>, AuError> {
    // SAFETY: a fullState dictionary contains only property list types.
    let data = unsafe {
        NSPropertyListSerialization::dataWithPropertyList_format_options_error(state, format, 0)
    }
    .map_err(|_| AuError::State)?;
    // Foundation has already created NSData; reject before making an additional Rust copy.
    if data.len() > plughost_core::MAX_STATE_BYTES {
        return Err(AuError::StateTooLarge);
    }
    Ok(data.to_vec())
}

pub fn decode(bytes: &[u8]) -> Result<Retained<State>, AuError> {
    if bytes.len() > plughost_core::MAX_STATE_BYTES {
        return Err(AuError::Input(plughost_core::InputError::StateSize));
    }
    let data = NSData::with_bytes(bytes);
    let object = unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(
            &data,
            NSPropertyListMutabilityOptions::Immutable,
            std::ptr::null_mut(),
        )
    }
    .map_err(|_| AuError::State)?;
    let dictionary = object
        .downcast::<NSDictionary>()
        .map_err(|_| AuError::State)?;
    // SAFETY: property list dictionaries have string keys.
    Ok(unsafe { Retained::cast_unchecked(dictionary) })
}

/// Converts a stored state into the XML property list of an `.aupreset` file.
pub fn to_preset(state: &[u8]) -> Result<Vec<u8>, crate::Error> {
    let state = decode(state)?;
    let bytes = encode(&state, NSPropertyListFormat::XMLFormat_v1_0)?;
    validate_preset_size(&bytes)?;
    Ok(bytes)
}

/// Reads an `.aupreset` file into the binary form states are stored in.
pub fn from_preset(preset: &[u8]) -> Result<Vec<u8>, crate::Error> {
    validate_preset_size(preset)?;
    let state = decode(preset)?;
    inspect_dictionary(&state)?;
    Ok(encode(&state, NSPropertyListFormat::BinaryFormat_v1_0)?)
}

/// Reads identity from the container itself, before a host applies any opaque state.
pub fn inspect_preset(bytes: &[u8]) -> Result<plughost_core::PresetInfo, crate::Error> {
    validate_preset_size(bytes)?;
    let state = decode(bytes)?;
    Ok(inspect_dictionary(&state)?)
}

fn validate_preset_size(bytes: &[u8]) -> Result<(), AuError> {
    if bytes.len() > plughost_core::MAX_PRESET_BYTES {
        return Err(AuError::Input(plughost_core::InputError::PresetSize));
    }
    Ok(())
}

fn inspect_dictionary(state: &State) -> Result<plughost_core::PresetInfo, AuError> {
    let code = |key: &str| -> Result<u32, AuError> {
        let value = state
            .objectForKey(&NSString::from_str(key))
            .ok_or(AuError::State)?;
        let number = value
            .downcast::<objc2_foundation::NSNumber>()
            .map_err(|_| AuError::State)?;
        // NSNumber includes CFBoolean. A property-list Boolean is not an integer FourCC.
        // SAFETY: Foundation NSNumber is toll-free bridged to CFNumber or CFBoolean.
        let cf_number =
            unsafe { &*((&*number as *const objc2_foundation::NSNumber).cast::<CFType>()) };
        if CFGetTypeID(Some(cf_number)) == CFBoolean::type_id() {
            return Err(AuError::State);
        }
        let value = number.doubleValue();
        // Apple property lists can store FourCCs as signed or unsigned 32-bit integers.
        if !value.is_finite()
            || value.fract() != 0.0
            || value < i32::MIN as f64
            || value > u32::MAX as f64
        {
            return Err(AuError::State);
        }
        Ok(value as i64 as u32)
    };
    let class_id = format!(
        "{:08X}{:08X}{:08X}",
        code("type")?,
        code("subtype")?,
        code("manufacturer")?
    );
    let name = state
        .objectForKey(&NSString::from_str("name"))
        .map(|value| {
            let name = value.downcast::<NSString>().map_err(|_| AuError::State)?;
            // Malformed binary plists can produce NSStrings with invalid Unicode. Their
            // UTF8String may be null, so Display/to_string is not a valid boundary conversion.
            let bytes = name
                .dataUsingEncoding(NSUTF8StringEncoding)
                .ok_or(AuError::State)?;
            String::from_utf8(bytes.to_vec()).map_err(|_| AuError::State)
        })
        .transpose()?;
    Ok(plughost_core::PresetInfo::new(
        class_id,
        plughost_core::PresetMetadata::AudioUnit { name },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(kind: &str, subtype: &str, manufacturer: &str, name: &str) -> Vec<u8> {
        format!(
            "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
             <key>type</key>{kind}<key>subtype</key>{subtype}\
             <key>manufacturer</key>{manufacturer}<key>name</key>{name}\
             </dict></plist>"
        )
        .into_bytes()
    }

    #[test]
    fn preset_identity_and_name_survive_binary_xml_roundtrip() {
        let bytes = preset(
            "<integer>1635083896</integer>",
            "<integer>1684368505</integer>",
            "<integer>1634758764</integer>",
            "<string>Delay &amp; space</string>",
        );
        let info = inspect_preset(&bytes).unwrap();
        assert_eq!(info.class_id, "6175667864656C796170706C");
        assert_eq!(
            info.metadata,
            plughost_core::PresetMetadata::AudioUnit {
                name: Some("Delay & space".to_owned()),
            }
        );
        let encoded = to_preset(&from_preset(&bytes).unwrap()).unwrap();
        assert_eq!(inspect_preset(&encoded).unwrap(), info);
    }

    #[test]
    fn malformed_identity_is_rejected_before_a_plugin_receives_it() {
        for kind in [
            "<true/>",
            "<string>aufx</string>",
            "<real>1.5</real>",
            "<integer>4294967296</integer>",
        ] {
            let bytes = preset(
                kind,
                "<integer>1</integer>",
                "<integer>2</integer>",
                "<string>bad</string>",
            );
            assert_eq!(
                inspect_preset(&bytes).unwrap_err().kind(),
                plughost_core::FailureKind::State
            );
            assert_eq!(
                from_preset(&bytes).unwrap_err().kind(),
                plughost_core::FailureKind::State
            );
        }
    }
}
