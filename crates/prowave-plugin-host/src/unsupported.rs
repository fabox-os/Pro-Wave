//! The API of this crate on platforms without plug-in hosting (Linux, Intel Macs).

use std::path::PathBuf;

use crate::{Error, Format, Parameter, PluginDescriptor};

/// No catalog exists on this platform.
pub struct Catalog;

/// No plug-in can be loaded on this platform.
pub struct Hosted {
    pub control: Control,
    pub effect: Box<dyn prowave_engine::HostedEffect>,
    pub latency: u32,
    pub parameters: Vec<Parameter>,
}

/// No instrument can be loaded on this platform.
pub struct HostedInstrument {
    pub control: Control,
    pub instrument: Box<dyn prowave_engine::HostedInstrument>,
    pub latency: u32,
    pub parameters: Vec<Parameter>,
}

/// No control side exists on this platform.
pub struct Control;

pub fn available() -> bool {
    false
}

pub fn scan(_extra: &[PathBuf]) -> Result<Catalog, Error> {
    Err(Error::unsupported())
}

impl Catalog {
    pub fn plugins(&self) -> Vec<PluginDescriptor> {
        Vec::new()
    }

    pub fn counts(&self) -> [(Format, usize); 3] {
        Format::ALL.map(|format| (format, 0))
    }

    pub fn instantiate(
        &self,
        _key: &str,
        _sample_rate: f64,
        _quantum: usize,
        _state: Option<&[u8]>,
    ) -> Result<Hosted, Error> {
        Err(Error::unsupported())
    }

    pub fn instantiate_instrument(
        &self,
        _key: &str,
        _sample_rate: f64,
        _quantum: usize,
        _state: Option<&[u8]>,
    ) -> Result<HostedInstrument, Error> {
        Err(Error::unsupported())
    }
}

impl Control {
    pub fn set_parameter(&mut self, _id: u64, _value: f64) -> Result<(), Error> {
        Err(Error::unsupported())
    }

    pub fn parameters(&mut self) -> Result<Vec<Parameter>, Error> {
        Err(Error::unsupported())
    }

    pub fn state(&mut self) -> Result<Vec<u8>, Error> {
        Err(Error::unsupported())
    }

    pub fn set_bypass(&mut self, _bypass: bool) -> Result<(), Error> {
        Err(Error::unsupported())
    }

    pub fn resync_if_needed(&mut self) -> Result<(), Error> {
        Err(Error::unsupported())
    }

    pub fn open_editor(&mut self) -> Result<(), Error> {
        Err(Error::unsupported())
    }

    pub fn close_editor(&mut self) -> Result<(), Error> {
        Err(Error::unsupported())
    }

    pub fn editor_open(&mut self) -> Result<bool, Error> {
        Err(Error::unsupported())
    }
}
