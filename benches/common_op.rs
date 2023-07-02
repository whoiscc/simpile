use std::alloc::{GlobalAlloc, Layout, System};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dlmalloc::GlobalDlmalloc;
use linked_list_allocator::LockedHeap;
use simpile::{linked::Allocator, space::Mmap, Space};

fn run(c: &mut Criterion) {
    fn new_alloc() -> Allocator<Mmap> {
        let mut space = Mmap::new();
        space.set_size(128 << 10);
        Allocator::new(space)
    }

    fn one_alloc(alloc: &impl GlobalAlloc) {
        let layout = Layout::from_size_align(1, 1).unwrap();
        let ptr = black_box(unsafe { alloc.alloc(layout) });
        unsafe { alloc.dealloc(ptr, layout) }
    }
    let mut group = c.benchmark_group("One Alloc");
    group.bench_function("system", |b| b.iter(|| one_alloc(&System)));
    group.bench_function("minimal", |b| {
        let mut space = Mmap::new();
        space.set_size(128 << 10);
        let alloc = unsafe { LockedHeap::new(space.as_mut_ptr(), space.len()) };
        b.iter(|| one_alloc(&alloc));
        drop(space)
    });
    group.bench_function("dl", |b| b.iter(|| one_alloc(&GlobalDlmalloc)));
    group.bench_function("linked", |b| {
        let alloc = new_alloc();
        b.iter(|| one_alloc(&alloc))
    });
    group.finish();

    let mut group = c.benchmark_group("One Alloc Fast");
    group.bench_function("system", |b| {
        let layout = Layout::from_size_align(1, 1).unwrap();
        let ptr = unsafe { System.alloc(layout) };
        let occupied_higher = unsafe { System.alloc(layout) };
        unsafe { System.dealloc(ptr, layout) }
        b.iter(|| one_alloc(&System));
        unsafe { System.dealloc(occupied_higher, layout) }
    });
    group.bench_function("minimal", |b| {
        let mut space = Mmap::new();
        space.set_size(128 << 10);
        let alloc = unsafe { LockedHeap::new(space.as_mut_ptr(), space.len()) };
        let layout = Layout::from_size_align(1, 1).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        let occupied_higher = unsafe { alloc.alloc(layout) };
        unsafe { alloc.dealloc(ptr, layout) }
        b.iter(|| one_alloc(&alloc));
        unsafe { alloc.dealloc(occupied_higher, layout) }
        drop(space)
    });
    group.bench_function("dlmalloc", |b| {
        let layout = Layout::from_size_align(1, 1).unwrap();
        let ptr = unsafe { GlobalDlmalloc.alloc(layout) };
        let occupied_higher = unsafe { GlobalDlmalloc.alloc(layout) };
        unsafe { GlobalDlmalloc.dealloc(ptr, layout) }
        b.iter(|| one_alloc(&System));
        unsafe { GlobalDlmalloc.dealloc(occupied_higher, layout) }
    });
    group.bench_function("linked", |b| {
        let alloc = new_alloc();
        let layout = Layout::from_size_align(1, 1).unwrap();
        let ptr = unsafe { alloc.alloc(layout) };
        let occupied_higher = unsafe { alloc.alloc(layout) };
        unsafe { alloc.dealloc(ptr, layout) }
        b.iter(|| one_alloc(&alloc));
        unsafe { alloc.dealloc(occupied_higher, layout) }
    });
    group.finish();

    fn hundred_alloc(alloc: &impl GlobalAlloc, lifo: bool) {
        let ptrs = Vec::from_iter((1..100).map(|size| {
            black_box(unsafe { alloc.alloc(Layout::from_size_align(size, 1).unwrap()) })
        }));
        for (index, ptr) in if !lifo {
            Box::new(ptrs.into_iter().enumerate()) as Box<dyn Iterator<Item = (usize, *mut u8)>>
        } else {
            Box::new(ptrs.into_iter().enumerate().rev())
        } {
            unsafe { alloc.dealloc(ptr, Layout::from_size_align(index + 1, 1).unwrap()) }
        }
    }
    let mut group = c.benchmark_group("1..100 Alloc");
    group.bench_function("system", |b| b.iter(|| hundred_alloc(&System, false)));
    group.bench_function("minimal", |b| {
        let mut space = Mmap::new();
        space.set_size(128 << 10);
        let alloc = unsafe { LockedHeap::new(space.as_mut_ptr(), space.len()) };
        b.iter(|| hundred_alloc(&alloc, false));
        drop(space)
    });
    group.bench_function("dl", |b| b.iter(|| hundred_alloc(&GlobalDlmalloc, false)));
    group.bench_function("linked", |b| {
        let alloc = new_alloc();
        b.iter(|| hundred_alloc(&alloc, false))
    });
    group.finish();

    let mut group = c.benchmark_group("1..100 Alloc LIFO");
    group.bench_function("system", |b| b.iter(|| hundred_alloc(&System, true)));
    group.bench_function("minimal", |b| {
        let mut space = Mmap::new();
        space.set_size(128 << 10);
        let alloc = unsafe { LockedHeap::new(space.as_mut_ptr(), space.len()) };
        b.iter(|| hundred_alloc(&alloc, true));
        drop(alloc)
    });
    group.bench_function("dl", |b| b.iter(|| hundred_alloc(&GlobalDlmalloc, true)));
    group.bench_function("linked", |b| {
        let alloc = new_alloc();
        b.iter(|| hundred_alloc(&alloc, true))
    });
    group.finish();

    fn hundred_realloc(alloc: &impl GlobalAlloc, interleave: bool) {
        let mut interleaved = [std::ptr::null_mut(); 100];
        let mut size = 1;
        let mut layout = Layout::from_size_align(size, 1).unwrap();
        let mut ptr = black_box(unsafe { alloc.alloc(layout) });
        for i in 0..100 {
            if interleave {
                interleaved[i] = unsafe { alloc.alloc(Layout::from_size_align(1, 1).unwrap()) };
            }
            size += 8;
            ptr = black_box(unsafe { alloc.realloc(ptr, layout, size) });
            layout = Layout::from_size_align(size, 1).unwrap();
        }
        unsafe { alloc.dealloc(ptr, layout) }
        if interleave {
            for ptr in interleaved {
                unsafe { alloc.dealloc(ptr, Layout::from_size_align(1, 1).unwrap()) }
            }
        }
    }
    let mut group = c.benchmark_group("100 (+8) Realloc");
    group.bench_function("system", |b| b.iter(|| hundred_realloc(&System, false)));
    group.bench_function("minimal", |b| {
        let mut space = Mmap::new();
        space.set_size(128 << 10);
        let alloc = unsafe { LockedHeap::new(space.as_mut_ptr(), space.len()) };
        b.iter(|| hundred_realloc(&alloc, false));
        drop(space)
    });
    group.bench_function("dl", |b| b.iter(|| hundred_realloc(&GlobalDlmalloc, false)));
    group.bench_function("linked", |b| {
        let alloc = new_alloc();
        b.iter(|| hundred_realloc(&alloc, false))
    });
    group.finish();

    let mut group = c.benchmark_group("100 (+8) Realloc Copied");
    group.bench_function("system", |b| b.iter(|| hundred_realloc(&System, true)));
    group.bench_function("minimal", |b| {
        let mut space = Mmap::new();
        space.set_size(128 << 10);
        let alloc = unsafe { LockedHeap::new(space.as_mut_ptr(), space.len()) };
        b.iter(|| hundred_realloc(&alloc, true));
        drop(space)
    });
    group.bench_function("dl", |b| b.iter(|| hundred_realloc(&GlobalDlmalloc, true)));
    group.bench_function("linked", |b| {
        let alloc = new_alloc();
        b.iter(|| hundred_realloc(&alloc, true))
    });
    group.finish();
}

criterion_group!(benches, run);
criterion_main!(benches);
