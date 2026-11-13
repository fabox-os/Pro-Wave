use super::*;
use vst3::Steinberg::IBStream_::IStreamSeekMode_::{kIBSeekCur, kIBSeekSet};

#[test]
fn state_stream_refuses_native_overflow_before_copying_and_keeps_failure_sticky() {
    let owner = MemoryStream::bounded(StateKind::Project, 16);
    let stream = owner.as_com_ref::<IBStream>().unwrap();
    let mut bytes = [0x5A; 16];
    let mut written = -1;
    unsafe {
        assert_eq!(
            stream.write(bytes.as_mut_ptr().cast(), 16, &mut written),
            kResultOk
        );
        assert_eq!(written, 16);
        assert_eq!(
            stream.write(bytes.as_mut_ptr().cast(), 1, &mut written),
            kResultFalse
        );
        assert_eq!(written, 0);
        assert_eq!(
            stream.seek(0, kIBSeekSet as i32, std::ptr::null_mut()),
            kResultOk
        );
        assert_eq!(
            stream.write(bytes.as_mut_ptr().cast(), 1, &mut written),
            kResultFalse
        );
    }
    assert!(owner.exceeded());
    assert_eq!(owner.take_bytes(), bytes);
}

#[test]
fn state_stream_bounds_sparse_seeks_and_handles_read_past_end_without_copying() {
    let owner = MemoryStream::bounded(StateKind::Project, 16);
    let stream = owner.as_com_ref::<IBStream>().unwrap();
    let mut byte = 0x5Au8;
    let mut read = -1;
    unsafe {
        assert_eq!(
            stream.seek(8, kIBSeekSet as i32, std::ptr::null_mut()),
            kResultOk
        );
        assert_eq!(
            stream.read((&mut byte as *mut u8).cast(), 1, &mut read),
            kResultOk
        );
        assert_eq!(read, 0);
        assert_eq!(byte, 0x5A);
        assert_eq!(stream.read(std::ptr::null_mut(), 0, &mut read), kResultOk);
        assert_eq!(
            stream.write(std::ptr::null_mut(), 0, std::ptr::null_mut()),
            kResultOk
        );
        assert_eq!(
            stream.seek(i64::MAX, kIBSeekCur as i32, std::ptr::null_mut()),
            kInvalidArgument
        );
    }
    assert!(owner.exceeded());
    assert!(owner.take_bytes().is_empty());
}

#[test]
fn state_stream_transfers_large_native_payload_and_limits_remaining_controller_budget() {
    let owner = MemoryStream::bounded(StateKind::Project, 8 << 20);
    let stream = owner.as_com_ref::<IBStream>().unwrap();
    for byte in 0..128u8 {
        let mut chunk = [byte; 65536];
        let mut written = 0;
        unsafe {
            assert_eq!(
                stream.write(chunk.as_mut_ptr().cast(), chunk.len() as i32, &mut written),
                kResultOk
            );
        }
        assert_eq!(written as usize, chunk.len());
    }
    let component = owner.take_bytes();
    assert_eq!(component.len(), 8 << 20);
    for (index, chunk) in component.as_chunks::<65536>().0.iter().enumerate() {
        assert!(chunk.iter().all(|&byte| byte == index as u8));
    }
    let controller = MemoryStream::bounded(StateKind::Project, (8 << 20) - component.len());
    let mut byte = 0u8;
    unsafe {
        assert_eq!(
            controller.write((&mut byte as *mut u8).cast(), 1, std::ptr::null_mut()),
            kResultFalse
        );
    }
    assert!(controller.exceeded());
    assert!(controller.take_bytes().is_empty());
}
