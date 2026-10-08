//! Entry point. clap errors/help/version are resolved locally, before GApplication.
//! clap must not run in the command_line callback: that runs in the primary process,
//! so remote invocations would never see its stdout.

use clap::error::ErrorKind;
use clap::Parser;
use gtk4::glib;
use kosuzu::args::Args;

fn main() -> glib::ExitCode {
    if let Err(e) = Args::try_parse_from(std::env::args()) {
        let _ = e.print();
        return match e.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => glib::ExitCode::SUCCESS,
            _ => glib::ExitCode::new(2),
        };
    }
    kosuzu::app::run()
}
