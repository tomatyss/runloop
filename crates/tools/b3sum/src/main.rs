use std::env;
use std::fs;
use std::io;

fn main() -> io::Result<()> {
    let mut paths = env::args().skip(1).collect::<Vec<_>>();
    if paths.is_empty() {
        eprintln!("usage: b3sum <file> [...]");
        std::process::exit(1);
    }
    paths.sort();
    for path in paths {
        match fs::read(&path) {
            Ok(bytes) => {
                let hash = blake3::hash(&bytes);
                println!("{path}\t{hash}");
            }
            Err(err) => {
                return Err(err);
            }
        }
    }
    Ok(())
}
