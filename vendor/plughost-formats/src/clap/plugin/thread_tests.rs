use super::*;
use std::path::Path;

#[test]
#[ignore = "needs CLAP thread-check fixture"]
fn a_native_call_on_the_wrong_thread_is_counted_and_logged() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-plugins/plughost-test-thread-check.clap");
    // SAFETY: the tests load only the repository's fixture plugins.
    let entry = unsafe { super::super::Entry::load(&path) }.unwrap();
    let mut plugin = Plugin::new(
        &entry,
        "com.studio.plughost.test-delay.7900000f",
        &HostIdentity::default(),
    )
    .unwrap();
    plugin.prepare(&crate::timing_tests::config()).unwrap();
    let reports = |plugin: &mut Plugin| {
        let parameters = plugin.parameters();
        let value = |id| parameters.iter().find(|(info, _)| info.id == id).unwrap().1;
        assert_eq!(value(16), Some(1.0));
        assert_eq!(value(17), Some(1.0));
        assert!(value(18) > Some(0.0));
        (value(19).unwrap() * 1_000_000.0).round() as u32
    };
    plugin.reset().unwrap();
    assert_eq!(reports(&mut plugin), 0, "{:?}", plugin.take_diagnostics());
    {
        let mut engine = lock(&plugin.engine);
        let raw = engine
            .processor
            .as_mut()
            .unwrap()
            .plugin_handle()
            .as_raw_ptr();
        // SAFETY: this fixture is activated but stopped, with exclusive engine access. Skip
        // the host's audio-thread context deliberately so the fixture detects a wrong call.
        unsafe { (*raw).reset.unwrap()(raw) };
    }
    assert_eq!(reports(&mut plugin), 1);
    let wrong: Vec<_> = plugin
        .take_diagnostics()
        .records
        .into_iter()
        .filter(|record| record.message.starts_with("called on the wrong thread: "))
        .collect();
    assert_eq!(wrong.len(), 1);
    assert_eq!(
        wrong[0].severity,
        plughost_core::DiagnosticSeverity::HostMisbehaving
    );
    assert_eq!(wrong[0].message, "called on the wrong thread: reset");
}
