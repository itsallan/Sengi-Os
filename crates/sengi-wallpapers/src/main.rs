//! `sengi-wallpapers DIR` writes the shipped wallpapers and their
//! thumbnails into DIR. `scripts/gen-wallpapers.sh` runs it on the
//! repository's copy.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir] = &args[..] else {
        eprintln!("usage: sengi-wallpapers DIR");
        return ExitCode::FAILURE;
    };
    match sengi_wallpapers::write_all(Path::new(dir)) {
        Ok(count) => {
            println!("wrote {count} wallpapers to {dir}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("sengi-wallpapers: {error}");
            ExitCode::FAILURE
        }
    }
}
