use std::{
    alloc::{GlobalAlloc, Layout},
    ptr::{copy_nonoverlapping, null_mut, NonNull},
    sync::{Mutex, MutexGuard},
};

use crate::Space;

// invariants:
// chunk.ptr < chunk.limit (to be exact, chunk.ptr + CHUNK::MIN_SIZE <= chunk.limit)
// if chunk1 and chunk2 belong to the same heap, then chunk1.limit == chunk2.limit
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Chunk {
    data: NonNull<u8>,
    limit: NonNull<u8>,
}

impl Chunk {
    const META_MASK: u64 = 0x7;
    const IN_USE_BIT: u32 = 0;
    const LOWER_IN_USE_BIT: u32 = 1;

    // overhead of in-use chunk
    const META_SIZE: usize = 8;
    // 8 bytes prev, 8 bytes next, 8 bytes size
    const MIN_SIZE: usize = Self::META_SIZE + 24;

    fn new(data: NonNull<u8>, limit: NonNull<u8>) -> Self {
        debug_assert!(data < limit);
        Self { data, limit }
    }

    unsafe fn get_in_use(&self) -> bool {
        let meta = unsafe { *self.data.cast::<u64>().as_ref() };
        meta & (1 << Self::IN_USE_BIT) != 0
    }

    unsafe fn set_in_use(&mut self, in_use: bool) {
        // do this first because `is_top` can only be used on free chunk
        // asserting top chunk is never used, so in use chunk is not top
        if unsafe { self.get_in_use() || !self.is_top() } {
            unsafe { self.get_higher_chunk().set_lower_in_use(in_use) }
        }
        let meta = unsafe { self.data.cast::<u64>().as_mut() };
        *meta = (*meta & !(1 << Self::IN_USE_BIT)) | ((in_use as u64) << Self::IN_USE_BIT);
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
        NonNull::new(unsafe { *(self.data.as_ptr().offset(8).cast::<*mut u8>()) })
            .map(|data| Self::new(data, self.limit))
    }

    unsafe fn set_prev(&mut self, prev: Option<Self>) {
        let prev = prev
            .map(|chunk| {
                debug_assert_eq!(chunk.limit, self.limit);
                chunk.data.as_ptr()
            })
            .unwrap_or_else(null_mut);
        unsafe { *(self.data.as_ptr().offset(8).cast::<*mut u8>()) = prev }
    }

    unsafe fn get_next(&self) -> Option<Self> {
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

    unsafe fn from_user_data(user_data: *mut u8, limit: NonNull<u8>) -> Self {
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
        let mut new_size = usize::max(padding_size as usize + layout.size(), Self::MIN_SIZE);
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
            None
        } else {
            let mut remain = Self::new(
                NonNull::new(unsafe { self.data.as_ptr().add(new_size) }).unwrap(),
                self.limit,
            );
            unsafe {
                remain.set_in_use(false);
                remain.set_size(remain_size as _);
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

    // not assert `self` is not top chunk because this method will be called by the previous top
    // chunk, which still "looks like" a top chunk when calling
    // the assertion in `get_higher_chunk` make sure the "real" top chunk cannot call this
    unsafe fn get_free_higher_chunk(&self) -> Option<Self> {
        unsafe { Some(self.get_higher_chunk()).filter(|chunk| !chunk.get_in_use()) }
    }

    unsafe fn coalesce(&mut self, chunk: Self) {
        debug_assert_eq!(unsafe { self.get_free_higher_chunk() }, Some(chunk));
        unsafe { self.set_size(self.get_size() + chunk.get_size()) }
    }
}

// the overlay over some `Space`
struct Overlay {
    space: NonNull<u8>,
    limit: NonNull<u8>,
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
        assert!(len >= 8 * Self::BINS_LEN + Chunk::MIN_SIZE);
        assert_eq!(len % 8, 0);

        for index in Self::bin_index_of_size(Self::MIN_USER_SIZE)..Self::BINS_LEN {
            unsafe { self.set_bin_chunk(index, None) }
        }
        unsafe {
            let mut chunk = self.start_chunk();
            chunk.set_in_use(false);
            chunk.set_lower_in_use(true); // because there's no lower chunk
            let chunk_size = self
                .space
                .as_ptr()
                .add(len)
                .offset_from(chunk.data.as_ptr()) as usize;
            chunk.set_size(chunk_size);
            chunk.set_prev(None);
            chunk.set_next(None);
            self.set_bin_chunk(Self::bin_index_of_size(usize::MAX), Some(chunk));
        }
        // a little bit of best-effort sanity marker for initialized space
        unsafe { *self.space.as_mut() = 0x82 }
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
        // println!("{chunk:?}");
        chunk.expect("top chunk always reachable from bins")
    }

    unsafe fn alloc(&mut self, layout: Layout) -> Result<NonNull<u8>, Chunk> {
        if layout.size() == 0 {
            return Ok(NonNull::dangling()); // feels like better than null?
        }

        let mut chunk = unsafe { self.find_smallest(layout.size()) };
        let (mut user_data, mut remain) = unsafe { chunk.split(layout) };
        while user_data.is_none() {
            chunk = if let Some(next_chunk) = unsafe { chunk.get_next() } {
                next_chunk
            } else {
                return Err(chunk);
            };
            (user_data, remain) = unsafe { chunk.split(layout) };
        }
        // println!("{chunk:?} {user_data:?} {remain:?}");

        if unsafe { !chunk.is_top() } {
            unsafe {
                self.remove_chunk(chunk);
                if let Some(remain) = remain {
                    self.add_chunk(remain)
                }
            }
        } else if let Some(remain) = remain {
            unsafe { self.update_top_chunk(chunk, remain) }
        } else {
            // the case where the top chunk is almost the same size as requested allocation
            // i guess the algorithm cannot run if the top chunk is not free, so let's take the slow
            // path panelty here
            return Err(chunk);
        }

        // println!("{chunk:?}");
        let user_data = user_data.unwrap();
        // a little duplication to `split`
        let padding = unsafe { user_data.as_ptr().offset(-8) };
        let padding_size = unsafe { padding.offset_from(chunk.data.as_ptr()) } as usize;
        if padding_size != 0 {
            // println!("padding size {padding_size}");
            debug_assert_eq!(padding_size as u64 & Chunk::META_MASK, 0); // which also clear meta bits
            unsafe { *padding.cast::<u64>() = padding_size as _ }
        }
        unsafe { chunk.set_in_use(true) }

        Ok(user_data)
    }

    unsafe fn dealloc(&mut self, user_data: *mut u8) {
        let mut chunk = unsafe { Chunk::from_user_data(user_data, self.limit) };
        // coalesce with lower neighbor first to prevent update top chunk twice
        if let Some(mut free_lower) = unsafe { chunk.get_free_lower_chunk() } {
            unsafe {
                self.remove_chunk(free_lower);
                free_lower.coalesce(chunk);
                chunk = free_lower;
            }
        }
        if let Some(free_higher) = unsafe { chunk.get_free_higher_chunk() } {
            if unsafe { !free_higher.is_top() } {
                unsafe {
                    self.remove_chunk(free_higher);
                    chunk.coalesce(free_higher);
                    self.add_chunk(chunk);
                }
            } else {
                unsafe {
                    // have to coalesce first because we don't allow top chunk to coalesce
                    chunk.coalesce(free_higher);
                    self.update_top_chunk(free_higher, chunk);
                }
            }
        } else {
            unsafe { self.add_chunk(chunk) }
        }
        unsafe { chunk.set_in_use(false) }
    }

    unsafe fn realloc(
        &mut self,
        user_data: *mut u8,
        layout: Layout,
        new_size: usize,
    ) -> Option<NonNull<u8>> {
        let mut chunk = unsafe { Chunk::from_user_data(user_data, self.limit) };
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

    fn new(space: &mut impl Space) -> Self {
        let ptr_range = space.as_mut_ptr_range();
        Self {
            space: NonNull::new(ptr_range.start).unwrap(),
            limit: NonNull::new(ptr_range.end).unwrap(),
        }
    }

    unsafe fn alloc_in_space(space: &mut impl Space, layout: Layout) -> *mut u8 {
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut overlay = Self::new(space);
        match unsafe { overlay.alloc(layout) } {
            Ok(user_data) => user_data.as_ptr(),
            Err(mut top_chunk) => {
                let size = space.len();
                if !space.grow(size + layout.size() + layout.align() + Chunk::META_SIZE) {
                    null_mut()
                } else {
                    overlay = Self::new(space);
                    top_chunk.limit = overlay.limit; // the only `Chunk` we are keeping
                    let new_size = space.len();
                    assert_eq!(new_size % 8, 0);
                    unsafe {
                        top_chunk.set_size(top_chunk.get_size() + (new_size - size));
                        overlay.alloc(layout)
                    }
                    .expect("second allocating try always success")
                    .as_ptr()
                }
            }
        }
    }

    unsafe fn dealloc_in_space(space: &mut impl Space, user_data: *mut u8) {
        debug_assert_eq!(space.first(), Some(&0x82));
        unsafe { Self::new(space).dealloc(user_data) }
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
            if let Ok(space) = self.0.try_lock() {
                break space;
            }
        }
    }
}

unsafe impl<S> GlobalAlloc for Allocator<S>
where
    S: Space,
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { Overlay::alloc_in_space(&mut *self.acquire_space(), layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { Overlay::dealloc_in_space(&mut *self.acquire_space(), ptr) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { Overlay::realloc_in_space(&mut *self.acquire_space(), ptr, layout, new_size) }
    }
}

#[cfg(test)]
#[allow(unused)]
impl<S> Allocator<S> {
    unsafe fn iter_all_chunk(&self) -> impl Iterator<Item = Chunk>
    where
        S: Space,
    {
        use std::iter::from_fn;

        let mut space = self.0.try_lock().unwrap();
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut chunk = Some(unsafe { Overlay::new(&mut *space).start_chunk() });
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

    unsafe fn iter_free_chunk(&self) -> impl Iterator<Item = Chunk>
    where
        S: Space,
    {
        use std::iter::from_fn;

        let mut space = self.0.try_lock().unwrap();
        debug_assert_eq!(space.first(), Some(&0x82));
        let mut chunk = Some(unsafe { Overlay::new(&mut *space).find_smallest(0) });
        from_fn(move || {
            let item = chunk;
            if let Some(item) = item {
                chunk = unsafe { item.get_next() };
            }
            item
        })
    }
}

#[cfg(test)]
mod tests {
    use std::iter::repeat;

    use crate::space::Fixed;

    use super::*;

    #[test]
    fn valid_addr() {
        fn run(sizes: impl Iterator<Item = usize>) {
            let mut data = vec![0; 1 << 12].into_boxed_slice();
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
            let alloc = Allocator::new(Fixed::from(vec![0; 1 << 12].into_boxed_slice()));
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
            let alloc = Allocator::new(Fixed::from(vec![0; 1 << 11].into_boxed_slice()));
            let chunks = Vec::from_iter(unsafe { alloc.iter_all_chunk() });
            let ptrs = Vec::from_iter(
                layouts
                    .clone()
                    .map(|layout| unsafe { alloc.alloc(layout) })
                    .take_while(|ptr| !ptr.is_null()),
            );
            for (ptr, layout) in ptrs.into_iter().zip(layouts) {
                println!("{:?}", Vec::from_iter(unsafe { alloc.iter_all_chunk() }));
                println!("{ptr:?} {layout:?}");
                unsafe { alloc.dealloc(ptr, layout) }
            }
            println!("{:?}", Vec::from_iter(unsafe { alloc.iter_all_chunk() }));
            assert_eq!(Vec::from_iter(unsafe { alloc.iter_all_chunk() }), chunks);
        }

        // run([Layout::from_size_align(1, 1).unwrap()].into_iter());
        // run((1..10).map(|size| Layout::from_size_align(size, 1).unwrap()));
        // run((0..=6).map(|align| Layout::from_size_align(1, 1 << align).unwrap()));
        // run((1..).map(|size| Layout::from_size_align(size, 1).unwrap()));
    }
}
