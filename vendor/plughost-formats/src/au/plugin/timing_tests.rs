use super::*;

#[test]
fn timing_poll_does_not_wait_for_a_busy_au_engine() {
    // SAFETY: the tests load only the repository's fixtures and Apple's own units.
    let mut plugin = unsafe { Plugin::new("6175667864656C796170706C") }.unwrap();
    plugin.prepare(&crate::timing_tests::config()).unwrap();
    crate::timing_tests::while_busy(Arc::clone(&plugin.engine), || plugin.poll_timing());
}
