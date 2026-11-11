use super::{CString, ClapError, Plugin};
use clack_host::prelude::ClapId;
use plughost_core::internal::{validate_normalized, validate_parameter_text};
use plughost_core::{InputError, PARAMETER_TEXT_BYTES};

impl Plugin {
    pub(crate) fn parameter_details(
        &mut self,
        id: u64,
    ) -> Result<plughost_core::ParameterDetails, ClapError> {
        let (_, min, max) = self.parameter_range(id)?;
        let cache = self.parameter_cache();
        let parameter = cache
            .get(id)
            .ok_or(ClapError::Input(InputError::UnknownParameter { id }))?;
        let info = parameter.info.clone();
        if info.flags.discrete
            && ((max - min) > f64::from(u32::MAX) || min.fract() != 0.0 || max.fract() != 0.0)
        {
            return Err(ClapError::ParameterResult);
        }
        if let Some(default) = info.default_value {
            validate_normalized(default).map_err(|_| ClapError::ParameterResult)?;
        }
        let module = &parameter.module;
        let mut groups = Vec::new();
        let mut path = String::new();
        if !module.is_empty() {
            for (index, name) in module.split('/').enumerate() {
                if index > 0 {
                    path.push('/');
                }
                path.push_str(name);
                groups.push(plughost_core::ParameterGroup::new(
                    path.clone(),
                    name.to_owned(),
                    None,
                ));
            }
        }
        Ok({
            let mut parameter_details = plughost_core::ParameterDetails::new(info, min, max);
            parameter_details.groups = Some(groups);
            parameter_details
        })
    }

    /// CLAP stepped values are integers. Do not round-trip them through normalization before formatting.
    pub(crate) fn parameter_choices(
        &mut self,
        id: u64,
        start: u64,
        count: u32,
    ) -> Result<plughost_core::ParameterChoicePage, crate::Error> {
        plughost_core::internal::validate_choice_page(start, count).map_err(crate::Error::from)?;
        let details = self.parameter_details(id)?;
        let (total, indices) = crate::parameter_choices::indices(&details, start, count)?;
        let (native, min, _) = self.parameter_range(id)?;
        let mut choices = Vec::new();
        for index in indices {
            let plain = min + index as f64;
            let normalized = if details.info.step_count == 0 {
                0.0
            } else {
                index as f64 / f64::from(details.info.step_count)
            };
            choices.push(plughost_core::ParameterChoice::new(
                index,
                normalized,
                plain,
                self.plain_text(native, plain)?,
            ));
        }
        Ok(plughost_core::ParameterChoicePage::new(
            total, start, choices,
        ))
    }

    fn parameter_range(&mut self, id: u64) -> Result<(u32, f64, f64), ClapError> {
        let cache = self.parameter_cache();
        let parameter = cache
            .get(id)
            .ok_or(ClapError::Input(InputError::UnknownParameter { id }))?;
        let (min, max) = crate::parameter_values::range(parameter.min, parameter.max)
            .ok_or(ClapError::ParameterResult)?;
        Ok((parameter.native.get(), min, max))
    }

    /// CLAP values are native plain values; the common normalized domain maps over its range, and
    /// stepped parameters take whole values.
    pub(crate) fn parameter_to_plain(&mut self, id: u64, value: f64) -> Result<f64, ClapError> {
        self.parameter_range(id)?;
        validate_normalized(value).map_err(ClapError::Input)?;
        let cache = self.parameter_cache();
        let parameter = cache
            .get(id)
            .ok_or(ClapError::Input(InputError::UnknownParameter { id }))?;
        parameter.plain(value).ok_or(ClapError::ParameterResult)
    }

    pub(crate) fn parameter_to_normalized(
        &mut self,
        id: u64,
        plain: f64,
    ) -> Result<f64, ClapError> {
        let (_, min, max) = self.parameter_range(id)?;
        if !plain.is_finite() || !(min..=max).contains(&plain) {
            return Err(ClapError::Input(InputError::PlainParameterValue));
        }
        Ok(if min == max {
            0.0
        } else {
            (plain - min) / (max - min)
        })
    }

    pub(crate) fn parameter_text(&mut self, id: u64, value: f64) -> Result<String, ClapError> {
        let plain = self.parameter_to_plain(id, value)?;
        let (native, _, _) = self.parameter_range(id)?;
        self.plain_text(native, plain)
    }

    fn plain_text(&mut self, native: u32, plain: f64) -> Result<String, ClapError> {
        let params = self
            .shared()
            .extensions()
            .params
            .ok_or(ClapError::ParameterConversion)?;
        let mut buffer = [0u8; PARAMETER_TEXT_BYTES + 1];
        let bytes = params
            .value_to_text(
                &self.instance.plugin_handle(),
                ClapId::new(native),
                plain,
                &mut buffer,
            )
            .map_err(|_| ClapError::ParameterConversion)?;
        if bytes.len() > PARAMETER_TEXT_BYTES {
            return Err(ClapError::ParameterResult);
        }
        String::from_utf8(bytes.to_vec()).map_err(|_| ClapError::ParameterResult)
    }

    pub(crate) fn parameter_from_text(&mut self, id: u64, text: &str) -> Result<f64, ClapError> {
        let (native, _, _) = self.parameter_range(id)?;
        validate_parameter_text(text).map_err(ClapError::Input)?;
        let text = CString::new(text).map_err(|_| ClapError::Input(InputError::ParameterText))?;
        let params = self
            .shared()
            .extensions()
            .params
            .ok_or(ClapError::ParameterConversion)?;
        let plain = params
            .text_to_value(&self.instance.plugin_handle(), ClapId::new(native), &text)
            .ok_or(ClapError::Input(InputError::ParameterText))?;
        if !plain.is_finite() {
            return Err(ClapError::ParameterResult);
        }
        self.parameter_to_normalized(id, plain)
    }
}
