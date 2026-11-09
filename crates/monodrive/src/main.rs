use monodrive_core::mcp::Profile;

fn main() {
    monodrive_core::seed_dev_support();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") => {
            let profile = match args.get(1..).unwrap_or_default() {
                [] => None,
                [flag, name] if flag == "--profile" => match Profile::parse(name) {
                    Some(profile) => Some(profile),
                    None => usage(),
                },
                _ => usage(),
            };
            if let Err(e) = monodrive_core::mcp::serve_stdio(profile) {
                eprintln!("monodrive mcp: {e}");
                std::process::exit(1);
            }
        }
        // The operation owner, started by the window or `mcp` when needed.
        Some("ops") => std::process::exit(monodrive_core::ops::run_owner()),
        Some("--version" | "-V") => println!("Monodrive {}", monodrive_core::VERSION),
        Some(other) if other.starts_with('-') => usage(),
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

fn usage() -> ! {
    eprintln!(
        "usage: monodrive [folder] | monodrive mcp [--profile core|all] | monodrive --version"
    );
    std::process::exit(2);
}
