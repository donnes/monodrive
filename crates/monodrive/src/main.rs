fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") => {
            if let Err(e) = monodrive_core::mcp::serve_stdio() {
                eprintln!("monodrive mcp: {e}");
                std::process::exit(1);
            }
        }
        // The operation owner, started by the window or `mcp` when needed.
        Some("ops") => std::process::exit(monodrive_core::ops::run_owner()),
        Some("--version" | "-V") => println!("Monodrive {}", env!("CARGO_PKG_VERSION")),
        Some(other) if other.starts_with('-') => {
            eprintln!("usage: monodrive [folder] | monodrive mcp | monodrive --version");
            std::process::exit(2);
        }
        // Relative to where it was started, and without `..`: the window
        // and the owner only take absolute paths, and the path bar shows it.
        folder => monodrive::run(folder.map(|folder| {
            let folder = monodrive_core::expand_home(folder);
            std::fs::canonicalize(&folder)
                .or_else(|_| std::path::absolute(&folder))
                .unwrap_or(folder)
        })),
    }
}
