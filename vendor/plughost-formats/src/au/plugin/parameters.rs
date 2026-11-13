use super::{AUParameter, AuError, Plugin, Retained, parameter_info};
use objc2::msg_send;
use objc2_foundation::NSString;
use plughost_core::internal::{validate_normalized, validate_parameter_text};
use plughost_core::{InputError, PARAMETER_TEXT_BYTES};

impl Plugin {
    pub(crate) fn parameter_details(
        &self,
        id: u64,
    ) -> Result<plughost_core::ParameterDetails, AuError> {
        let parameter = self.parameter(id)?;
        let plain_at_zero = self.parameter_to_plain(id, 0.0)?;
        let plain_at_one = self.parameter_to_plain(id, 1.0)?;
        let unit = self.shared_unit()?;
        let groups = (unsafe { unit.parameterTree() }).and_then(|tree| group_path(&tree, id));
        Ok({
            let mut parameter_details = plughost_core::ParameterDetails::new(
                super::parameter_info(&parameter),
                plain_at_zero,
                plain_at_one,
            );
            parameter_details.groups = groups;
            parameter_details.native_unit = Some(unsafe { parameter.unit() }.0);
            parameter_details
        })
    }

    fn parameter(&self, id: u64) -> Result<Retained<AUParameter>, AuError> {
        let unit = self.shared_unit()?;
        (unsafe { unit.parameterTree() })
            .and_then(|tree| unsafe { tree.parameterWithAddress(id) })
            .ok_or(AuError::Input(InputError::UnknownParameter { id }))
    }

    pub(crate) fn parameter_to_plain(&self, id: u64, value: f64) -> Result<f64, AuError> {
        let parameter = self.parameter(id)?;
        validate_normalized(value).map_err(AuError::Input)?;
        let (min, max) = unsafe {
            (
                f64::from(parameter.minValue()),
                f64::from(parameter.maxValue()),
            )
        };
        if !min.is_finite() || !max.is_finite() || min > max {
            return Err(AuError::ParameterResult);
        }
        let plain = min + value * (max - min);
        // Whole for a discrete parameter, as an edit with `value` selects it.
        Ok(if parameter_info(&parameter).flags.discrete {
            plain.round()
        } else {
            plain
        })
    }

    pub(crate) fn parameter_to_normalized(&self, id: u64, plain: f64) -> Result<f64, AuError> {
        let parameter = self.parameter(id)?;
        let (min, max) = unsafe {
            (
                f64::from(parameter.minValue()),
                f64::from(parameter.maxValue()),
            )
        };
        if !min.is_finite() || !max.is_finite() || min > max {
            return Err(AuError::ParameterResult);
        }
        if !plain.is_finite() || !(min..=max).contains(&plain) {
            return Err(AuError::Input(InputError::PlainParameterValue));
        }
        Ok(if min == max {
            0.0
        } else {
            (plain - min) / (max - min)
        })
    }

    /// A value the unit reports, normalized, when it is one of the parameter's: in its range or
    /// past an end by rounding.
    pub(crate) fn reported_normalized(&self, id: u64, plain: f64) -> Option<f64> {
        let parameter = self.parameter(id).ok()?;
        let (min, max) = unsafe { (parameter.minValue(), parameter.maxValue()) };
        crate::parameter_values::reported(plain, f64::from(min), f64::from(max))
    }

    pub(crate) fn parameter_text(&self, id: u64, value: f64) -> Result<String, AuError> {
        let plain = self.parameter_to_plain(id, value)? as f32;
        // AUv2 bridges can return nil when no native formatter is provided, despite
        // the nonnull SDK annotation. Preserve unsupported conversion as an error.
        let text: Option<Retained<NSString>> =
            unsafe { msg_send![&*self.parameter(id)?, stringFromValue: &plain] };
        let text = super::text(&*text.ok_or(AuError::ParameterConversion)?);
        if text.len() > PARAMETER_TEXT_BYTES {
            return Err(AuError::ParameterResult);
        }
        Ok(text)
    }

    /// AU's parser returns a value without a separate success flag; interpretation is native.
    pub(crate) fn parameter_from_text(&self, id: u64, text: &str) -> Result<f64, AuError> {
        validate_parameter_text(text).map_err(AuError::Input)?;
        let plain = unsafe {
            self.parameter(id)?
                .valueFromString(&NSString::from_str(text))
        };
        if !plain.is_finite() {
            return Err(AuError::ParameterResult);
        }
        self.parameter_to_normalized(id, f64::from(plain))
    }
}

fn group_path(
    group: &objc2_audio_toolbox::AUParameterGroup,
    id: u64,
) -> Option<Vec<plughost_core::ParameterGroup>> {
    let children = unsafe { group.children() };
    for index in 0..children.count() {
        let node = children.objectAtIndex(index);
        if let Some(parameter) = node.downcast_ref::<AUParameter>()
            && unsafe { parameter.address() } == id
        {
            return Some(Vec::new());
        }
        if let Some(child) = node.downcast_ref::<objc2_audio_toolbox::AUParameterGroup>()
            && let Some(mut path) = group_path(child, id)
        {
            path.insert(
                0,
                plughost_core::ParameterGroup::new(
                    super::text(&*unsafe { child.keyPath() }),
                    super::text(&*unsafe { child.displayName() }),
                    None,
                ),
            );
            return Some(path);
        }
    }
    None
}
