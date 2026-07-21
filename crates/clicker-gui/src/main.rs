//! Auto Clicker — neumorphic Direct2D interface.

#[cfg(not(windows))]
fn main() {
    eprintln!("clicker-gui is Windows-only");
}

#[cfg(windows)]
fn main() {
    use clicker_gui::render::color::Theme;
    use clicker_gui::window;

    // Must precede any window creation.
    window::init_dpi_awareness();

    let hwnd = match window::create_main_window(Theme::Light) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("could not create window: {e}");
            std::process::exit(1);
        }
    };
    window::show(hwnd);
    std::process::exit(window::run_message_loop());
}
