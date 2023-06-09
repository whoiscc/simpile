pub mod linked;

use std::{
    mem::align_of,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::null_mut,
    slice,
};

use nix::{
    libc::{MAP_ANONYMOUS, MAP_SHARED, PROT_READ, PROT_WRITE},
    sys::mman::{mmap, mremap, munmap, MRemapFlags, MapFlags, ProtFlags},
};

pub trait Space
where
    Self: DerefMut<Target = [u8]>,
{
    fn set_size(&mut self, bytes: usize) -> bool;
}

pub struct Mmap {
    addr: *mut u8,
    len: usize,
}

impl Mmap {
    pub const fn new() -> Self {
        Self {
            addr: null_mut(),
            len: 0,
        }
    }
}

impl Default for Mmap {
    fn default() -> Self {
        Self::new()
    }
}

impl Deref for Mmap {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(self.addr, self.len) }
    }
}

impl DerefMut for Mmap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { slice::from_raw_parts_mut(self.addr, self.len) }
    }
}

impl Space for Mmap {
    fn set_size(&mut self, bytes: usize) -> bool {
        if bytes == self.len {
            return true;
        }

        let Ok(bytes) = NonZeroUsize::try_from(bytes) else {
            self.clear();
            return true;
        };

        let result = if self.addr.is_null() {
            unsafe {
                mmap(
                    None,
                    bytes,
                    ProtFlags::from_bits(PROT_READ | PROT_WRITE).unwrap(),
                    MapFlags::from_bits(MAP_SHARED | MAP_ANONYMOUS).unwrap(),
                    -1,
                    0,
                )
            }
        } else {
            unsafe {
                mremap(
                    self.addr as _,
                    self.len,
                    bytes.get(),
                    MRemapFlags::empty(),
                    None,
                )
            }
        };
        if let Ok(addr) = result {
            self.addr = addr as _;
            self.len = bytes.get();
        }
        result.is_ok()
    }
}

impl Mmap {
    pub fn clear(&mut self) {
        unsafe { munmap(self.addr as _, self.len) }.unwrap();
        self.addr = null_mut();
        self.len = 0;
    }
}

impl Drop for Mmap {
    fn drop(&mut self) {
        if self.len != 0 {
            self.clear()
        }
    }
}

#[repr(align(4096))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
struct Unit(u8);
pub struct Fixed(Box<[Unit]>);

impl Fixed {
    pub fn new(len: usize) -> Self {
        let data = vec![Unit(0); len / align_of::<Unit>()].into_boxed_slice();
        Self(data)
    }
}

impl Deref for Fixed {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { slice::from_raw_parts(self.0.as_ptr() as _, self.0.len() * align_of::<Unit>()) }
    }
}

impl DerefMut for Fixed {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe {
            slice::from_raw_parts_mut(self.0.as_mut_ptr() as _, self.0.len() * align_of::<Unit>())
        }
    }
}

impl Space for Fixed {
    fn set_size(&mut self, bytes: usize) -> bool {
        bytes == self.0.len() * align_of::<Unit>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_data() {
        fn run<S: Space>(space: &mut S) {
            let done = space.set_size(1 << 12); // 4KB
            assert!(done);
            let source = b"important data";
            space[..source.len()].copy_from_slice(source);
            space.set_size(1 << 13); // 8KB, expect fail in Fixed so no check
            assert_eq!(&space[..source.len()], source);
        }
        run(&mut Mmap::new());
        run(&mut Fixed::new(1 << 12));
    }

    #[test]
    fn aligned_data() {
        fn run<S: Space>(space: &mut S) {
            space.set_size(1 << 12);
            assert_eq!((space.as_ptr() as usize) % (1 << 12), 0);
        }
        run(&mut Mmap::new());
        run(&mut Fixed::new(1 << 12));
    }
}
