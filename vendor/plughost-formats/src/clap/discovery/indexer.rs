use super::*;
use clack_host::prelude::HostError;
pub(super) struct Indexer {
    pub error: Option<ClapError>,
    file_types: Vec<PresetFileType>,
    locations: Vec<PresetLocationInfo>,
    soundpacks: Vec<PresetSoundpack>,
}
impl Indexer {
    pub fn new() -> Self {
        Self {
            error: None,
            file_types: Vec::new(),
            locations: Vec::new(),
            soundpacks: Vec::new(),
        }
    }
    pub fn take(&mut self) -> Result<Declarations, ClapError> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        Ok(Declarations {
            file_types: std::mem::take(&mut self.file_types),
            locations: std::mem::take(&mut self.locations),
            soundpacks: std::mem::take(&mut self.soundpacks),
        })
    }
    fn receive(
        &mut self,
        action: impl FnOnce(&mut Self) -> Result<(), ClapError>,
    ) -> Result<(), HostError> {
        if self.error.is_some() {
            return Err(HostError::Message(
                super::super::errors::PRESET_DISCOVERY_FAILED,
            ));
        }
        action(self).map_err(|error| {
            self.error = Some(error);
            HostError::Message(super::super::errors::PRESET_DISCOVERY_FAILED)
        })
    }
}
impl IndexerImpl for Indexer {
    fn declare_filetype(&mut self, value: FileType) -> Result<(), HostError> {
        self.receive(|s| {
            let v = PresetFileType::new(
                text(value.name)?,
                optional(value.description)?,
                optional(value.file_extension)?,
            );
            s.file_types.push(v);
            Ok(())
        })
    }
    fn declare_location(&mut self, value: LocationInfo) -> Result<(), HostError> {
        self.receive(|s| {
            let location = match value.location {
                Location::Plugin => PresetLocation::Plugin,
                Location::File { path } => PresetLocation::File {
                    path: text(path)?.into(),
                },
            };
            location_path(&location).map_err(|_| ClapError::PresetMetadata)?;
            let flags = PresetFlags::from_bits_retain(value.flags.bits());
            let v = PresetLocationInfo::new(text(value.name)?, flags, location);
            s.locations.push(v);
            Ok(())
        })
    }
    fn declare_soundpack(&mut self, value: Soundpack) -> Result<(), HostError> {
        self.receive(|s| {
            let flags = PresetFlags::from_bits_retain(value.flags.bits());
            let mut v = PresetSoundpack::new(text(value.id)?, text(value.name)?, flags);
            v.description = optional(value.description)?;
            v.homepage_url = optional(value.homepage_url)?;
            v.vendor = optional(value.vendor)?;
            v.image_path = optional(value.image_path)?;
            v.release_timestamp = value.release_timestamp.map(|v| v.seconds_since_epoch());
            s.soundpacks.push(v);
            Ok(())
        })
    }
}

pub(super) struct Declarations {
    pub file_types: Vec<PresetFileType>,
    pub locations: Vec<PresetLocationInfo>,
    pub soundpacks: Vec<PresetSoundpack>,
}
