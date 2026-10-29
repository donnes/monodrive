fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") => {
            if let Err(e) = monodrive_core::mcp::serve_stdio() {
                eprintln!("monodrive mcp: {e}");
                std::process::exit(1);
            }
        }
        Some("--version" | "-V") => println!("Monodrive {}", env!("CARGO_PKG_VERSION")),
        Some(other) if other.starts_with('-') => {
            eprintln!("usage: monodrive [folder] | monodrive mcp | monodrive --version");
            std::process::exit(2);
        }
        folder => monodrive::run(folder.map(monodrive_core::expand_home)),
    }
}
