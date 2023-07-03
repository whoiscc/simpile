use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, Ordering::SeqCst},
    thread::panicking,
};

use crate::{linked::Allocator, Space};

pub trait EnablePtr {
    fn enable_ptr(&self, ptr: *mut u8) -> bool;
}

impl<S> EnablePtr for Allocator<S>
where
    S: Space,
{
    fn enable_ptr(&self, ptr: *mut u8) -> bool {
        let mut space = self.acquire_space();
        space.as_mut_ptr_range().contains(&ptr)
    }
}

pub struct Switchable<A> {
    alloc: A,
    enable: AtomicBool,
}

impl<A> From<A> for Switchable<A> {
    fn from(value: A) -> Self {
        Self::new(value)
    }
}

impl<A> Switchable<A> {
    pub const fn new(alloc: A) -> Self {
        Self {
            alloc,
            enable: AtomicBool::new(true),
        }
    }

    pub fn set_enable(&self, enable: bool) {
        self.enable.store(enable, SeqCst)
    }

    fn enable_alloc(&self) -> bool {
        self.enable.load(SeqCst) && !panicking()
    }
}

unsafe impl<A> GlobalAlloc for Switchable<A>
where
    A: GlobalAlloc + EnablePtr,
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.enable_alloc() {
            unsafe { self.alloc.alloc(layout) }
        } else {
            unsafe { System.alloc(layout) }
        }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if self.enable_alloc() {
            unsafe { self.alloc.alloc_zeroed(layout) }
        } else {
            unsafe { System.alloc_zeroed(layout) }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if self.alloc.enable_ptr(ptr) {
            unsafe { self.alloc.dealloc(ptr, layout) }
        } else {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if self.alloc.enable_ptr(ptr) {
            unsafe { self.alloc.realloc(ptr, layout, new_size) }
        } else {
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }
}
