//! Worker thread <-> GTK main loop bridge.
//!
//! gtk4-rs UI handles are `!Send`, so `done` must run on the main thread: `work`
//! runs on the gio thread pool and the `Result` returns via `spawn_local`.

use gtk4::glib;

pub fn run_in_worker<R: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<R> + Send + 'static,
    done: impl FnOnce(anyhow::Result<R>) + 'static,
) {
    glib::MainContext::default().spawn_local(async move {
        let result = match gtk4::gio::spawn_blocking(work).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(e),
            Err(panic) => Err(anyhow::anyhow!("worker thread panicked: {}", panic_message(&panic))),
        };
        done(result);
    });
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}
