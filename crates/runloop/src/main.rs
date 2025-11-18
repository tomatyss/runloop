use std::env;
use std::process::{Command, exit};

fn main() {
    let mut cmd = Command::new("rlp");
    cmd.args(env::args().skip(1));

    match cmd.status() {
        Ok(status) => {
            if let Some(code) = status.code() {
                exit(code);
            }
        }
        Err(err) => {
            eprintln!("runloop wrapper failed to exec `rlp`: {err}");
        }
    }
    exit(1);
}
