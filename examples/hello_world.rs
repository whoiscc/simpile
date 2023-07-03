use std::{alloc::GlobalAlloc, sync::OnceLock};

use simpile::{linked::Allocator, space::Mmap, Space};

#[cfg(not(feature = "std"))]
compile_error!("feature \"std\" is required to compile");

struct Global(OnceLock<Allocator<Mmap>>);

impl Global {
    fn init() -> Allocator<Mmap> {
        let mut space = Mmap::new();
        space.set_size(128 << 10); // 128 KB
        Allocator::new(space)
    }
}

unsafe impl GlobalAlloc for Global {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        self.0.get_or_init(Self::init).alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        self.0.get_or_init(Self::init).dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        self.0
            .get_or_init(Self::init)
            .realloc(ptr, layout, new_size)
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        self.0.get_or_init(Self::init).alloc_zeroed(layout)
    }
}

#[global_allocator]
static GLOBAL: Global = Global(OnceLock::new());

fn main() {
    GLOBAL.0.get_or_init(|| unreachable!()).sanity_check();
    println!("Hello, world!");
}
