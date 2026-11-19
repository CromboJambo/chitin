//! Chitin compositor binary.
//!
//! Wayland compositor with DRM/KMS output and virtual display support.
//! Supports both physical display (DRM) and headless/containerized modes.

use std::process;

/// Command-line options for the chitin compositor.
#[derive(Debug)]
struct Options {
    /// Virtual display resolution (e.g., "1920x1080").
    /// When set, creates a virtual output even without a physical display.
    virtual_resolution: Option<(u32, u32)>,
}

impl Options {
    fn parse() -> Self {
        let mut opts = Options { virtual_resolution: None };
        let args: Vec<String> = std::env::args().collect();

        let mut i = 1;
        while i < args.len() {
            match args[i].as_str() {
                "--virtual-resolution" => {
                    if i + 1 < args.len() {
                        if let Some(res) = parse_resolution(&args[i + 1]) {
                            opts.virtual_resolution = Some(res);
                        } else {
                            eprintln!(
                                "error: invalid resolution '{}', expected WxH (e.g., 1920x1080)",
                                args[i + 1]
                            );
                            process::exit(1);
                        }
                    } else {
                        eprintln!("error: --virtual-resolution requires a value");
                        process::exit(1);
                    }
                }
                "--help" | "-h" => {
                    print_usage();
                    process::exit(0);
                }
                "--version" | "-V" => {
                    println!("chitin-compositor {}", env!("CARGO_PKG_VERSION"));
                    process::exit(0);
                }
                other => {
                    eprintln!("error: unknown argument '{}'", other);
                    print_usage();
                    process::exit(1);
                }
            }
            i += 1;
        }

        opts
    }
}

fn parse_resolution(s: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = s.split('x').collect();
    if parts.len() == 2 {
        let w = parts[0].parse::<u32>().ok()?;
        let h = parts[1].parse::<u32>().ok()?;
        if w > 0 && h > 0 {
            return Some((w, h));
        }
    }
    None
}

fn print_usage() {
    println!(
        "chitin-compositor — Wayland compositor with DRM/KMS and virtual display support\n\
         \n\
         USAGE:\n\
         \tchitin-compositor [OPTIONS]\n\
         \n\
         OPTIONS:\n\
         \t--virtual-resolution WxH  Create a virtual output at the given resolution\n\
         \t                          (used for containerized/headless deployment)\n\
         \t-h, --help                Show this help message\n\
         \t-V, --version             Show version information"
    );
}

fn main() {
    let opts = Options::parse();

    // Initialize tracing subscriber.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chitin_core=debug".into()),
        )
        .try_init();

    if let Some((w, h)) = opts.virtual_resolution {
        tracing::info!(
            "chitin compositor starting in virtual display mode: {}x{}",
            w, h
        );
    } else {
        tracing::info!("chitin compositor starting (DRM/KMS mode)");
    }

    if let Err(e) = chitin_core::run() {
        tracing::error!("compositor exited with error: {}", e);
        process::exit(1);
    }
}
