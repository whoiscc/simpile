use std::alloc::System;

use afl::fuzz;
use simpile::fuzz::Method;

fn main() {
    fuzz!(|bytes: &[u8]| { Method::run_fuzz(Method::from_bytes(bytes).into_iter(), System) });
}
