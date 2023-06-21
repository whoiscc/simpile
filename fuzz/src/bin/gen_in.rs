use std::fs::{create_dir, write};

use simpile::fuzz::Method;

fn main() -> std::io::Result<()> {
    create_dir("in")?;
    write("in/0", Method::to_bytes(&[]))?;
    write(
        "in/1",
        Method::to_bytes(&[
            Method::Alloc { size: 1, align: 1 },
            Method::Dealloc { index: 0 },
        ]),
    )?;
    write(
        "in/2",
        Method::to_bytes(&[
            Method::Alloc { size: 1, align: 1 },
            Method::Realloc {
                index: 0,
                new_size: 2,
            },
            Method::Dealloc { index: 0 },
        ]),
    )?;
    write(
        "in/3",
        Method::to_bytes(&[
            Method::Alloc { size: 1, align: 64 },
            Method::Alloc { size: 1, align: 1 },
            Method::Realloc {
                index: 0,
                new_size: 2,
            },
            Method::Dealloc { index: 0 },
        ]),
    )?;
    Ok(())
}
