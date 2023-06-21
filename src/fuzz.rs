use std::{
    alloc::{GlobalAlloc, Layout},
    io::{Read, Write},
    mem::size_of,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Alloc { size: usize, align: usize },
    Dealloc { index: usize },
    Realloc { index: usize, new_size: usize },
}

impl Method {
    pub fn from_bytes(mut bytes: &[u8]) -> Vec<Self> {
        let mut methods = Vec::new();
        const N: usize = size_of::<usize>();
        let mut kind = [0; 1];
        let mut read = || {
            bytes.read_exact(&mut kind)?;
            match kind[0] % 3 {
                0 => {
                    let mut size = [0; N];
                    bytes.read_exact(&mut size)?;
                    let mut log_align = [0; 1];
                    bytes.read_exact(&mut log_align)?;
                    methods.push(Self::Alloc {
                        size: usize::from_le_bytes(size),
                        // fuzz with align up to 2048 bytes, so a 4096 block can always allocate at least once
                        align: 1 << (log_align[0] % 11),
                    });
                }
                1 => {
                    let mut index = [0; N];
                    bytes.read_exact(&mut index)?;
                    methods.push(Self::Dealloc {
                        index: usize::from_le_bytes(index),
                    });
                }
                2 => {
                    let mut index = [0; N];
                    let mut new_size = [0; N];
                    bytes.read_exact(&mut index)?;
                    bytes.read_exact(&mut new_size)?;
                    methods.push(Self::Realloc {
                        index: usize::from_le_bytes(index),
                        new_size: usize::from_le_bytes(new_size),
                    });
                }
                _ => unreachable!(),
            }
            std::io::Result::Ok(())
        };
        while read().is_ok() {}
        methods
    }

    pub fn to_bytes(methods: &[Self]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for method in methods {
            match method {
                Self::Alloc { size, align } => {
                    bytes.write_all(&[0]).unwrap();
                    bytes.write_all(&size.to_le_bytes()).unwrap();
                    bytes.write_all(&[align.trailing_zeros() as u8]).unwrap();
                }
                Self::Dealloc { index } => {
                    bytes.write_all(&[1]).unwrap();
                    bytes.write_all(&index.to_le_bytes()).unwrap();
                }
                Self::Realloc { index, new_size } => {
                    bytes.write_all(&[2]).unwrap();
                    bytes.write_all(&index.to_le_bytes()).unwrap();
                    bytes.write_all(&new_size.to_le_bytes()).unwrap();
                }
            }
        }
        bytes
    }

    pub fn run_fuzz(methods: impl Iterator<Item = Self>, alloc: impl GlobalAlloc) {
        let mut objects = Vec::new();

        for method in methods {
            println!("{method:?},");
            match method {
                Self::Alloc { size, align } => {
                    let Ok(layout) = Layout::from_size_align(size, align) else {
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
                Self::Dealloc { index } => match objects.get_mut(index).and_then(Option::take) {
                    Some((ptr, layout)) if !ptr.is_null() => unsafe { alloc.dealloc(ptr, layout) },
                    _ => {}
                },
                Self::Realloc { index, new_size } => {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_serialization() {
        let methods = vec![
            Method::Alloc { size: 1, align: 1 },
            Method::Realloc {
                index: 0,
                new_size: 2,
            },
            Method::Dealloc { index: 0 },
        ];
        assert_eq!(Method::from_bytes(&Method::to_bytes(&methods)), methods);
    }
}
