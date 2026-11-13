//! Independent native-container fixtures, also used as libFuzzer seeds.
#[cfg(feature = "vst3")]
#[test]
fn corpus_vstpreset_keeps_native_chunks_and_rejects_invalid_ranges() {
    use crate::vst3::Preset;
    let bytes = include_bytes!("corpus/vstpreset/three-chunks.vstpreset");
    let preset = Preset::from_bytes(bytes).unwrap();
    assert_eq!(preset.class_id, "0123456789ABCDEF0123456789ABCDEF");
    assert_eq!(preset.component, [0x10, 0x20, 0x30]);
    assert_eq!(preset.controller.as_deref(), Some(&[0x40, 0x50][..]));
    assert_eq!(preset.info, b"<Meta/>");
    assert_eq!(preset.to_bytes().unwrap(), bytes);
    for bytes in [
        &include_bytes!("corpus/vstpreset/truncated.vstpreset")[..],
        include_bytes!("corpus/vstpreset/negative-table.vstpreset"),
        include_bytes!("corpus/vstpreset/overlap.vstpreset"),
        include_bytes!("corpus/vstpreset/duplicate.vstpreset"),
    ] {
        assert!(Preset::from_bytes(bytes).is_err());
    }
}

#[cfg(all(feature = "au", target_os = "macos"))]
#[test]
fn corpus_au_plists_preserve_identity_and_reject_noninteger_fourccs() {
    use crate::au::{from_preset, inspect_preset, to_preset};
    objc2::rc::autoreleasepool(|_| {
        for bytes in [
            &include_bytes!("corpus/au_plist/valid.xml")[..],
            include_bytes!("corpus/au_plist/valid.binary"),
        ] {
            let info = inspect_preset(bytes).unwrap();
            assert_eq!(info.class_id, "6175667864656C796170706C");
            assert_eq!(
                info.metadata,
                plughost_core::PresetMetadata::AudioUnit {
                    name: Some("Corpus & delay".into())
                }
            );
            let state = from_preset(bytes).unwrap();
            assert_eq!(inspect_preset(&to_preset(&state).unwrap()).unwrap(), info);
        }
        for bytes in [
            &include_bytes!("corpus/au_plist/boolean.xml")[..],
            include_bytes!("corpus/au_plist/fraction.xml"),
            include_bytes!("corpus/au_plist/overflow.xml"),
            include_bytes!("corpus/au_plist/truncated.binary"),
            include_bytes!("corpus/au_plist/invalid-string.binary"),
            include_bytes!("corpus/au_plist/array.xml"),
        ] {
            assert!(inspect_preset(bytes).is_err());
            assert!(from_preset(bytes).is_err());
        }
    });
}

#[cfg(all(feature = "au", target_os = "macos"))]
#[test]
fn corpus_au_identity_inspection_does_not_guarantee_encodable_payload() {
    let bytes = include_bytes!("corpus/au_plist/null-payload.binary");
    objc2::rc::autoreleasepool(|_| {
        assert_eq!(
            crate::au::inspect_preset(bytes).unwrap().class_id,
            "6175667864656C796170706C"
        );
        // Binary plist nulls can be read by Foundation but are not writable property-list values.
        assert!(crate::au::from_preset(bytes).is_err());
    });
}
