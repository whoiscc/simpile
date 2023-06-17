use std::fs::{create_dir, write};

use simpile_fuzz::AllocatorMethod;

fn main() -> std::io::Result<()> {
    create_dir("in")?;
    write("in/0", AllocatorMethod::to_bytes(&[]))?;
    write(
        "in/1",
        AllocatorMethod::to_bytes(&[
            AllocatorMethod::Alloc { size: 1 },
            AllocatorMethod::Dealloc { index: 0 },
        ]),
    )?;
    write(
        "in/2",
        AllocatorMethod::to_bytes(&[
            AllocatorMethod::Alloc { size: 1 },
            AllocatorMethod::Realloc {
                index: 0,
                new_size: 2,
            },
            AllocatorMethod::Dealloc { index: 0 },
        ]),
    )?;
    Ok(())
}
