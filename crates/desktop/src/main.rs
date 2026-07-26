fn main() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    desktop::run(desktop::DesktopApplicationOptions::new(cwd));
}
