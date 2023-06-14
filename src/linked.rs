#![cfg_attr(not(any(dev, test)), allow(clippy::unit_arg, clippy::unit_cmp))]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    fmt::Debug,
    ptr::{copy_nonoverlapping, null_mut, NonNull},
    sync::{
        atomic::{AtomicBool, Ordering::SeqCst},
        Mutex, MutexGuard,
        TryLockError::WouldBlock,
    },
    thread::panicking,
};

use crate::Space;

// invariants:
// chunk.ptr < chunk.limit (to be exact, chunk.ptr + CHUNK::MIN_SIZE <= chunk.limit)
// if chunk1 and chunk2 belong to the same heap, then chunk1.limit == chunk2.limit
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Chunk {
    data: NonNull<u8>,
    #[cfg(any(dev, test))]
    limit: NonNull<u8>,
    #[cfg(not(any(dev, test)))]
    limit: (),
}

impl Chunk {
    const META_MASK: u64 = 0x7;
    const IN_USE_BIT: u32 = 0;
    const LOWER_IN_USE_BIT: u32 = 1;

    // overhead of in-use chunk
    const META_SIZE: usize = 8;
    // 8 bytes prev, 8 bytes next, 8 bytes size
    const MIN_SIZE: usize = Self::META_SIZE + 24;

    fn new(
        data: NonNull<u8>,
        #[cfg(any(dev, test))] limit: NonNull<u8>,
        #[cfg(not(any(dev, test)))] limit: (),
    ) -> Self {
        #[cfg(any(dev, test))]
        debug_assert!(data < limit, "expect {data:?} < {limit:?}");
        Self { data, limit }
    }

    unsafe fn get_in_use(&self) -> bool {
        let meta = unsafe { *self.data.cast::<u64>().as_ref() };
        meta & (1 << Self::IN_USE_BIT) != 0
    }

    unsafe fn set_in_use(&mut self, in_use: bool) {
        let prev_in_use = unsafe { self.get_in_use() };
        let meta = unsafe { self.data.cast::<u64>().as_mut() };
        *meta = (*meta & !(1 << Self::IN_USE_BIT)) | ((in_use as u64) << Self::IN_USE_BIT);
        if unsafe { prev_in_use || in_use || !self.is_top() } {
            unsafe { self.get_higher_chunk().set_lower_in_use(in_use) }
        }
    }

    unsafe fn get_lower_in_use(&self) -> bool {
        let meta = unsafe { *self.data.cast::<u64>().as_ref() };
        meta & (1 << Self::LOWER_IN_USE_BIT) != 0
    }

    unsafe fn set_lower_in_use(&mut self, lower_in_use: bool) {
        let meta = unsafe { self.data.cast::<u64>().as_mut() };
        *meta = (*meta & !(1 << Self::LOWER_IN_USE_BIT))
            | ((lower_in_use as u64) << Self::LOWER_IN_USE_BIT);
    }

    unsafe fn get_size(&self) -> usize {
        let meta = unsafe { *self.data.cast::<u64>().as_ref() };
        (meta & !Self::META_MASK) as _
    }

    unsafe fn set_size(&mut self, size: usize) {
        let meta = unsafe { self.data.cast::<u64>().as_mut() };
        debug_assert_eq!(size as u64 & Self::META_MASK, 0);
        *meta = (*meta & Self::META_MASK) | (size as u64);
        // not necessary for a chunk that is about to be allocated, hope not too expensive
        debug_assert!(size >= Self::MIN_SIZE);
        unsafe { *self.data.as_ptr().add(size - 8).cast::<u64>() = size as _ }
    }

    unsafe fn get_prev(&self) -> Option<Self> {
        debug_assert!(unsafe { !self.get_in_use() });
        NonNull::new(unsafe { *(self.data.as_ptr().offset(8).cast::<*mut u8>()) })
            .map(|data| Self::new(data, self.limit))
    }

    unsafe fn set_prev(&mut self, prev: Option<Self>) {
        debug_assert!(unsafe { !self.get_in_use() });
        let prev = prev
            .map(|chunk| {
                debug_assert_eq!(chunk.limit, self.limit);
                chunk.data.as_ptr()
            })
            .unwrap_or_else(null_mut);
        unsafe { *(self.data.as_ptr().offset(8).cast::<*mut u8>()) = prev }
    }

    unsafe fn get_next(&self) -> Option<Self> {
        debug_assert!(unsafe { !self.get_in_use() });
        NonNull::new(unsafe { *(self.data.as_ptr().offset(16).cast::<*mut u8>()) })
            .map(|data| Self::new(data, self.limit))
    }

    // while `set_prev` can be called with `None` as `prev` on every chunk, i.e. every chunk can be
    // the first chunk, `set_next` should maintain an extra invariant that `next` can only be `None`
    // when `self` is the (new) top chunk, which should only happen when the original top chunk get
    // splitted or coalescing
    // `Chunk` does not have necessary state to sanity check on this, consider to find a way for
    // this if necessary
    unsafe fn set_next(&mut self, next: Option<Self>) {
        debug_assert!(unsafe { !self.get_in_use() });
        let next = next
            .map(|chunk| {
                debug_assert_eq!(chunk.limit, self.limit);
                chunk.data.as_ptr()
            })
            .unwrap_or_else(null_mut);
        unsafe { *(self.data.as_ptr().offset(16).cast::<*mut u8>()) = next }
    }

    unsafe fn is_top(&self) -> bool {
        // may be a little bit limited, but always safe first
        debug_assert!(unsafe { !self.get_in_use() });
        // define the top (i.e. highest) chunk to have the largest size, so it is always also the
        // last chunk and has no next chunk
        unsafe { self.get_next().is_none() }
    }

    unsafe fn get_user_data(&self, layout: Layout) -> Option<NonNull<u8>> {
        // or .add(Self::META_SIZE)
        let addr = unsafe { self.data.as_ptr().offset(8) };
        let align_offset = addr.align_offset(layout.align());
        if layout.size() + align_offset > unsafe { self.get_size() } - Self::META_SIZE {
            None
        } else {
            Some(NonNull::new(unsafe { addr.add(align_offset) }).unwrap())
        }
    }

    unsafe fn from_user_data(
        user_data: *mut u8,
        #[cfg(any(dev, test))] limit: NonNull<u8>,
        #[cfg(not(any(dev, test)))] limit: (),
    ) -> Self {
        let mut chunk = Self::new(
            NonNull::new(unsafe { user_data.offset(-8) }).unwrap(),
            limit,
        );
        if unsafe { !chunk.get_in_use() } {
            // alignment padding indicator, which should set all meta bits to 0
            debug_assert!(!unsafe { chunk.get_lower_in_use() });
            // data can only decrease so will not be over limit after this
            chunk.data =
                NonNull::new(unsafe { chunk.data.as_ptr().sub(chunk.get_size()) }).unwrap();
        }
        chunk
    }

    unsafe fn split(&mut self, layout: Layout) -> (Option<NonNull<u8>>, Option<Self>) {
        let Some(user_data) = (unsafe { self.get_user_data(layout) }) else {
            return (None, None);
        };
        // println!("{user_data:?}");

        let padding_size = unsafe {
            user_data
                .as_ptr()
                .offset(-8)
                .offset_from(self.data.as_ptr())
        };
        debug_assert!(padding_size >= 0);
        // the padding indicator will only be writen after chain updated, or pointers may get
        // corrupted by this
        let mut new_size = usize::max(
            Chunk::META_SIZE + padding_size as usize + layout.size(),
            Self::MIN_SIZE,
        );
        if new_size % 8 != 0 {
            new_size += 8 - new_size % 8;
        }
        // println!("new size {new_size}");
        let remain_size;
        unsafe {
            debug_assert!(self.get_size() >= new_size);
            remain_size = self.get_size() - new_size;
            self.set_size(new_size);
        }

        // cannot just `get_higher_chunk` because we may be splitting the top chunk
        let remain = if remain_size < Self::MIN_SIZE {
            unsafe { self.set_size(new_size + remain_size) }
            None
        } else {
            let mut remain = Self::new(
                NonNull::new(unsafe { self.data.as_ptr().add(new_size) }).unwrap(),
                self.limit,
            );
            unsafe {
                remain.set_in_use(false);
                remain.set_size(remain_size);
            }
            Some(remain)
        };
        (Some(user_data), remain)
    }

    unsafe fn get_free_lower_chunk(&self) -> Option<Self> {
        if unsafe { self.get_lower_in_use() } {
            None
        } else {
            let lower_size = unsafe { *self.data.as_ptr().offset(-8).cast::<u64>() };
            Some(Self::new(
                NonNull::new(unsafe { self.data.as_ptr().sub(lower_size as _) }).unwrap(),
                self.limit,
            ))
        }
    }

    unsafe fn get_higher_chunk(&self) -> Self {
        Self::new(
            NonNull::new(unsafe { self.data.as_ptr().add(self.get_size()) }).unwrap(),
            self.limit,
        )
    }

    // not assert `self` is not top chunk because
    // 1. this method will be called by the previous top chunk, which still
    // "looks like" a top chunk when calling
    // 2. this method is only used by de/reallocation, and the (current) top chunk is never
    // allocated, so never get de/reallocated
    // the assertion in `get_higher_chunk` make sure the "real" top chunk cannot call this
    unsafe fn get_free_higher_chunk(&self) -> Option<Self> {
        unsafe { Some(self.get_higher_chunk()).filter(|chunk| !chunk.get_in_use()) }
    }

    unsafe fn coalesce(&mut self, chunk: Self) {
        debug_assert!(unsafe { !chunk.is_top() });
        debug_assert_eq!(unsafe { self.get_free_higher_chunk() }, Some(chunk));
        unsafe {
            self.set_size(self.get_size() + chunk.get_size());
            self.set_in_use(self.get_in_use()) // update lower_in_use on higher chunk
        }
    }
}

impl Debug for Chunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut meta = Vec::new();
        if unsafe { self.get_in_use() } {
            meta.push(String::from("in_use"));
        } else {
            meta.push(format!("prev = {:?}", unsafe {
                self.get_prev().map(|chunk| chunk.data)
            }));
            meta.push(format!("next = {:?}", unsafe {
                self.get_next().map(|chunk| chunk.data)
            }));
        }
        if unsafe { self.get_lower_in_use() } {
            meta.push(String::from("lower_in_use"));
        }
        write!(
            f,
            "Chunk({:?}, size = {}, {})",
            self.data.as_ptr(),
            unsafe { self.get_size() },
            meta.join(", ")
        )
    }
}

// the overlay over some `Space`
struct Overlay {
    space: NonNull<u8>,
    #[cfg(any(dev, test))]
    limit: NonNull<u8>,
    #[cfg(not(any(dev, test)))]
    limit: (),
}

impl Overlay {
    const EXACT_BINS_LEN: usize = 32;
    const SORTED_BINS_LEN: usize = 64;
    const BINS_LEN: usize = Self::EXACT_BINS_LEN + Self::SORTED_BINS_LEN;

    const MIN_USER_SIZE: usize = Chunk::MIN_SIZE - Chunk::META_SIZE;

    unsafe fn start_chunk(&self) -> Chunk {
        Chunk::new(
            NonNull::new(unsafe { self.space.as_ptr().add(8 * Self::BINS_LEN) }).unwrap(),
            self.limit,
        )
    }

    unsafe fn get_bin_chunk(&self, index: usize) -> Option<Chunk> {
        let chunk_addr = unsafe { *(self.space.as_ptr().add(8 * index).cast()) };
        NonNull::new(chunk_addr).map(|data| Chunk::new(data, self.limit))
    }

    unsafe fn set_bin_chunk(&mut self, index: usize, chunk: Option<Chunk>) {
        let chunk = chunk
            .map(|chunk| {
                debug_assert_eq!(chunk.limit, self.limit);
                chunk.data.as_ptr()
            })
            .unwrap_or_else(null_mut);
        unsafe { *(self.space.as_ptr().add(8 * index).cast()) = chunk }
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
            Self::EXACT_BINS_LEN + (m << 2) + ((size >> (m + 6)) & 3)
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
                    "adding {chunk:?} that is not lower than the top {bin_chunk:?}"
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
    // additionally, the old top chunk is also not removed here
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

    // sematic equal to `remove_chunk(top)` + `add_chunk(new_top)`
    unsafe fn update_top_chunk(&mut self, top: Chunk, mut new_top: Chunk) {
        // println!("update top {top:?} -> {new_top:?}");
        unsafe {
            new_top.set_next(None);
            new_top.set_prev(top.get_prev());
            // println!("update prev {:?}", top.get_prev());
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
        assert!(len >= 8 * Self::BINS_LEN + Chunk::MIN_SIZE * 2);
        assert_eq!(len % 8, 0);

        for index in Self::bin_index_of_size(Self::MIN_USER_SIZE)..Self::BINS_LEN {
            unsafe { self.set_bin_chunk(index, None) }
        }
        unsafe {
            let mut chunk = self.start_chunk();
            let chunk_size = self
                .space
                .as_ptr()
                .add(len)
                .offset_from(chunk.data.as_ptr()) as usize
                // save space for the top chunk
                - Chunk::MIN_SIZE;
            chunk.set_size(chunk_size);
            chunk.set_in_use(false);
            chunk.set_lower_in_use(true); // because there's no lower chunk

            let mut top_chunk = chunk.get_higher_chunk();
            top_chunk.set_size(Chunk::MIN_SIZE);
            top_chunk.set_in_use(false);
            top_chunk.set_next(None);
            top_chunk.set_prev(None);
            self.set_bin_chunk(Self::bin_index_of_size(usize::MAX), Some(top_chunk));

            self.add_chunk(chunk);
        }

        unsafe {
            // a little bit of best-effort sanity marker for initialized space
            *self.space.as_mut() = 0x82;
            self.sanity_check()
        }
    }

    // extract this subroutine for reusing in test helper
    unsafe fn find_smallest(&self, min_size: usize) -> Chunk {
        let mut chunk = None;
        for index in Self::bin_index_of_size(min_size)..Self::BINS_LEN {
            chunk = unsafe { self.get_bin_chunk(index) };
            if chunk.is_some() {
                break;
            }
        }
        chunk.expect("top chunk always reachable from bins")
    }

    unsafe fn alloc(&mut self, layout: Layout) -> Result<NonNull<u8>, Chunk> {
        if layout.size() == 0 {
            return Ok(NonNull::dangling()); // feels like better than null?
        }

        let mut chunk = unsafe { self.find_smallest(layout.size()) };
        // println!("{chunk:?}");
        let mut user_data = unsafe { chunk.get_user_data(layout) };
        while user_data.is_none() {
            chunk = if let Some(next_chunk) = unsafe { chunk.get_next() } {
                next_chunk
            } else {
                return Err(chunk);
            };
            user_data = unsafe { chunk.get_user_data(layout) };
        }
        // println!("{chunk:?} {user_data:?} {remain:?}");
        debug_assert!(unsafe { chunk.get_size() } >= layout.size() + Chunk::META_SIZE);

        if unsafe { chunk.is_top() } {
            return Err(chunk); // top chunk is never used
        } else {
            unsafe {
                // println!("{chunk:?}");
                self.remove_chunk(chunk);
                // user data will be the same
                let (_, remain) = chunk.split(layout);
                if let Some(remain) = remain {
                    self.add_chunk(remain)
                }
            }
        }

        // println!("{chunk:?}");
        unsafe { chunk.set_in_use(true) }

        let user_data = user_data.unwrap();
        // a little duplication to `split`
        let padding = unsafe { user_data.as_ptr().offset(-8) };
        let padding_size = unsafe { padding.offset_from(chunk.data.as_ptr()) } as usize;
        if padding_size != 0 {
            // println!("padding size {padding_size}");
            debug_assert_eq!(padding_size as u64 & Chunk::META_MASK, 0); // which also clear meta bits
            unsafe { *padding.cast::<u64>() = padding_size as _ }
        }
        Ok(user_data)
    }

    unsafe fn dealloc(&mut self, user_data: *mut u8) {
        let mut chunk = unsafe { Chunk::from_user_data(user_data, self.limit) };
        // println!("{chunk:?}");

        if let Some(mut free_lower) = unsafe { chunk.get_free_lower_chunk() } {
            unsafe {
                self.remove_chunk(free_lower);
                chunk.set_in_use(false);
                free_lower.coalesce(chunk);
                chunk = free_lower;
            }
        } else {
            unsafe { chunk.set_in_use(false) }
        }
        // println!("{chunk:?}");
        if let Some(free_higher) = unsafe { chunk.get_free_higher_chunk() } {
            if unsafe { !free_higher.is_top() } {
                unsafe {
                    self.remove_chunk(free_higher);
                    chunk.coalesce(free_higher);
                    self.add_chunk(chunk);
                }
            }
            // otherwise do not coalesce with the top chunk so it remains minimum
        } else {
            unsafe { self.add_chunk(chunk) }
        }
        // println!("{chunk:?}");
    }

    unsafe fn realloc(
        &mut self,
        user_data: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> Option<NonNull<u8>> {
        let mut chunk = unsafe { Chunk::from_user_data(user_data, self.limit) };
        // println!("{chunk:?} {layout:?} -> {new_size}");
        if let (Some(user_data), remain) =
            unsafe { chunk.split(Layout::from_size_align(new_size, layout.align()).unwrap()) }
        {
            debug_assert!(remain.is_none());
            return Some(user_data);
        }

        let free_higher;
        unsafe {
            chunk.set_in_use(false);
            free_higher = chunk.get_free_higher_chunk();
            chunk.set_in_use(true);
        }

        // also falling back for the top chunk since it does not have higher chunk
        let Some(free_higher) = free_higher else {
            return None;
        };
        if unsafe { free_higher.is_top() }
            || unsafe { chunk.get_size() + free_higher.get_size() } < new_size + Chunk::META_SIZE
        {
            return None;
        }

        unsafe {
            self.remove_chunk(free_higher);
            chunk.coalesce(free_higher);
        }
        // println!("{chunk:?}");

        if let (Some(user_data), remain) =
            unsafe { chunk.split(Layout::from_size_align(new_size, layout.align()).unwrap()) }
        {
            // println!("{chunk:?}");
            if let Some(remain) = remain {
                unsafe { self.add_chunk(remain) }
            }
            Some(user_data)
        } else {
            // feels like unnecessary to revert the coalescing
            // the chunk will be deallocated as a whole shortly, and the coalescing will happen
            // again if we revert it now
            // anyway this can only happen because of alignment issue and should be pretty rare
            None
        }
    }

    fn new(space: &mut impl Space) -> Self {
        let ptr_range = space.as_mut_ptr_range();
        Self {
            space: NonNull::new(ptr_range.start).unwrap(),
            #[cfg(any(dev, test))]
            limit: NonNull::new(ptr_range.end).unwrap(),
            #[cfg(not(any(dev, test)))]
            limit: (),
        }
    }

    unsafe fn alloc_in_space(space: &mut impl Space, layout: Layout) -> *mut u8 {
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut overlay = Self::new(space);
        unsafe { overlay.sanity_check() }

        let user_data = match unsafe { overlay.alloc(layout) } {
            Ok(user_data) => user_data.as_ptr(),
            Err(mut top) => {
                let size = space.len();
                if !space.grow(size + layout.size() + layout.align() + Chunk::META_SIZE) {
                    null_mut()
                } else {
                    overlay = Self::new(space);
                    top.limit = overlay.limit; // the only `Chunk` we are keeping
                    let new_size = dbg!(space.len());
                    assert_eq!(new_size % 8, 0);
                    unsafe {
                        let mut new_top = Chunk::new(
                            NonNull::new(space.as_mut_ptr_range().end.sub(Chunk::MIN_SIZE))
                                .unwrap(),
                            overlay.limit,
                        );
                        new_top.set_prev(None);
                        new_top.set_next(None);
                        new_top.set_in_use(false);
                        new_top.set_size(Chunk::MIN_SIZE);
                        overlay.update_top_chunk(top, new_top);
                        top.set_size(new_size - size);
                        top.set_in_use(false); // update lower_in_use on the new top chunk
                        if let Some(mut free_lower) = top.get_free_lower_chunk() {
                            // not coalescing because `top` looks like a top chunk
                            overlay.remove_chunk(free_lower);
                            free_lower.set_size(free_lower.get_size() + top.get_size());
                            overlay.add_chunk(free_lower);
                        }
                        overlay.alloc(layout)
                    }
                    .expect("second allocating try always success")
                    .as_ptr()
                }
            }
        };
        unsafe { overlay.sanity_check() }
        user_data
    }

    unsafe fn dealloc_in_space(space: &mut impl Space, user_data: *mut u8) {
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut overlay = Self::new(space);
        if space.as_mut_ptr_range().contains(&user_data) {
            unsafe { overlay.dealloc(user_data) }
        }
        unsafe { overlay.sanity_check() }

        // TODO otherwise proxy to System
        // TODO do space shrinking
    }

    unsafe fn realloc_in_space(
        space: &mut impl Space,
        user_data: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> *mut u8 {
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut overlay = Self::new(space);
        if let Some(user_data) = unsafe { overlay.realloc(user_data, layout, new_size) } {
            unsafe { overlay.sanity_check() }
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
        unsafe { Overlay::new(&mut space).init(space.len()) };
        Self(Mutex::new(space))
    }

    fn acquire_space(&self) -> MutexGuard<'_, S> {
        loop {
            match self.0.try_lock() {
                Ok(space) => break space,
                Err(WouldBlock) => {}
                _ => panic!(),
            }
        }
    }

    pub fn sanity_check(&self)
    where
        S: Space,
    {
        unsafe { Overlay::new(&mut *self.acquire_space()).sanity_check() }
    }
}

static ENABLED: AtomicBool = AtomicBool::new(true);

unsafe impl<S> GlobalAlloc for Allocator<S>
where
    S: Space,
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ENABLED.load(SeqCst) && !panicking() {
            unsafe { Overlay::alloc_in_space(&mut *self.acquire_space(), layout) }
        } else {
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ENABLED.load(SeqCst) && !panicking() {
            unsafe { Overlay::dealloc_in_space(&mut *self.acquire_space(), ptr) }
        } else {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ENABLED.load(SeqCst) && !panicking() {
            unsafe { Overlay::realloc_in_space(&mut *self.acquire_space(), ptr, layout, new_size) }
        } else {
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
}

// #[cfg(any(test, dev))]
#[allow(unused)]
impl Overlay {
    unsafe fn iter_all_chunk(&self) -> impl Iterator<Item = Chunk> {
        use std::iter::from_fn;

        debug_assert_eq!(unsafe { self.space.as_ref() }, &0x82);
        let mut chunk = Some(unsafe { self.start_chunk() });
        from_fn(move || {
            let item = chunk;
            chunk = match item {
                Some(item) if unsafe { item.get_in_use() || !item.is_top() } => {
                    Some(unsafe { item.get_higher_chunk() })
                }
                _ => None,
            };
            item
        })
    }

    unsafe fn iter_free_chunk(&self) -> impl Iterator<Item = Chunk> {
        use std::iter::from_fn;

        debug_assert_eq!(unsafe { self.space.as_ref() }, &0x82);
        let mut chunk = Some(unsafe { self.find_smallest(0) });
        from_fn(move || {
            let item = chunk;
            if let Some(item) = item {
                chunk = unsafe { item.get_next() };
            }
            item
        })
    }

    unsafe fn sanity_check(&self) {
        let mut chunks = [None; 10];
        for (i, chunk) in unsafe { self.iter_all_chunk() }.enumerate() {
            chunks[i % 10] = Some(chunk);
            debug_assert!(unsafe { chunk.get_size() } >= Chunk::MIN_SIZE, "{chunks:?}",);
        }
        // TODO more check if needed
    }
}

#[cfg(test)]
mod tests {
    use std::{iter::repeat, slice};

    use crate::space::{Fixed, Mmap};

    use super::*;

    #[test]
    fn single_free_chunk_on_init() {
        let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
        let all_chunk =
            Vec::from_iter(unsafe { Overlay::new(&mut *alloc.acquire_space()).iter_all_chunk() });
        let free_chunk =
            Vec::from_iter(unsafe { Overlay::new(&mut *alloc.acquire_space()).iter_free_chunk() });
        assert_eq!(all_chunk.len(), 2);
        assert_eq!(free_chunk, all_chunk);
    }

    #[test]
    fn working_debug_chunk() {
        let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
        assert!(format!(
            "{:?}",
            unsafe { Overlay::new(&mut *alloc.acquire_space()).iter_all_chunk() }
                .next()
                .unwrap()
        )
        .starts_with("Chunk"));
    }

    #[test]
    fn valid_addr() {
        fn run(sizes: impl Iterator<Item = usize>) {
            let mut data = vec![0; 4 << 10].into_boxed_slice();
            let ptr_range = data.as_mut_ptr_range();
            let alloc = Allocator::new(Fixed::from(data));
            for size in sizes {
                let ptr = unsafe { alloc.alloc(Layout::from_size_align(size, 1).unwrap()) };
                if ptr.is_null() {
                    break;
                }
                assert!(ptr_range.contains(&ptr));
            }
        }

        run([1].into_iter());
        run(1..10);
        run(repeat(1));
        run(1..);
    }

    #[test]
    fn aligned_addr() {
        fn run(sizes: impl Iterator<Item = usize>, align: usize) {
            let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
            for size in sizes {
                let ptr = unsafe { alloc.alloc(Layout::from_size_align(size, align).unwrap()) };
                if ptr.is_null() {
                    break;
                }
                assert_eq!(ptr.align_offset(align), 0);
            }
        }

        run([1].into_iter(), 16);
        run(1..10, 16);
        run(1..10, 32);
        run(repeat(1), 16);
        run(repeat(1), 32);
        run(1.., 16);
        run(1.., 32);
        run(1.., 64);
    }

    #[test]
    fn alloc_dealloc_identical() {
        fn run(layouts: impl Iterator<Item = Layout> + Clone) {
            let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
            let chunks = Vec::from_iter(unsafe {
                Overlay::new(&mut *alloc.acquire_space()).iter_all_chunk()
            });
            let ptrs = Vec::from_iter(
                layouts
                    .clone()
                    .map(|layout| unsafe { (alloc.alloc(layout), layout) })
                    .take_while(|(ptr, _)| !ptr.is_null()),
            );
            for (ptr, layout) in ptrs.into_iter() {
                // println!("{ptr:?} {layout:?}");
                unsafe { alloc.dealloc(ptr, layout) }
            }
            assert_eq!(
                Vec::from_iter(unsafe {
                    Overlay::new(&mut *alloc.acquire_space()).iter_all_chunk()
                }),
                chunks
            );

            // again but dealloc in LIFO order
            // yet to find a way to eliminate this duplication
            let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
            let chunks = Vec::from_iter(unsafe {
                Overlay::new(&mut *alloc.acquire_space()).iter_all_chunk()
            });
            let ptrs = Vec::from_iter(
                layouts
                    .map(|layout| unsafe { (alloc.alloc(layout), layout) })
                    .take_while(|(ptr, _)| !ptr.is_null()),
            );
            for (ptr, layout) in ptrs.into_iter().rev() {
                unsafe { alloc.dealloc(ptr, layout) }
            }
            assert_eq!(
                Vec::from_iter(unsafe {
                    Overlay::new(&mut *alloc.acquire_space()).iter_all_chunk()
                }),
                chunks
            );
        }

        run([Layout::from_size_align(1, 1).unwrap()].into_iter());
        run((1..10).map(|size| Layout::from_size_align(size, 1).unwrap()));
        run((0..=6).map(|align| Layout::from_size_align(1, 1 << align).unwrap()));
        run((1..).map(|size| Layout::from_size_align(size, 1).unwrap()));
    }

    #[test]
    fn realloc_in_place() {
        let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
        let mut layout = Layout::from_size_align(8, 1).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        let new_ptr = unsafe { alloc.realloc(ptr, layout, 16) };
        assert_eq!(new_ptr, ptr);

        layout = Layout::from_size_align(16, 1).unwrap();
        loop {
            let new_ptr = unsafe { alloc.realloc(ptr, layout, layout.size() * 2) };
            if new_ptr.is_null() {
                break;
            }
            assert_eq!(new_ptr, ptr);
            layout = Layout::from_size_align(layout.size() * 2, 1).unwrap();
        }
    }

    #[test]
    fn realloc_copied() {
        let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
        let layout = Layout::from_size_align(8, 1).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        unsafe { slice::from_raw_parts_mut(ptr, 8) }
            .copy_from_slice(&u64::to_ne_bytes(0x1122334455667788));
        unsafe { alloc.alloc(layout) };
        let new_ptr = unsafe { alloc.realloc(ptr, layout, 32) };
        assert_eq!(
            unsafe { slice::from_raw_parts_mut(new_ptr, 8) },
            &u64::to_ne_bytes(0x1122334455667788)
        );
    }

    #[test]
    fn grow() {
        let mut space = Mmap::new();
        space.set_size(1 << 10);
        let alloc = Allocator::new(space);
        unsafe { alloc.alloc(Layout::from_size_align(1 << 10, 1).unwrap()) };
    }
}
