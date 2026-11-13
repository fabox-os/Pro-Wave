use super::*;
use vst3::Steinberg::Vst::{
    IAudioProcessor, IComponent, IEditController, IMidiMapping, INoteExpressionController,
    IParameterFinder, IProcessContextRequirements, IUnitInfo,
};

fn supported(host: &ComWrapper<HostApplication>, iid: &[u8; 16]) -> tresult {
    let support = host.as_com_ref::<IPlugInterfaceSupport>().unwrap();
    let iid: TUID = iid.map(|byte| byte as _);
    unsafe { support.isPlugInterfaceSupported(&iid) }
}

#[test]
fn every_plugin_interface_the_host_calls_is_reported_as_supported() {
    let host = ComWrapper::new(HostApplication {
        name: String::new(),
    });
    for iid in [
        IComponent::IID,
        IAudioProcessor::IID,
        IEditController::IID,
        IConnectionPoint::IID,
        IUnitInfo::IID,
        IMidiMapping::IID,
        IProcessContextRequirements::IID,
        INoteExpressionController::IID,
    ] {
        assert_eq!(supported(&host, &iid), kResultOk);
    }
    assert_eq!(supported(&host, &IParameterFinder::IID), kResultFalse);
}
