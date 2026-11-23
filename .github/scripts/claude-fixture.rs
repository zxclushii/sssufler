use std::{env, fs, io};

fn main() -> io::Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    fs::write("launch.tmp", args.join("\0"))?;
    fs::rename("launch.tmp", "launch.txt")
}
