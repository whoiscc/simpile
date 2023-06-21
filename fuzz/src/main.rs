use afl::fuzz;
use simpile::fuzz::Method;
use simpile::{linked::Allocator, space::Fixed};

#[repr(align(4096))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Page(u8);

union Data {
    page: Page,
    buf: [u8; 4096],
}

fn main() {
    fuzz!(|bytes: &[u8]| {
        // keep alignment same to make sure the failure is reproducible
        let mut data = Data {
            page: Default::default(),
        };
        Method::run_fuzz(
            Method::from_bytes(bytes).into_iter(),
            Allocator::new(Fixed::from(unsafe { &mut data.buf[..] })),
        );
    });
}
