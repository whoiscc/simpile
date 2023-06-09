use std::{alloc::Layout, ptr::NonNull};

struct Chunk(NonNull<u8>);

impl Chunk {
    unsafe fn get_in_use(&self) -> bool {
        let meta = unsafe { *self.0.cast::<u64>().as_ref() };
        meta & 1 != 0
    }

    unsafe fn set_in_use(&mut self, in_use: bool) {
        let meta = unsafe { self.0.cast::<u64>().as_mut() };
        *meta |= in_use as u64;
    }

    unsafe fn get_lower_in_use(&self) -> bool {
        let meta = unsafe { *self.0.cast::<u64>().as_ref() };
        meta & (1 << 1) != 0
    }

    unsafe fn set_lower_in_use(&mut self, prev_in_use: bool) {
        let meta = unsafe { self.0.cast::<u64>().as_mut() };
        *meta |= (prev_in_use as u64) << 1;
    }

    unsafe fn get_size(&self) -> usize {
        let meta = unsafe { *self.0.cast::<u64>().as_ref() };
        (meta & !0x7) as _
    }

    unsafe fn set_size(&mut self, size: usize) {
        let meta = unsafe { self.0.cast::<u64>().as_mut() };
        debug_assert_eq!(size & 0x7, 0);
        *meta = (*meta & 0x7) | (size as u64);
        // not necessary for a chunk that is about to get allocated, hope not too expensive
        debug_assert!(size >= 32);
        unsafe { *self.0.as_ptr().add(size - 8).cast::<u64>() = size as _ }
    }

    unsafe fn get_prev(&self) -> *mut u8 {
        let prev_addr = unsafe { *(self.0.as_ptr().offset(8) as *const usize) };
        prev_addr as _
    }

    unsafe fn set_prev(&mut self, prev_addr: *mut u8) {
        unsafe { *(self.0.as_ptr().offset(8) as *mut usize) = prev_addr as _ }
    }

    unsafe fn get_next(&self) -> *mut u8 {
        let next_addr = unsafe { *(self.0.as_ptr().offset(16) as *const usize) };
        next_addr as _
    }

    unsafe fn set_next(&mut self, next_addr: *mut u8) {
        unsafe { *(self.0.as_ptr().offset(16) as *mut usize) = next_addr as _ }
    }

    unsafe fn is_top_chunk(&self) -> bool {
        unsafe { self.get_next() }.is_null()
    }

    unsafe fn get_user_data(&self, layout: Layout) -> Option<*mut u8> {
        let addr = unsafe { self.0.as_ptr().offset(8) };
        if layout.size() + addr.align_offset(layout.align()) > self.get_size() - 8 {
            None
        } else {
            Some(addr.add(addr.align_offset(layout.align())))
        }
    }

    unsafe fn from_user_data(user_data: *mut u8) -> Self {
        let meta = Self(NonNull::new(unsafe { user_data.offset(-8) }).unwrap());
        if meta.get_in_use() {
            meta
        } else {
            // alignment padding indicator
            debug_assert!(!meta.get_lower_in_use());
            Self(NonNull::new(unsafe { meta.0.as_ptr().sub(meta.get_size()) }).unwrap())
        }
    }

    unsafe fn split(&mut self, layout: Layout) -> (Option<NonNull<u8>>, Option<Self>) {
        let Some(user_data) = (unsafe { self.get_user_data(layout) }) else {
            return (None, None);
        };
        let padding = unsafe { user_data.offset(-8) };
        let padding_len = padding.offset_from(self.0.as_ptr());
        if padding_len != 0 {
            debug_assert!(padding_len > 0);
            unsafe { *(padding as *mut usize) = padding_len as _ }
        }
        let remain = unsafe { user_data.add(layout.size()) };
        let remain_size = unsafe { self.0.as_ptr().add(self.get_size()).offset_from(remain) };
        debug_assert!(remain_size >= 0);
        let remain = if remain_size >= 32 {
            let mut remain = Self(NonNull::new(remain).unwrap());
            remain.set_in_use(false);
            remain.set_lower_in_use(true); // which is `self`
            remain.set_size(remain_size as _);
            Some(remain)
        } else {
            None
        };
        (Some(NonNull::new(user_data).unwrap()), remain)
    }

    unsafe fn get_free_lower_chunk(&self) -> Option<Self> {
        if self.get_lower_in_use() {
            None
        } else {
            let lower_size = unsafe { *self.0.as_ptr().offset(-8).cast::<u64>() };
            Some(Self(
                NonNull::new(self.0.as_ptr().sub(lower_size as _)).unwrap(),
            ))
        }
    }

    unsafe fn get_free_higher_chunk(&self) -> Option<Self> {
        if self.is_top_chunk() {
            return None;
        }
        let higher_chunk = Self(NonNull::new(self.0.as_ptr().add(self.get_size())).unwrap());
        if higher_chunk.get_in_use() {
            None
        } else {
            Some(higher_chunk)
        }
    }
}

struct Overlay(NonNull<u8>);

impl Overlay {
    unsafe fn start_chunk(&self) -> Chunk {
        // 32 exact bins and 64 sorted bins
        Chunk(NonNull::new(unsafe { self.0.as_ptr().offset(8 * (32 + 64)) }).unwrap())
    }

    unsafe fn bin_chunk(&self, index: usize) -> Option<Chunk> {
        let chunk_addr = unsafe { *(self.0.as_ptr().add(8 * index) as *mut usize) } as *mut u8;
        NonNull::new(chunk_addr).map(Chunk)
    }

    unsafe fn start_chunk_of_size(&self, size: usize) -> Option<Chunk> {
        let size = usize::max(size, 16);
        let chunk_index = if (size >> 8) == 0 {
            size / 8
        } else if (size >> 8) >= 0x10000 {
            32 + 64 - 1
        } else {
            debug_assert_ne!((size >> 8).leading_zeros(), usize::BITS);
            let m = (usize::BITS - (size >> 8).leading_zeros() - 1) as usize;
            (m << 2) + ((size >> (m + 6)) & 3)
        };
        for index in chunk_index..32 + 64 {
            if let Some(chunk) = self.bin_chunk(index) {
                return Some(chunk);
            }
        }
        None
    }
}
