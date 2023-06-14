use std::alloc::{GlobalAlloc, Layout, System};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use simpile::{linked::Allocator, space::Mmap, Space};

fn run(c: &mut Criterion) {
    let layout = Layout::from_size_align(1, 1).unwrap();
    c.bench_function("system 1", |b| {
        b.iter(|| {
            let ptr = black_box(unsafe { System.alloc(layout) });
            unsafe { System.dealloc(ptr, layout) }
        })
    });

    c.bench_function("linked 1", |b| {
        let mut space = Mmap::new();
        space.set_size(1 << 10);
        let alloc = Allocator::new(space);
        b.iter(|| {
            let ptr = black_box(unsafe { alloc.alloc(layout) });
            unsafe { alloc.dealloc(ptr, layout) }
        })
    });

    c.bench_function("system 1..100", |b| {
        b.iter(|| {
            let ptrs = Vec::from_iter((1..100).map(|size| {
                black_box(unsafe { System.alloc(Layout::from_size_align(size, 1).unwrap()) })
            }));
            for ptr in ptrs {
                unsafe { System.dealloc(ptr, layout) }
            }
        })
    });

    c.bench_function("linked 1..100", |b| {
        let mut space = Mmap::new();
        space.set_size(16 << 10);
        let alloc = Allocator::new(space);
        b.iter(|| {
            let ptrs = Vec::from_iter((1..100).map(|size| {
                black_box(unsafe { alloc.alloc(Layout::from_size_align(size, 1).unwrap()) })
            }));
            for ptr in ptrs {
                unsafe { alloc.dealloc(ptr, layout) }
            }
        })
    });

    c.bench_function("system 1..100 reverse", |b| {
        b.iter(|| {
            let ptrs = Vec::from_iter((1..100).map(|size| {
                black_box(unsafe { System.alloc(Layout::from_size_align(size, 1).unwrap()) })
            }));
            for ptr in ptrs.into_iter().rev() {
                unsafe { System.dealloc(ptr, layout) }
            }
        })
    });

    c.bench_function("linked 1..100 reverse", |b| {
        let mut space = Mmap::new();
        space.set_size(16 << 10);
        let alloc = Allocator::new(space);
        b.iter(|| {
            let ptrs = Vec::from_iter((1..100).map(|size| {
                black_box(unsafe { alloc.alloc(Layout::from_size_align(size, 1).unwrap()) })
            }));
            for ptr in ptrs.into_iter().rev() {
                unsafe { alloc.dealloc(ptr, layout) }
            }
        })
    });
}

criterion_group!(benches, run);
criterion_main!(benches);
