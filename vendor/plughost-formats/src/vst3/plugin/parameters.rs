use super::{Plugin, Vst3Error, wide_string};
use plughost_core::InputError;
use plughost_core::internal::{validate_normalized, validate_parameter_text};
use vst3::Steinberg::Vst::{IEditControllerTrait, String128};
use vst3::Steinberg::kResultOk;

impl Plugin {
    pub(crate) fn parameter_details(
        &self,
        id: u64,
    ) -> Result<plughost_core::ParameterDetails, Vst3Error> {
        use vst3::Steinberg::Vst::{IUnitInfo, IUnitInfoTrait, UnitInfo, kRootUnitId};
        let parameters = self.parameter_cache();
        let index = parameters
            .index(id)
            .ok_or(Vst3Error::Input(InputError::UnknownParameter { id }))?;
        let info = parameters.info(index).clone();
        let plain_at_zero = self.parameter_to_plain(id, 0.0)?;
        let plain_at_one = self.parameter_to_plain(id, 1.0)?;
        if let Some(default) = info.default_value {
            validate_normalized(default).map_err(|_| Vst3Error::ParameterResult)?;
        }
        let groups = if let Some(units) = self.controller.cast::<IUnitInfo>() {
            let count = unsafe { units.getUnitCount() };
            let mut current = parameters.unit(index);
            let mut seen = std::collections::HashSet::new();
            let mut path = Vec::new();
            while current != -1 {
                if !seen.insert(current) {
                    return Err(Vst3Error::ParameterGroups);
                }
                let unit = (0..count).find_map(|index| {
                    let mut unit: UnitInfo = unsafe { std::mem::zeroed() };
                    (unsafe { units.getUnitInfo(index, &mut unit) } == kResultOk
                        && unit.id == current)
                        .then_some(unit)
                });
                // Plugins may leave the root unit implicit, as the SDK's own examples do.
                let Some(unit) = unit else {
                    if current == kRootUnitId {
                        break;
                    }
                    return Err(Vst3Error::ParameterGroups);
                };
                path.push(plughost_core::ParameterGroup::new(
                    unit.id.to_string(),
                    wide_string(&unit.name),
                    (unit.programListId != -1).then_some(unit.programListId),
                ));
                current = unit.parentUnitId;
            }
            path.reverse();
            Some(path)
        } else {
            None
        };
        Ok({
            let mut parameter_details =
                plughost_core::ParameterDetails::new(info, plain_at_zero, plain_at_one);
            parameter_details.groups = groups;
            parameter_details
        })
    }

    fn parameter_id(&self, id: u64) -> Result<u32, Vst3Error> {
        let parameters = self.parameter_cache();
        parameters
            .index(id)
            .map(|index| parameters.native_id(index))
            .ok_or(Vst3Error::Input(InputError::UnknownParameter { id }))
    }

    /// Uses the controller's display formatter, including its units and nonlinear scale.
    pub(crate) fn parameter_text(&self, id: u64, value: f64) -> Result<String, Vst3Error> {
        let id = self.parameter_id(id)?;
        validate_normalized(value).map_err(Vst3Error::Input)?;
        let mut text: String128 = [0; 128];
        let result = unsafe { self.controller.getParamStringByValue(id, value, &mut text) };
        if result != kResultOk {
            return Err(Vst3Error::ParameterConversion);
        }
        Ok(wide_string(&text))
    }

    /// Parses without editing. A native rejection is an invalid text error, never a default value.
    pub(crate) fn parameter_from_text(&self, id: u64, text: &str) -> Result<f64, Vst3Error> {
        let id = self.parameter_id(id)?;
        validate_parameter_text(text).map_err(Vst3Error::Input)?;
        let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut value = 0.0;
        let result = unsafe {
            self.controller
                .getParamValueByString(id, wide.as_mut_ptr(), &mut value)
        };
        if result != kResultOk {
            return Err(Vst3Error::Input(InputError::ParameterText));
        }
        validate_normalized(value).map_err(|_| Vst3Error::ParameterResult)?;
        Ok(value)
    }

    /// Uses VST3's native conversion rather than interpolating the endpoints.
    pub(crate) fn parameter_to_plain(&self, id: u64, value: f64) -> Result<f64, Vst3Error> {
        let id = self.parameter_id(id)?;
        validate_normalized(value).map_err(Vst3Error::Input)?;
        let plain = unsafe { self.controller.normalizedParamToPlain(id, value) };
        if !plain.is_finite() {
            return Err(Vst3Error::ParameterResult);
        }
        Ok(plain)
    }

    /// The controller owns the plain domain and conversion; no host clamp or linear fallback.
    pub(crate) fn parameter_to_normalized(&self, id: u64, plain: f64) -> Result<f64, Vst3Error> {
        let id = self.parameter_id(id)?;
        if !plain.is_finite() {
            return Err(Vst3Error::Input(InputError::PlainParameterValue));
        }
        let value = unsafe { self.controller.plainParamToNormalized(id, plain) };
        validate_normalized(value).map_err(|_| Vst3Error::ParameterResult)?;
        Ok(value)
    }
}
