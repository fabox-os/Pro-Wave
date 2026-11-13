//! Loading a `.vst3` module and reading its factory, following the SDK's `module_mac.mm` and
//! `module_win32.cpp`.

use std::ffi::c_char;
use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use plughost_core::{HostIdentity, PluginFormat, PluginId, PluginInfo, PluginKind};
use vst3::Steinberg::{
    FUnknown, IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactory3,
    IPluginFactory3Trait, IPluginFactoryTrait, PClassInfo, PClassInfo2, PClassInfoW, PFactoryInfo,
    TUID, kResultOk,
};
use vst3::{ComPtr, ComWrapper};

use super::errors::Vst3Error;
use super::host::{HostApplication, wide_string};
use super::uid;

const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// A loaded VST3 module. Plugins created from it keep it loaded.
pub struct Module {
    path: PathBuf,
    factory: ManuallyDrop<ComPtr<IPluginFactory>>,
    binary: platform::Binary,
    // Some factories borrow the context; keep it alive through factory release and module exit.
    _host: ComWrapper<HostApplication>,
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Module")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Module {
    /// Loads the module at `path`: a `.vst3` bundle, or on Windows also a single-file `.vst3`.
    ///
    /// # Safety
    ///
    /// Loading runs the module's own code in this process, with everything the process may do,
    /// and nothing checks that code: one that is not a sound VST3 plugin can corrupt or end the
    /// process. Load only modules trusted to behave, or load them in a process of their own, as
    /// `plughost`'s helper does.
    pub unsafe fn load(path: &Path) -> Result<Rc<Module>, crate::Error> {
        // SAFETY: the caller accepts running the module's code.
        unsafe { Self::load_with_host(path, &HostIdentity::default()) }
    }

    /// Loads a module with a factory-scoped host identity, supplied before class enumeration.
    /// Optional IPluginFactory3 host-context support does not replace per-instance initialization.
    ///
    /// # Safety
    ///
    /// Loading runs the module's own code in this process, with everything the process may do,
    /// and nothing checks that code: one that is not a sound VST3 plugin can corrupt or end the
    /// process. Load only modules trusted to behave, or load them in a process of their own, as
    /// `plughost`'s helper does.
    pub unsafe fn load_with_host(
        path: &Path,
        identity: &HostIdentity,
    ) -> Result<Rc<Module>, crate::Error> {
        identity.validate()?;
        let (binary, factory) = platform::Binary::open(path)?;
        // SAFETY: GetPluginFactory returns an owned reference.
        let Some(factory) = (unsafe { ComPtr::from_raw(factory) }) else {
            binary.exit();
            return Err(Vst3Error::NoFactory.into());
        };
        let host = ComWrapper::new(HostApplication {
            name: identity.name.clone(),
        });
        if let Some(factory3) = factory.cast::<IPluginFactory3>() {
            let context = host.as_com_ref::<FUnknown>().unwrap();
            // SAFETY: the wrapper owns the context for the module lifetime. Factories may ignore
            // the optional context, so a non-success result does not invalidate the factory.
            unsafe { factory3.setHostContext(context.as_ptr()) };
        }
        Ok(Rc::new(Module {
            path: path.to_path_buf(),
            factory: ManuallyDrop::new(factory),
            binary,
            _host: host,
        }))
    }

    /// The bundle or module path used to load this factory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn factory(&self) -> &ComPtr<IPluginFactory> {
        &self.factory
    }

    /// The module's audio module classes (effects and instruments).
    pub fn classes(&self) -> Vec<PluginInfo> {
        self.audio_classes()
            .into_iter()
            .map(|(_, info)| info)
            .collect()
    }

    pub(crate) fn find_class(&self, class_id: &str) -> Result<(TUID, PluginInfo), Vst3Error> {
        let id = PluginId::new(PluginFormat::Vst3, class_id);
        self.audio_classes()
            .into_iter()
            .find(|(_, info)| info.id == id)
            .ok_or_else(|| Vst3Error::ClassNotFound(class_id.to_owned()))
    }

    fn audio_classes(&self) -> Vec<(TUID, PluginInfo)> {
        // SAFETY: the info structs are plain C data the factory fills in.
        let mut factory_info: PFactoryInfo = unsafe { std::mem::zeroed() };
        unsafe { self.factory.getFactoryInfo(&mut factory_info) };
        let factory_vendor = c_string(&factory_info.vendor);

        let count = unsafe { self.factory.countClasses() };
        let mut classes = Vec::new();
        for index in 0..count {
            let Some(class) = self.class_info(index) else {
                continue;
            };
            if class.category != AUDIO_MODULE_CLASS {
                continue;
            }
            let categories: Vec<String> = class
                .sub_categories
                .split('|')
                .filter(|category| !category.is_empty())
                .map(str::to_owned)
                .collect();
            let kind = if categories.iter().any(|c| c == "Instrument") {
                PluginKind::Instrument
            } else {
                PluginKind::Effect
            };
            classes.push((class.cid, {
                let mut plugin_info = PluginInfo::new(
                    PluginId::new(PluginFormat::Vst3, uid::to_string(&class.cid)),
                    class.name,
                    kind,
                );
                plugin_info.vendor = if class.vendor.is_empty() {
                    factory_vendor.clone()
                } else {
                    class.vendor
                };
                plugin_info.version = class.version;
                plugin_info.sdk_version = class.sdk_version;
                plugin_info.categories = categories;
                plugin_info
            }));
        }
        classes
    }

    /// The richest class description the factory provides, as the SDK's hosting module reads it:
    /// Unicode classes are only described through `IPluginFactory3`.
    fn class_info(&self, index: i32) -> Option<ClassInfo> {
        // SAFETY: the info structs are plain C data the factory fills in.
        if let Some(factory3) = self.factory.cast::<IPluginFactory3>() {
            let mut info: PClassInfoW = unsafe { std::mem::zeroed() };
            if unsafe { factory3.getClassInfoUnicode(index, &mut info) } == kResultOk {
                return Some(ClassInfo {
                    cid: info.cid,
                    category: c_string(&info.category),
                    name: wide_string(&info.name),
                    sub_categories: c_string(&info.subCategories),
                    vendor: wide_string(&info.vendor),
                    version: wide_string(&info.version),
                    sdk_version: wide_string(&info.sdkVersion),
                });
            }
        }
        if let Some(factory2) = self.factory.cast::<IPluginFactory2>() {
            let mut info: PClassInfo2 = unsafe { std::mem::zeroed() };
            if unsafe { factory2.getClassInfo2(index, &mut info) } == kResultOk {
                return Some(ClassInfo {
                    cid: info.cid,
                    category: c_string(&info.category),
                    name: c_string(&info.name),
                    sub_categories: c_string(&info.subCategories),
                    vendor: c_string(&info.vendor),
                    version: c_string(&info.version),
                    sdk_version: c_string(&info.sdkVersion),
                });
            }
        }
        let mut info: PClassInfo = unsafe { std::mem::zeroed() };
        (unsafe { self.factory.getClassInfo(index, &mut info) } == kResultOk).then(|| ClassInfo {
            cid: info.cid,
            category: c_string(&info.category),
            name: c_string(&info.name),
            sub_categories: String::new(),
            vendor: String::new(),
            version: String::new(),
            sdk_version: String::new(),
        })
    }
}

/// One factory class, whichever `PClassInfo` version described it.
struct ClassInfo {
    cid: TUID,
    category: String,
    name: String,
    sub_categories: String,
    vendor: String,
    version: String,
    sdk_version: String,
}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: the factory is released before the module's exit function and not used again.
        unsafe { ManuallyDrop::drop(&mut self.factory) };
        self.binary.exit();
    }
}

pub(crate) fn c_string(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
compile_error!("plughost supports macOS and Windows");

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;
    use std::path::Path;

    use objc2_core_foundation::{CFBundle, CFRetained, CFString, CFURL, CFURLPathStyle};
    use vst3::Steinberg::IPluginFactory;

    use crate::vst3::errors::Vst3Error;

    type BundleEntry = unsafe extern "C" fn(*mut c_void) -> bool;
    type BundleExit = unsafe extern "C" fn() -> bool;
    type GetPluginFactory = unsafe extern "C" fn() -> *mut IPluginFactory;

    pub struct Binary {
        bundle: CFRetained<CFBundle>,
    }

    impl Binary {
        pub fn open(path: &Path) -> Result<(Binary, *mut IPluginFactory), Vst3Error> {
            let path = CFString::from_str(&path.to_string_lossy());
            let url = CFURL::with_file_system_path(
                None,
                Some(&path),
                CFURLPathStyle::CFURLPOSIXPathStyle,
                true,
            )
            .ok_or(Vst3Error::BundleOpen)?;
            let bundle = CFBundle::new(None, Some(&url)).ok_or(Vst3Error::BundleOpen)?;
            // SAFETY: running the module's code is the purpose of loading it.
            if !unsafe { bundle.load_executable() } {
                return Err(Vst3Error::BundleLoad(None));
            }
            let binary = Binary { bundle };
            // The SDK accepts both spellings.
            let entry = binary
                .function(&["bundleEntry", "BundleEntry"])
                .ok_or(Vst3Error::NoEntryPoint("bundleEntry"))?;
            // SAFETY: bundleEntry has this signature in the VST3 module ABI.
            let entry: BundleEntry = unsafe { std::mem::transmute(entry) };
            let bundle_ref = &*binary.bundle as *const CFBundle as *mut c_void;
            if !unsafe { entry(bundle_ref) } {
                return Err(Vst3Error::EntryFailed);
            }
            // An entered module is exited even when loading fails from here, as the SDK does.
            let Some(get_factory) = binary.function(&["GetPluginFactory"]) else {
                binary.exit();
                return Err(Vst3Error::NoEntryPoint("GetPluginFactory"));
            };
            // SAFETY: GetPluginFactory has this signature in the VST3 module ABI.
            let get_factory: GetPluginFactory = unsafe { std::mem::transmute(get_factory) };
            let factory = unsafe { get_factory() };
            Ok((binary, factory))
        }

        pub fn exit(&self) {
            if let Some(exit) = self.function(&["bundleExit", "BundleExit"]) {
                // SAFETY: bundleExit has this signature in the VST3 module ABI.
                let exit: BundleExit = unsafe { std::mem::transmute(exit) };
                unsafe { exit() };
            }
        }

        fn function(&self, names: &[&'static str]) -> Option<*mut c_void> {
            names.iter().find_map(|name| {
                let pointer = self
                    .bundle
                    .function_pointer_for_name(Some(&CFString::from_static_str(name)));
                (!pointer.is_null()).then_some(pointer)
            })
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use std::path::{Path, PathBuf};

    use libloading::Library;
    use vst3::Steinberg::IPluginFactory;

    use crate::vst3::errors::Vst3Error;

    type ModuleEntry = unsafe extern "system" fn() -> bool;
    type GetPluginFactory = unsafe extern "system" fn() -> *mut IPluginFactory;

    #[cfg(target_arch = "x86_64")]
    const ARCHITECTURE_FOLDER: &str = "x86_64-win";

    pub struct Binary {
        library: Library,
    }

    /// A bundle keeps its binary in `Contents/<architecture>/<bundle name>`; an old single-file
    /// module is the binary itself.
    fn binary_path(path: &Path) -> PathBuf {
        match path.file_name() {
            Some(name) if path.is_dir() => {
                path.join("Contents").join(ARCHITECTURE_FOLDER).join(name)
            }
            _ => path.to_path_buf(),
        }
    }

    impl Binary {
        pub fn open(path: &Path) -> Result<(Binary, *mut IPluginFactory), Vst3Error> {
            // SAFETY: running the module's code is the purpose of loading it.
            let library = unsafe { Library::new(binary_path(path)) }
                .map_err(|error| Vst3Error::BundleLoad(Some(Box::new(error))))?;
            let binary = Binary { library };
            // SAFETY: InitDll and GetPluginFactory have these signatures in the VST3 module ABI.
            unsafe {
                if let Ok(init) = binary.library.get::<ModuleEntry>(c"InitDll")
                    && !init()
                {
                    return Err(Vst3Error::EntryFailed);
                }
                // An entered module is exited even when loading fails from here, as the SDK does.
                let Ok(get_factory) = binary.library.get::<GetPluginFactory>(c"GetPluginFactory")
                else {
                    binary.exit();
                    return Err(Vst3Error::NoEntryPoint("GetPluginFactory"));
                };
                let factory = get_factory();
                Ok((binary, factory))
            }
        }

        pub fn exit(&self) {
            // SAFETY: ExitDll has this signature in the VST3 module ABI.
            unsafe {
                if let Ok(exit) = self.library.get::<ModuleEntry>(c"ExitDll") {
                    exit();
                }
            }
        }
    }
}
