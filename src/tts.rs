//! Text-to-speech over Google's keyless `translate_tts` endpoint, played by gstreamer.
//!
//! - the endpoint rejects text longer than 200 chars, so longer input is split on
//!   line boundaries (reusing `translate::split_text`) and the chunks are queued;
//! - `playbin` does download, MP3 decode and PipeWire output: no temp files, no
//!   audio threads, the UI thread never blocks;
//! - language codes are Google's, the same ones `LANGUAGES` already carries;
//!   `auto` is rejected by the endpoint, so the caller resolves it first.

use crate::translate::split_text;
use gstreamer as gst;
use gst::glib;
use gst::prelude::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

const ENDPOINT: &str = "https://translate.google.com/translate_tts";
/// the endpoint rejects longer requests, counted in characters
const MAX_CHARS: usize = 200;

pub const SPEAK_ICON: &str = "audio-speakers-symbolic";
pub const STOP_ICON: &str = "media-playback-stop-symbolic";

/// URL of one chunk's audio; `Url` percent-encodes the query for us.
fn chunk_url(text: &str, lang: &str) -> String {
    let textlen = text.chars().count().to_string();
    reqwest::Url::parse_with_params(
        ENDPOINT,
        &[
            ("ie", "UTF-8"),
            ("q", text),
            ("tl", lang),
            ("client", "gtx"),
            ("textlen", textlen.as_str()),
        ],
    )
    .expect("endpoint is a constant URL")
    .to_string()
}

struct Inner {
    queue: VecDeque<String>,
    lang: String,
    /// called when everything is done (`None`) or a chunk failed (`Some(reason)`)
    finished: Box<dyn FnMut(Option<&str>)>,
}

/// One playback session. Dropping it stops the output mid-word.
pub struct Speaker {
    player: gst::Element,
    inner: Rc<RefCell<Inner>>,
    /// ends the bus watch when the session is dropped; never read on purpose
    #[allow(dead_code)]
    watch: gst::bus::BusWatchGuard,
}

impl Speaker {
    /// Split `text` into chunks and start playing the first one in `lang`.
    pub fn start(
        text: &str,
        lang: &str,
        finished: impl FnMut(Option<&str>) + 'static,
    ) -> anyhow::Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("nothing to speak");
        }
        gst::init()?;
        let player = gst::ElementFactory::make("playbin").build()?;
        let inner = Rc::new(RefCell::new(Inner {
            queue: split_text(text, MAX_CHARS).into(),
            lang: lang.to_string(),
            finished: Box::new(finished),
        }));
        let Some(first) = inner.borrow_mut().queue.pop_front() else {
            anyhow::bail!("nothing to speak");
        };
        let watch = Self::watch_bus(&player, Rc::downgrade(&inner))?;
        play_chunk(&player, &first, lang)?;
        Ok(Self {
            player,
            inner,
            watch,
        })
    }

    pub fn stop(&mut self) {
        // Null silences the output; the bus watch dies with the session, so any
        // chunks left in the queue never play and need no clearing. (Touching
        // inner here would also re-enter the very borrow a finished() call may
        // still be holding.)
        let _ = self.player.set_state(gst::State::Null);
    }

    /// advance the queue on end-of-chunk; report failure and the final end
    fn watch_bus(
        player: &gst::Element,
        inner: Weak<RefCell<Inner>>,
    ) -> anyhow::Result<gst::bus::BusWatchGuard> {
        let player = player.clone();
        // local: Rc state stays on the GTK main thread
        let watch = player
            .bus()
            .expect("every pipeline has a bus")
            .add_watch_local(move |_, msg| match msg.view() {
                gst::MessageView::Eos(..) => {
                    let Some(inner) = inner.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    // one borrow at a time: an if-let on a borrow_mut would
                    // keep it alive for the whole branch
                    let next = inner.borrow_mut().queue.pop_front();
                    if let Some(text) = next {
                        let lang = inner.borrow().lang.clone();
                        if let Err(e) = play_chunk(&player, &text, &lang) {
                            (inner.borrow_mut().finished)(Some(&e.to_string()));
                        }
                    } else {
                        (inner.borrow_mut().finished)(None);
                    }
                    glib::ControlFlow::Continue
                }
                gst::MessageView::Error(e) => {
                    if let Some(inner) = inner.upgrade() {
                        (inner.borrow_mut().finished)(Some(&e.error().to_string()));
                    }
                    let _ = player.set_state(gst::State::Null);
                    glib::ControlFlow::Continue
                }
                _ => glib::ControlFlow::Continue,
            })?;
        Ok(watch)
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn play_chunk(player: &gst::Element, text: &str, lang: &str) -> anyhow::Result<()> {
    // down first: a uri set on a Playing pipeline reloads at the wrong time
    player.set_state(gst::State::Ready)?;
    player.set_property("uri", chunk_url(text, lang));
    player.set_state(gst::State::Playing)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_url_encodes_the_query() {
        let url = chunk_url("你好 world", "zh-CN");
        assert!(url.starts_with(ENDPOINT), "{url}");
        assert!(url.contains("tl=zh-CN"), "{url}");
        assert!(url.contains("q=%E4%BD%A0%E5%A5%BD+world"), "{url}");
        assert!(url.contains("textlen=8"), "{url}");
    }

    #[test]
    fn empty_text_is_refused_without_touching_the_network() {
        assert!(Speaker::start("  \n ", "en", |_| {}).is_err());
    }
}
