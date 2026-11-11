use super::*;
use std::ptr::{null, null_mut};

#[test]
fn zero_length_null_binary_round_trips_through_the_native_interface() {
    let owner = ComWrapper::new(AttributeList::default());
    let attributes = owner.as_com_ref::<IAttributeList>().unwrap();
    let (mut data, mut size) = (null(), 9);
    unsafe {
        assert_eq!(
            attributes.setBinary(c"empty".as_ptr(), null(), 0),
            kResultOk
        );
        assert_eq!(
            attributes.getBinary(c"empty".as_ptr(), &mut data, &mut size),
            kResultOk
        );
    }
    assert_eq!(size, 0);
}

#[test]
fn null_attribute_ids_and_destinations_are_rejected_without_changing_values() {
    let owner = ComWrapper::new(AttributeList::default());
    let attributes = owner.as_com_ref::<IAttributeList>().unwrap();
    let (mut integer, mut float, mut size, mut data) = (71, 0.25, 9, null());
    let text = [65u16, 0];
    let mut output = [0u16; 2];
    let binary = [1u8, 2];
    unsafe {
        assert_eq!(attributes.setInt(c"int".as_ptr(), 12), kResultOk);
        assert_eq!(attributes.setFloat(c"float".as_ptr(), 0.75), kResultOk);
        assert_eq!(
            attributes.setString(c"text".as_ptr(), text.as_ptr()),
            kResultOk
        );
        assert_eq!(
            attributes.setBinary(c"bytes".as_ptr(), binary.as_ptr().cast(), 2),
            kResultOk
        );
        assert_eq!(attributes.getInt(null(), &mut integer), kInvalidArgument);
        assert_eq!(attributes.getFloat(null(), &mut float), kInvalidArgument);
        assert_eq!(
            attributes.getString(null(), output.as_mut_ptr(), 4),
            kInvalidArgument
        );
        assert_eq!(
            attributes.getBinary(null(), &mut data, &mut size),
            kInvalidArgument
        );
        assert_eq!((integer, float, size, data), (71, 0.25, 9, null()));
        assert_eq!(
            attributes.getInt(c"int".as_ptr(), null_mut()),
            kInvalidArgument
        );
        assert_eq!(
            attributes.getFloat(c"float".as_ptr(), null_mut()),
            kInvalidArgument
        );
        assert_eq!(
            attributes.getString(c"text".as_ptr(), null_mut(), 4),
            kInvalidArgument
        );
        assert_eq!(
            attributes.getBinary(c"bytes".as_ptr(), null_mut(), &mut size),
            kInvalidArgument
        );
        assert_eq!(
            attributes.getBinary(c"bytes".as_ptr(), &mut data, null_mut()),
            kInvalidArgument
        );
        assert_eq!(attributes.setInt(null(), 0), kInvalidArgument);
        assert_eq!(attributes.setFloat(null(), 0.0), kInvalidArgument);
        assert_eq!(
            attributes.setString(null(), text.as_ptr()),
            kInvalidArgument
        );
        assert_eq!(
            attributes.setString(c"text".as_ptr(), null()),
            kInvalidArgument
        );
        assert_eq!(
            attributes.setBinary(null(), binary.as_ptr().cast(), 2),
            kInvalidArgument
        );
        assert_eq!(
            attributes.setBinary(c"bytes".as_ptr(), null(), 2),
            kInvalidArgument
        );
        assert_eq!(attributes.getInt(c"int".as_ptr(), &mut integer), kResultOk);
        assert_eq!(
            attributes.getFloat(c"float".as_ptr(), &mut float),
            kResultOk
        );
        assert_eq!(
            attributes.getString(c"text".as_ptr(), output.as_mut_ptr(), 4),
            kResultOk
        );
        assert_eq!(
            attributes.getBinary(c"bytes".as_ptr(), &mut data, &mut size),
            kResultOk
        );
        assert_eq!((integer, float, output, size), (12, 0.75, text, 2));
        assert_eq!(
            std::slice::from_raw_parts(data.cast::<u8>(), size as usize),
            binary
        );
    }
}
