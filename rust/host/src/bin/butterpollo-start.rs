#![windows_subsystem = "windows"]
fn main() {
    if let Err(error) = butterpollo_windows::launcher::run() {
        butterpollo_windows::launcher::show_error(&format!("{error:#}"));
    }
}
