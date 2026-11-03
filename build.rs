// On Windows, embed the app icon into headway.exe so Explorer, the taskbar and the
// installer show it. Other platforms take their icon from the app bundle or desktop entry.
fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/headway.ico");
        res.compile().expect("couldn't embed the Windows icon");
    }
}
