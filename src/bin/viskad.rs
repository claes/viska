fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
        println!("Usage: viskad --config PATH\nLinux one-shot text delivery daemon. Configuration is JSON; secrets arrive only over authenticated IPC.");
        return;
    }
    let result = (|| {
        viska::protect_process()?;
        if args.len() != 3 || args[1] != "--config" {
            return Err("usage: viskad --config PATH");
        }
        viska::daemon::run(viska::config::Config::load(&args[2])?)
    })();
    if let Err(e) = result {
        eprintln!("viskad: {e}");
        std::process::exit(1);
    }
}
