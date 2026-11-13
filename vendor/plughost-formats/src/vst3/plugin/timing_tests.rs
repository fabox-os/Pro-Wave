use super::*;

#[test]
#[ignore = "needs routing VST3 fixture"]
fn timing_poll_does_not_wait_for_a_busy_vst3_engine() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-plugins/plughost-test-routing.vst3");
    // SAFETY: the tests load only the repository's fixture and SDK sample plugins.
    let module = Rc::new(unsafe { Module::load(&path) }.unwrap());
    let mut plugin = Plugin::new(
        &module,
        "706C7567686F7374526F7574696E6701",
        &HostIdentity::default(),
    )
    .unwrap();
    plugin.prepare(&crate::timing_tests::config()).unwrap();
    crate::timing_tests::while_busy(Arc::clone(&plugin.engine), || plugin.poll_timing());
}
