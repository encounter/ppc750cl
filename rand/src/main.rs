use rand_core::{RngCore, SeedableRng};
use sfmt::SFMT;

use ppc750cl::{Ins, Opcode};
use std::io::{BufWriter, Write};

fn main() {
    let mut rng = SFMT::seed_from_u64(42);
    let stdout = std::io::stdout();
    let stdout_lock = stdout.lock();
    let mut stream = BufWriter::with_capacity(1_000_000, stdout_lock);
    loop {
        let ins = Ins::disasm(rng.next_u32());
        if ins.op == Opcode::Illegal {
            continue;
        }
        if ins.write_string(&mut stream).is_err() {
            return;
        }
        if stream.write_all("\n".as_ref()).is_err() {
            return;
        }
    }
}