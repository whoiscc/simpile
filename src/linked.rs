struct Chunk(*mut u8);

impl Chunk {
    unsafe fn get_in_use(&self) -> bool {
        let meta = unsafe { *(self.0 as *const u64) };
        meta & 1 != 0
    }

    unsafe fn set_in_use(&mut self, in_use: bool) {
        let meta = unsafe { &mut *(self.0 as *mut u64) };
        *meta |= in_use as u64;
    }

    unsafe fn get_prev_in_use(&self) -> bool {
        let meta = unsafe { *(self.0 as *const u64) };
        meta & (1 << 1) != 0
    }

    unsafe fn set_prev_in_use(&mut self, prev_in_use: bool) {
        let meta = unsafe { &mut *(self.0 as *mut u64) };
        *meta |= (prev_in_use as u64) << 1;
    }

    unsafe fn get_size(&self) -> usize {
        let meta = unsafe { *(self.0 as *const u64) };
        (meta & !0x7) as _
    }

    unsafe fn set_size(&mut self, size: usize) {
        let meta = unsafe { &mut *(self.0 as *mut u64) };
        debug_assert_eq!(size & 0x7, 0);
        *meta = (*meta & 0x7) | (size as u64);
    }

    unsafe fn get_prev(&self) -> *mut u8 {
        let prev_addr = unsafe { *(self.0.offset(8) as *const usize) };
        prev_addr as _
    }

    unsafe fn set_prev(&mut self, prev_addr: *mut u8) {
        unsafe { *(self.0.offset(8) as *mut usize) = prev_addr as _ }
    }

    unsafe fn get_next(&self) -> *mut u8 {
        let next_addr = unsafe { *(self.0.offset(16) as *const usize) };
        next_addr as _
    }

    unsafe fn set_next(&mut self, next_addr: *mut u8) {
        unsafe { *(self.0.offset(16) as *mut usize) = next_addr as _ }
    }

    unsafe fn is_top_chunk(&self) -> bool {
        unsafe { self.get_next() }.is_null()
    }

    unsafe fn get_user_data(&self) -> *mut u8 {
        unsafe { self.0.offset(8) }
    }

    unsafe fn from_user_data(user_data: *mut u8) -> Self {
        Self(unsafe { user_data.offset(-8) })
    }
}

struct Overlay(*mut u8);

impl Overlay {
    unsafe fn start_chunk(&self) -> Chunk {
        // 32 exact bins and 64 sorted bins
        Chunk(unsafe { self.0.offset(8 * (32 + 64)) })
    }

    unsafe fn bin_chunk(&self, index: usize) -> Option<Chunk> {
        let chunk_addr = unsafe { *(self.0.add(8 * index) as *mut usize) } as *mut u8;
        if chunk_addr.is_null() {
            None
        } else {
            Some(Chunk(chunk_addr as _))
        }
    }

    unsafe fn start_chunk_of_size(&self, size: usize) -> Option<Chunk> {
        let size = usize::max(size, 16);
        let chunk_index = if size / 8 < 32 {
            size / 8
        } else if size >= 0x1000000 {
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
