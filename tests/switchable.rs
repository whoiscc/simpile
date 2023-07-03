use std::{
    alloc::{GlobalAlloc, Layout},
    sync::OnceLock,
};

use simpile::{linked::Allocator, space::Mmap, Space, Switchable};

#[cfg(not(feature = "switchable"))]
compile_error!("feature \"switchable\" is required to compile");

struct Global(OnceLock<Switchable<Allocator<Mmap>>>);

impl Global {
    fn init() -> Switchable<Allocator<Mmap>> {
        let mut space = Mmap::new();
        space.set_size(128 << 10); // 128 KB
        Switchable::new(Allocator::new(space))
    }
}

unsafe impl GlobalAlloc for Global {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0.get_or_init(Self::init).alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.0.get_or_init(Self::init).dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        self.0
            .get_or_init(Self::init)
            .realloc(ptr, layout, new_size)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.0.get_or_init(Self::init).alloc_zeroed(layout)
    }
}

#[global_allocator]
static GLOBAL: Global = Global(OnceLock::new());

#[test]
fn run() {
    let on_box = Box::new(42);
    println!("On: {:?}", &on_box as *const _);
    GLOBAL.0.get_or_init(|| unreachable!()).set_enable(false);
    println!("Off: {:?}", &Box::new(42) as *const _);
    drop(on_box)
}
