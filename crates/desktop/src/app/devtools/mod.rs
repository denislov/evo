mod native_replay;

pub(super) fn open_requested(cx: &mut gpui::App) -> bool {
    let request = match native_replay::request() {
        Ok(request) => request,
        Err(error) => {
            eprintln!("desktop: native replay configuration failed: {error}");
            cx.quit();
            return true;
        }
    };
    let Some(request) = request else {
        return false;
    };
    if let Err(error) = native_replay::open(cx, request) {
        eprintln!("desktop: native replay failed: {error}");
        cx.quit();
    }
    true
}
