use std::alloc::{GlobalAlloc, Layout};

use afl::fuzz;
use simpile::{linked::Allocator, space::Fixed};
use simpile_fuzz::AllocatorMethod;

fn run_fuzz(methods: Vec<AllocatorMethod>) {
    let alloc = Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice()));
    let mut objects = Vec::new();

    for method in methods {
        println!("{method:?}");
        match method {
            AllocatorMethod::Malloc { size } => {
                // TODO
                let Ok(layout) = Layout::from_size_align(size, 1) else {
                    continue;
                };
                if layout.size() == 0 {
                    continue;
                }
                let ptr = unsafe { alloc.alloc(layout) };
                if !ptr.is_null() {
                    objects.push(Some((ptr, layout)));
                }
            }
            AllocatorMethod::Dealloc { index } => {
                match objects.get_mut(index).and_then(Option::take) {
                    Some((ptr, layout)) if !ptr.is_null() => unsafe { alloc.dealloc(ptr, layout) },
                    _ => {}
                }
            }
            AllocatorMethod::Realloc { index, new_size } => {
                if let Some(object) = objects.get_mut(index) {
                    match object {
                        Some((ptr, layout)) if !ptr.is_null() => {
                            let Ok(new_layout) = Layout::from_size_align(new_size, layout.align()) else {
                                continue;
                            };
                            if new_layout.size() == 0 {
                                continue;
                            }
                            let new_ptr = unsafe { alloc.realloc(*ptr, *layout, new_size) };
                            if !new_ptr.is_null() {
                                *ptr = new_ptr;
                                *layout = new_layout;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Free any remaining allocations.
    for mut object in objects {
        if let Some((ptr, layout)) = object.take() {
            unsafe { alloc.dealloc(ptr, layout) }
        }
    }
}

fn main() {
    fuzz!(|bytes: &[u8]| { run_fuzz(AllocatorMethod::from_bytes(bytes)) });
}
