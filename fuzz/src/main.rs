use afl::fuzz;
use simpile::fuzz::Method;
use simpile::{linked::Allocator, space::Fixed};

fn main() {
    fuzz!(|bytes: &[u8]| {
        Method::run_fuzz(
            Method::from_bytes(bytes).into_iter(),
            Allocator::new(Fixed::from(&mut *vec![0; 4 << 10])),
        )
    });
}
