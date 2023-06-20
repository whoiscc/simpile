use afl::fuzz;
use simpile::fuzz::Method;
use simpile::{linked::Allocator, space::Fixed};

fn main() {
    fuzz!(|bytes: &[u8]| {
        Method::run_fuzz(
            Method::from_bytes(bytes).into_iter(),
            Allocator::new(Fixed::from(vec![0; 4 << 10].into_boxed_slice())),
        )
    });
}
