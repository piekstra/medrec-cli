//! Synthetic test data for dogfooding medrec: `cargo run --example synth --
//! <DIR>`. See `medrec::testdata` for what it writes; all of it is invented.

fn main() {
    let out = std::env::args().nth(1).expect("usage: synth <DIR>");
    medrec::testdata::write_all(std::path::Path::new(&out));
    println!("synthetic data in {out}");
}
