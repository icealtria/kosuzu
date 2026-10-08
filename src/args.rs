//! CLI args, mirroring the Python `app.py` GOption definitions.
//!
//! gio-rs exposes no GOption entries, so clap parses in `main()` before GApplication;
//! remaining argv flows through the command_line signal unchanged (same path for
//! first launch and remote forwarding).

use clap::Parser;

#[derive(Parser, Default)]
#[command(name = "kosuzu", version, about = "Selection/screenshot/image OCR translation tool")]
pub struct Args {
    /// Translate the current selection (PRIMARY)
    #[arg(short = 's', long)]
    pub selection: bool,
    /// OCR an image file and translate
    #[arg(short = 'i', long, value_name = "FILE")]
    pub image: Option<String>,
    /// Translate the given text
    #[arg(long, value_name = "TEXT")]
    pub text: Option<String>,
    /// Screenshot -> OCR -> translate (bind a global hotkey)
    #[arg(long)]
    pub screenshot_ocr: bool,
    /// OCR the clipboard image and translate
    #[arg(long)]
    pub clipboard_ocr: bool,
    /// Quit the resident instance
    #[arg(long)]
    pub quit: bool,
}
