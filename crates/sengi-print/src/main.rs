//! sengi-print, the helper behind Settings, Printers.
//!
//!     sengi-print add NAME URI    add a driverless printer (needs root)
//!     sengi-print remove NAME     remove a printer (needs root)
//!     sengi-print discover        list printers on the network
//!
//! Nothing else is accepted. See the README.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(sengi_print::helper::main_with(&args));
}
