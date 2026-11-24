#![windows_subsystem = "windows"]

mod app;
mod config;
mod hdr;
mod log;
mod watch;

fn main() {
    // A windowed exe has no console, so a panic would vanish without this.
    std::panic::set_hook(Box::new(|info| {
        log::write(&format!("[!] HDRify crashed: {info}"));
        watch::stop(); // the trace session would outlive the process
    }));
    app::run();
}
