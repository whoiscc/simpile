use std::{
    io::{Read, Write},
    mem::size_of,
};

#[derive(Debug)]
pub enum AllocatorMethod {
    Alloc { size: usize },
    Dealloc { index: usize },
    Realloc { index: usize, new_size: usize },
}

impl AllocatorMethod {
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
                    methods.push(Self::Alloc {
                        size: usize::from_le_bytes(size),
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
                Self::Alloc { size } => {
                    bytes.write(&[0]).unwrap();
                    bytes.write(&size.to_le_bytes()).unwrap();
                }
                Self::Dealloc { index } => {
                    bytes.write(&[1]).unwrap();
                    bytes.write(&index.to_le_bytes()).unwrap();
                }
                Self::Realloc { index, new_size } => {
                    bytes.write(&[2]).unwrap();
                    bytes.write(&index.to_le_bytes()).unwrap();
                    bytes.write(&new_size.to_le_bytes()).unwrap();
                }
            }
        }
        bytes
    }
}
