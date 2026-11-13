use super::*;
use std::path::Path;

fn plugin() -> Plugin {
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
    plugin
}

fn tail_reads(plugin: &mut Plugin) -> f64 {
    plugin
        .parameters()
        .into_iter()
        .find(|(info, _)| info.id == 15)
        .unwrap()
        .1
        .unwrap()
        * 1_000_000.0
}

#[test]
#[ignore = "needs CLAP thread-check fixture"]
fn timing_poll_does_not_wait_for_a_busy_clap_engine() {
    let plugin = plugin();
    crate::timing_tests::while_busy(Arc::clone(&plugin.engine), || plugin.poll_timing());
}

#[test]
#[ignore = "needs CLAP thread-check fixture"]
fn quiet_polls_skip_native_tail_reads_and_flush_notifications_refresh_the_tail() {
    let mut plugin = plugin();
    let first = plugin.timing().unwrap();
    let before = tail_reads(&mut plugin);
    for _ in 0..100 {
        assert_eq!(plugin.poll_timing(), Some(first));
    }
    assert_eq!(tail_reads(&mut plugin), before);
    plugin.set_parameter(9, 0.5).unwrap();
    plugin.flush_active();
    assert_eq!(plugin.poll_timing().unwrap().tail, Tail::Samples(16));
    assert_eq!(tail_reads(&mut plugin), before + 1.0);
    assert_eq!(plugin.poll_timing().unwrap().tail, Tail::Samples(16));
    assert_eq!(tail_reads(&mut plugin), before + 1.0);
    plugin.timing().unwrap();
    assert_eq!(tail_reads(&mut plugin), before + 2.0);
}
