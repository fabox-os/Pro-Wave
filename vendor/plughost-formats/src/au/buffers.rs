//! A variable-length `AudioBufferList` of non-interleaved channels.

use std::ffi::c_void;
use std::mem::size_of;

use objc2_core_audio_types::{AudioBuffer, AudioBufferList};

pub struct BufferList {
    /// u64 storage keeps the list aligned for its pointer fields.
    storage: Vec<u64>,
    count: usize,
}

impl BufferList {
    pub fn new(count: usize) -> BufferList {
        let bytes =
            size_of::<AudioBufferList>() + count.saturating_sub(1) * size_of::<AudioBuffer>();
        let mut list = BufferList {
            storage: vec![0; bytes.div_ceil(size_of::<u64>())],
            count,
        };
        // SAFETY: the storage is zeroed, aligned, and large enough for `count` buffers.
        unsafe { (*list.as_mut_ptr()).mNumberBuffers = count as u32 };
        list
    }

    pub fn as_mut_ptr(&mut self) -> *mut AudioBufferList {
        self.storage.as_mut_ptr().cast()
    }

    pub fn buffers(&mut self) -> &mut [AudioBuffer] {
        // SAFETY: the storage holds `count` buffers after the header.
        unsafe {
            std::slice::from_raw_parts_mut((*self.as_mut_ptr()).mBuffers.as_mut_ptr(), self.count)
        }
    }

    /// Releases sample pointers and restores the owned list header after a native call.
    pub fn clear(&mut self) {
        unsafe { (*self.as_mut_ptr()).mNumberBuffers = self.count as u32 };
        for buffer in self.buffers() {
            *buffer = AudioBuffer {
                mNumberChannels: 0,
                mDataByteSize: 0,
                mData: std::ptr::null_mut(),
            };
        }
    }

    /// Points buffer `i` at channel `order[i]`, or at channel `i` without an order, and restores
    /// the native-writable header.
    pub fn point_at(&mut self, channels: &mut [&mut [f32]], order: Option<&[usize]>) {
        unsafe { (*self.as_mut_ptr()).mNumberBuffers = self.count as u32 };
        for (index, buffer) in self.buffers().iter_mut().enumerate() {
            let channel = &mut channels[order.map_or(index, |order| order[index])];
            *buffer = AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: std::mem::size_of_val(*channel) as u32,
                mData: channel.as_mut_ptr().cast::<c_void>(),
            };
        }
    }
}

/// The buffers of a list the Audio Unit handed over, for example to a pull-input block.
///
/// # Safety
///
/// `list` must point to a valid `AudioBufferList`.
pub unsafe fn buffers_of<'a>(list: *mut AudioBufferList) -> &'a mut [AudioBuffer] {
    unsafe {
        let count = (*list).mNumberBuffers as usize;
        std::slice::from_raw_parts_mut((*list).mBuffers.as_mut_ptr(), count)
    }
}
