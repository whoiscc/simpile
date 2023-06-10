use std::{
    alloc::{GlobalAlloc, Layout},
    ptr::{copy_nonoverlapping, null_mut, NonNull},
    sync::Mutex,
};

use crate::Space;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Chunk(NonNull<u8>);

impl Chunk {
    const META_MASK: u64 = 0x7;
    const IN_USE_BIT: u32 = 0;
    const LOWER_IN_USE_BIT: u32 = 1;

    // overhead of in-use chunk
    const META_SIZE: usize = 8;
    // 8 bytes prev, 8 bytes next, 8 bytes size
    const MIN_SIZE: usize = Self::META_SIZE + 24;

    unsafe fn get_in_use(&self) -> bool {
        let meta = unsafe { *self.0.cast::<u64>().as_ref() };
        meta & (1 << Self::IN_USE_BIT) != 0
    }

    unsafe fn set_in_use(&mut self, in_use: bool) {
        let meta = unsafe { self.0.cast::<u64>().as_mut() };
        *meta |= (in_use as u64) << Self::IN_USE_BIT;
        if let Some(mut higher) = unsafe { self.get_higher_chunk() } {
            unsafe { higher.set_lower_in_use(in_use) }
        }
    }

    unsafe fn get_lower_in_use(&self) -> bool {
        let meta = unsafe { *self.0.cast::<u64>().as_ref() };
        meta & (1 << Self::LOWER_IN_USE_BIT) != 0
    }

    unsafe fn set_lower_in_use(&mut self, prev_in_use: bool) {
        let meta = unsafe { self.0.cast::<u64>().as_mut() };
        *meta |= (prev_in_use as u64) << Self::LOWER_IN_USE_BIT;
    }

    unsafe fn get_size(&self) -> usize {
        let meta = unsafe { *self.0.cast::<u64>().as_ref() };
        (meta & !Self::META_MASK) as _
    }

    unsafe fn set_size(&mut self, size: usize) {
        let meta = unsafe { self.0.cast::<u64>().as_mut() };
        debug_assert_eq!(size as u64 & Self::META_MASK, 0);
        *meta = (*meta & Self::META_MASK) | (size as u64);
        // not necessary for a chunk that is about to get allocated, hope not too expensive
        debug_assert!(size >= Self::MIN_SIZE);
        unsafe { *self.0.as_ptr().add(size - 8).cast::<u64>() = size as _ }
    }

    unsafe fn get_prev(&self) -> Option<Self> {
        NonNull::new(unsafe { *(self.0.as_ptr().offset(8).cast::<*mut u8>()) }).map(Self)
    }

    unsafe fn set_prev(&mut self, prev: Option<Self>) {
        let prev = prev.map(|chunk| chunk.0.as_ptr()).unwrap_or_else(null_mut);
        unsafe { *(self.0.as_ptr().offset(8).cast::<*mut u8>()) = prev }
    }

    unsafe fn get_next(&self) -> Option<Self> {
        NonNull::new(unsafe { *(self.0.as_ptr().offset(16).cast::<*mut u8>()) }).map(Self)
    }

    // while `set_prev` can be called with `None` as `prev` on every chunk, i.e. every chunk can be
    // the first chunk, `set_next` should maintain an extra invariant that `next` can only be `None`
    // when `self` is the (new) top chunk, which should only happen when the original top chunk get
    // splitted or coalescing
    // `Chunk` does not have necessary state to sanity check on this, consider to find a way for
    // this if necessary
    unsafe fn set_next(&mut self, next: Option<Self>) {
        let next = next.map(|chunk| chunk.0.as_ptr()).unwrap_or_else(null_mut);
        unsafe { *(self.0.as_ptr().offset(16).cast::<*mut u8>()) = next }
    }

    unsafe fn get_user_data(&self, layout: Layout) -> Option<NonNull<u8>> {
        // or .add(Self::META_SIZE)
        let addr = unsafe { self.0.as_ptr().offset(8) };
        if layout.size() + addr.align_offset(layout.align())
            > unsafe { self.get_size() } - Self::META_SIZE
        {
            None
        } else {
            Some(NonNull::new(unsafe { addr.add(addr.align_offset(layout.align())) }).unwrap())
        }
    }

    unsafe fn from_user_data(user_data: *mut u8) -> Self {
        let meta = Self(NonNull::new(unsafe { user_data.offset(-8) }).unwrap());
        if unsafe { meta.get_in_use() } {
            meta
        } else {
            // alignment padding indicator, which should set all meta bits to 0
            debug_assert!(!unsafe { meta.get_lower_in_use() });
            Self(NonNull::new(unsafe { meta.0.as_ptr().sub(meta.get_size()) }).unwrap())
        }
    }

    unsafe fn split(&mut self, layout: Layout) -> (Option<NonNull<u8>>, Option<Self>) {
        let Some(user_data) = (unsafe { self.get_user_data(layout) }) else {
            return (None, None);
        };

        let padding = unsafe { user_data.as_ptr().offset(-8) };
        let padding_size = unsafe { padding.offset_from(self.0.as_ptr()) };
        debug_assert!(padding_size >= 0);
        if padding_size != 0 {
            // which also clear meta bits
            debug_assert_eq!(padding_size as u64 & Self::META_MASK, 0);
            unsafe { *(padding as *mut u64) = padding_size as _ }
        }

        let remain_size;
        let new_size = padding_size as usize + layout.size();
        unsafe {
            debug_assert!(self.get_size() >= new_size);
            remain_size = self.get_size() - new_size;
            self.set_size(new_size)
        }

        let remain = unsafe { user_data.as_ptr().add(layout.size()) };
        let remain = if remain_size >= Self::MIN_SIZE {
            let mut remain = Self(NonNull::new(remain).unwrap());
            unsafe {
                remain.set_in_use(false);
                remain.set_size(remain_size as _);
            }
            Some(remain)
        } else {
            None
        };
        (Some(user_data), remain)
    }

    unsafe fn get_free_lower_chunk(&self) -> Option<Self> {
        if unsafe { self.get_lower_in_use() } {
            None
        } else {
            let lower_size = unsafe { *self.0.as_ptr().offset(-8).cast::<u64>() };
            Some(Self(
                NonNull::new(unsafe { self.0.as_ptr().sub(lower_size as _) }).unwrap(),
            ))
        }
    }

    unsafe fn get_higher_chunk(&self) -> Option<Self> {
        // define the top (i.e. highest) chunk to have the largest size, so it is always also the
        // last chunk and has no next chunk
        if unsafe { self.get_next().is_none() } {
            None
        } else {
            Some(Self(
                NonNull::new(unsafe { self.0.as_ptr().add(self.get_size()) }).unwrap(),
            ))
        }
    }

    unsafe fn get_free_higher_chunk(&self) -> Option<Self> {
        unsafe { self.get_higher_chunk().filter(|chunk| !chunk.get_in_use()) }
    }

    unsafe fn coalesce(&mut self, chunk: Self) {
        debug_assert_eq!(unsafe { self.get_free_higher_chunk() }, Some(chunk));
        unsafe { self.set_size(self.get_size() + chunk.get_size()) }
    }
}

struct Overlay(pub NonNull<u8>);

impl Overlay {
    const EXACT_BINS_LEN: usize = 32;
    const SORTED_BINS_LEN: usize = 64;
    const BINS_LEN: usize = Self::EXACT_BINS_LEN + Self::SORTED_BINS_LEN;

    const MIN_USER_SIZE: usize = Chunk::MIN_SIZE - Chunk::META_SIZE;

    unsafe fn start_chunk(&self) -> Chunk {
        // 32 exact bins and 64 sorted bins
        Chunk(NonNull::new(unsafe { self.0.as_ptr().add(8 * Self::BINS_LEN) }).unwrap())
    }

    unsafe fn get_bin_chunk(&self, index: usize) -> Option<Chunk> {
        let chunk_addr = unsafe { *(self.0.as_ptr().add(8 * index) as *mut *mut u8) };
        NonNull::new(chunk_addr).map(Chunk)
    }

    unsafe fn set_bin_chunk(&mut self, index: usize, chunk: Option<Chunk>) {
        let chunk = chunk.map(|chunk| chunk.0.as_ptr()).unwrap_or_else(null_mut);
        unsafe { *(self.0.as_ptr().add(8 * index) as *mut *mut u8) = chunk }
    }

    fn bin_index_of_size(size: usize) -> usize {
        let size = usize::max(size, Self::MIN_USER_SIZE);
        // TODO: currently relaying on exact value of constants
        if (size >> 8) == 0 {
            size / 8
        } else if (size >> 8) >= 0x10000 {
            Self::BINS_LEN - 1
        } else {
            debug_assert_ne!((size >> 8).leading_zeros(), usize::BITS);
            // m = index of most significant bit of (size >> 8), in range 0..16
            let m = (usize::BITS - (size >> 8).leading_zeros() - 1) as usize;
            (m << 2) + ((size >> (m + 6)) & 3)
        }
    }

    // add a chunk that is not the last chunk, i.e. the new top chunk
    // updating top chunk goes into `update_top_chunk`
    unsafe fn add_chunk(&mut self, mut chunk: Chunk) {
        let chunk_size = unsafe { chunk.get_size() };
        // bin is indexed by maximum possible available size for user data
        let index = Self::bin_index_of_size(chunk_size - Chunk::META_SIZE);
        let mut bin_chunk = unsafe { self.get_bin_chunk(index) };
        if bin_chunk.is_none() {
            unsafe { self.set_bin_chunk(index, Some(chunk)) }
            for index in index + 1..Self::BINS_LEN {
                bin_chunk = unsafe { self.get_bin_chunk(index) };
                if bin_chunk.is_some() {
                    break;
                }
            }
        }
        let mut bin_chunk = bin_chunk.expect("top chunk always reachable from bins");
        // oldest first (really?)
        while unsafe { bin_chunk.get_size() } <= chunk_size {
            bin_chunk = if let Some(next_chunk) = unsafe { bin_chunk.get_next() } {
                next_chunk
            } else {
                // even if `chunk` has larger size, `bin_chunk`, which is the top chunk, will always
                // be the last chunk
                debug_assert!(
                    chunk < bin_chunk,
                    "adding {chunk:?} that is higher than the top {bin_chunk:?}"
                );
                break;
            };
        }
        unsafe {
            chunk.set_next(Some(bin_chunk));
            chunk.set_prev(bin_chunk.get_prev());
            bin_chunk.set_prev(Some(chunk));
            if let Some(mut prev_chunk) = chunk.get_prev() {
                prev_chunk.set_next(Some(chunk))
            }
        }
    }

    // remove a chunk that is not the last chunk
    // because the last chunk (also the top chunk) never get removed, we can assume there will
    // always be at least one chunk presenting
    // updating top chunk goes into `update_top_chunk`
    unsafe fn remove_chunk(&mut self, chunk: Chunk) {
        let mut next_chunk;
        unsafe {
            next_chunk = chunk.get_next().expect("top chunk never get removed");
            next_chunk.set_prev(chunk.get_prev());
            if let Some(mut prev_chunk) = chunk.get_prev() {
                prev_chunk.set_next(Some(next_chunk));
            }
        }
        let index = Self::bin_index_of_size(unsafe { chunk.get_size() } - Chunk::META_SIZE);
        if unsafe { self.get_bin_chunk(index) } == Some(chunk) {
            unsafe {
                self.set_bin_chunk(
                    index,
                    if Self::bin_index_of_size(next_chunk.get_size()) == index {
                        Some(next_chunk)
                    } else {
                        None
                    },
                )
            }
        }
    }

    unsafe fn update_top_chunk(&mut self, top: Chunk, mut new_top: Chunk) {
        unsafe {
            new_top.set_next(None);
            new_top.set_prev(top.get_prev());
            if let Some(mut prev_chunk) = top.get_prev() {
                prev_chunk.set_next(Some(new_top));
            }
        }
        let index = Self::bin_index_of_size(usize::MAX);
        if unsafe { self.get_bin_chunk(index) } == Some(top) {
            unsafe { self.set_bin_chunk(index, Some(new_top)) }
        }
    }

    unsafe fn init(&mut self, len: usize) {
        for index in Self::bin_index_of_size(Self::MIN_USER_SIZE)..Self::BINS_LEN {
            unsafe { self.set_bin_chunk(index, None) }
        }
        let mut chunk = unsafe { self.start_chunk() };
        unsafe {
            chunk.set_in_use(false);
            chunk.set_lower_in_use(true); // because there's no lower chunk
            let chunk_size = len - chunk.0.as_ptr().offset_from(self.0.as_ptr()) as usize;
            chunk.set_size(chunk_size);
            chunk.set_prev(None);
            chunk.set_next(None);
            self.set_bin_chunk(Self::bin_index_of_size(chunk_size), Some(chunk));
        }
        // a little bit of best-effort sanity marker for initialized space
        unsafe { *self.0.as_mut() = 0x82 }
    }

    unsafe fn alloc(&mut self, layout: Layout) -> Option<NonNull<u8>> {
        let mut chunk = None;
        for index in Self::bin_index_of_size(layout.size())..Self::BINS_LEN {
            chunk = unsafe { self.get_bin_chunk(index) };
            if chunk.is_some() {
                break;
            }
        }
        let mut chunk = chunk.expect("top chunk always reachable from bins");
        let (mut user_data, mut remain) = unsafe { chunk.split(layout) };
        while user_data.is_none() {
            chunk = unsafe { chunk.get_next() }?;
            (user_data, remain) = unsafe { chunk.split(layout) };
        }

        if unsafe { chunk.get_next() }.is_some() {
            unsafe {
                self.remove_chunk(chunk);
                if let Some(remain) = remain {
                    self.add_chunk(remain)
                }
            }
        } else if let Some(remain) = remain {
            unsafe {
                self.update_top_chunk(chunk, remain);
                self.remove_chunk(chunk);
            }
        } else {
            // the case where the top chunk is almost the same size as requested allocation
            // i guess the algorithm cannot run if the top chunk is not free, so let's take the slow
            // path panelty here
            return None;
        }

        unsafe { chunk.set_in_use(true) }
        user_data
    }

    unsafe fn dealloc(&mut self, user_data: *mut u8) {
        let mut chunk = unsafe { Chunk::from_user_data(user_data) };
        unsafe { chunk.set_in_use(false) }
        // coalesce with lower neighbor first to prevent update top chunk twice
        if let Some(mut free_lower) = unsafe { chunk.get_free_lower_chunk() } {
            unsafe {
                self.remove_chunk(free_lower);
                free_lower.coalesce(chunk);
                chunk = free_lower;
            }
        }
        if let Some(free_higher) = unsafe { chunk.get_free_higher_chunk() } {
            if unsafe { free_higher.get_next() }.is_some() {
                unsafe {
                    self.remove_chunk(free_higher);
                    chunk.coalesce(free_higher);
                    self.add_chunk(chunk);
                }
            } else {
                unsafe {
                    self.update_top_chunk(free_higher, chunk);
                    self.remove_chunk(free_higher);
                    chunk.coalesce(free_higher);
                }
            }
        } else {
            unsafe { self.add_chunk(chunk) }
        }
    }

    unsafe fn realloc(
        &mut self,
        user_data: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> Option<NonNull<u8>> {
        let mut chunk = unsafe { Chunk::from_user_data(user_data) };
        // also falling back for the top chunk since it does not have higher chunk
        let Some(mut free_higher) = (unsafe { chunk.get_free_higher_chunk() }) else {
            return None;
        };
        unsafe {
            // leveraging the fact that `coalesce` only modify `chunk`'s size (in both header and
            // footer), i.e. re`set_size` on both chunks will revert the coalescing
            // feels a little bit hachy :|
            let chunk_size = chunk.get_size();
            let higher_chunk_size = free_higher.get_size();
            chunk.coalesce(free_higher);
            if let (Some(user_data), remain) =
                chunk.split(Layout::from_size_align(new_size, layout.align()).unwrap())
            {
                self.remove_chunk(chunk);
                if let Some(remain) = remain {
                    self.add_chunk(remain);
                }
                Some(user_data)
            } else {
                chunk.set_size(chunk_size);
                free_higher.set_size(higher_chunk_size);
                None
            }
        }
    }

    unsafe fn alloc_in_space<S: Space>(space: &mut S, layout: Layout) -> *mut u8 {
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut overlay = Self(space.first_mut().unwrap().into());
        if let Some(user_data) = unsafe { overlay.alloc(layout) } {
            user_data.as_ptr()
        } else {
            let new_size = (space.len() + layout.size() + layout.align() + Chunk::META_SIZE)
                .next_power_of_two();
            if space.set_size(new_size) {
                unsafe { overlay.alloc(layout) }.unwrap().as_ptr()
            } else {
                null_mut()
            }
        }
    }

    unsafe fn dealloc_in_space<S: Space>(space: &mut S, user_data: *mut u8) {
        debug_assert_eq!(space.first(), Some(&0x82));
        unsafe { Self(space.first_mut().unwrap().into()).dealloc(user_data) }
        // TODO do space shrinking
    }

    unsafe fn realloc_in_space<S: Space>(
        space: &mut S,
        user_data: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> *mut u8 {
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut overlay = Self(space.first_mut().unwrap().into());
        if let Some(user_data) = unsafe { overlay.realloc(user_data, layout, new_size) } {
            return user_data.as_ptr();
        }

        let new_user_data = unsafe {
            Self::alloc_in_space(
                space,
                Layout::from_size_align(new_size, layout.align()).unwrap(),
            )
        };
        if new_user_data.is_null() {
            null_mut()
        } else {
            unsafe {
                copy_nonoverlapping(user_data, new_user_data, layout.size());
                overlay.dealloc(user_data);
            }
            new_user_data
        }
    }
}

pub struct Allocator<S>(Mutex<S>);

impl<S> Allocator<S> {
    pub fn new(mut space: S) -> Self
    where
        S: Space,
    {
        unsafe { Overlay(space.first_mut().unwrap().into()).init(space.len()) };
        Self(Mutex::new(space))
    }
}

unsafe impl<S> GlobalAlloc for Allocator<S>
where
    S: Space,
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut space = loop {
            if let Ok(space) = self.0.try_lock() {
                break space;
            }
        };
        unsafe { Overlay::alloc_in_space(&mut *space, layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        let mut space = loop {
            if let Ok(space) = self.0.try_lock() {
                break space;
            }
        };
        unsafe { Overlay::dealloc_in_space(&mut *space, ptr) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let mut space = loop {
            if let Ok(space) = self.0.try_lock() {
                break space;
            }
        };
        unsafe { Overlay::realloc_in_space(&mut *space, ptr, layout, new_size) }
    }
}
