//! Core library: config/translate/screenshot/clipboard, GTK window and app skeleton.
//!
//! The OCR engine lives in the sibling crate [kosuzu-ocr](../kosuzu-ocr); this crate
//! only calls `kosuzu_ocr::OcrEngine`.

pub mod app;
pub mod args;
pub mod clipboard;
pub mod config;
pub mod screenshot;
pub mod translate;
pub mod tray;
pub mod tts;
pub mod ui;
pub mod worker;
