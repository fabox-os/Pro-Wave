use super::{Plugin, Vst3Error, lock, wide_string};
use plughost_core::{FactoryPreset, FactoryPresetId, MAX_FACTORY_PRESETS};
use vst3::Steinberg::Vst::{
    IComponentHandlerTrait, IEditControllerTrait, IUnitInfo, IUnitInfoTrait, ProgramListInfo,
    UnitInfo,
};
use vst3::Steinberg::kResultOk;

impl Plugin {
    /// Lists each unit's factory programs. IDs are scoped to this plugin class. Lists and
    /// programs the plugin cannot describe are left out; at most [`MAX_FACTORY_PRESETS`] are
    /// listed.
    pub(crate) fn factory_presets(&self) -> Result<Vec<FactoryPreset>, Vst3Error> {
        let units = self
            .controller
            .cast::<IUnitInfo>()
            .ok_or(Vst3Error::FactoryPresetsUnsupported)?;
        // Counts are bounded like the listing, so a bogus count cannot stall the owning thread.
        let bound = |count: i32| 0..count.min(MAX_FACTORY_PRESETS as i32);
        let mut unit_lists = Vec::new();
        for unit_index in bound(unsafe { units.getUnitCount() }) {
            let mut unit: UnitInfo = unsafe { std::mem::zeroed() };
            if unsafe { units.getUnitInfo(unit_index, &mut unit) } == kResultOk
                && unit.programListId != -1
                && !unit_lists.contains(&(unit.id, unit.programListId))
            {
                unit_lists.push((unit.id, unit.programListId));
            }
        }
        let mut presets = Vec::new();
        let mut lists = Vec::new();
        for list_index in bound(unsafe { units.getProgramListCount() }) {
            let mut list: ProgramListInfo = unsafe { std::mem::zeroed() };
            if unsafe { units.getProgramListInfo(list_index, &mut list) } != kResultOk
                || list.id == -1
                || lists.contains(&list.id)
            {
                continue;
            }
            lists.push(list.id);
            for program_index in bound(list.programCount) {
                let mut name = [0; 128];
                if unsafe { units.getProgramName(list.id, program_index, &mut name) } != kResultOk {
                    continue;
                }
                for &(unit_id, _) in unit_lists.iter().filter(|(_, id)| *id == list.id) {
                    if presets.len() == MAX_FACTORY_PRESETS {
                        return Ok(presets);
                    }
                    presets.push({
                        let mut factory_preset = FactoryPreset::new(
                            FactoryPresetId::Vst3 {
                                unit_id,
                                list_id: list.id,
                                program_index,
                            },
                            wide_string(&name),
                        );
                        factory_preset.group = Some(wide_string(&list.name));
                        factory_preset
                    });
                }
            }
        }
        Ok(presets)
    }

    /// Selects through the unit's program-change parameter and flushes it to the processor.
    /// Prepare first. Pending edits are discarded. The caller must stage on a separate instance
    /// for failure atomicity.
    pub(crate) fn select_factory_preset(&mut self, id: &FactoryPresetId) -> Result<(), Vst3Error> {
        let FactoryPresetId::Vst3 {
            unit_id,
            program_index,
            ..
        } = id
        else {
            return Err(Vst3Error::InvalidFactoryPreset);
        };
        if !self
            .factory_presets()?
            .iter()
            .any(|preset| preset.id == *id)
        {
            return Err(Vst3Error::InvalidFactoryPreset);
        }
        if lock(&self.engine).prepared.is_none() {
            return Err(Vst3Error::NotPrepared);
        }
        let parameters = self.parameter_cache();
        let index = (0..parameters.len())
            .find(|&index| {
                let flags = parameters.info(index).flags;
                parameters.unit(index) == *unit_id && flags.program_change && !flags.read_only
            })
            .ok_or(Vst3Error::FactoryPresetsUnsupported)?;
        let steps = parameters.info(index).step_count;
        let value = if steps == 0 {
            0.0
        } else {
            f64::from(*program_index) / f64::from(steps)
        };
        if value > 1.0 {
            return Err(Vst3Error::FactoryPresetMetadata);
        }
        let id = parameters.native_id(index);
        self.handler.take_pending();
        let result = unsafe { self.controller.setParamNormalized(id, value) };
        if result != kResultOk {
            return Err(Vst3Error::FactoryPresetSelect(result));
        }
        if unsafe { self.handler.performEdit(id, value) } != kResultOk {
            return Err(Vst3Error::Input(plughost_core::InputError::EventCapacity));
        }
        self.flush_edits()
    }
}
