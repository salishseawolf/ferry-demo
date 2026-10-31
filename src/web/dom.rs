//! Small helpers for the page: text, clocks, media queries, the clipboard,
//! events and frames.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::Element;

use super::App;

/// How long the copy button shows its check mark.
const COPIED_MS: i32 = 1500;
/// Ferry times are shown in the ferries' own time zone, wherever the
/// viewer is.
const FERRY_TIME_ZONE: &str = "America/Los_Angeles";

/// Whether the page's tab is in the background (or the window minimized).
pub(super) fn page_hidden() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| d.hidden())
}

pub(super) async fn sleep_ms(ms: f64) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ =
                window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms as i32);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

pub(super) fn set_text(el: &Element, text: &str) {
    if el.text_content().as_deref() != Some(text) {
        el.set_text_content(Some(text));
    }
}

/// Whether the viewer has asked for less motion.
pub(super) fn prefers_reduced_motion() -> bool {
    media_matches("(prefers-reduced-motion: reduce)")
}

/// Whether the page is laid out for a phone (index.html's narrow layout).
pub(super) fn phone_layout() -> bool {
    media_matches("(max-width: 700px)")
}

fn media_matches(query: &str) -> bool {
    web_sys::window()
        .and_then(|w| w.match_media(query).ok().flatten())
        .is_some_and(|q| q.matches())
}

/// A time (ms since the epoch) on the ferries' clock, e.g. "11:31 AM".
pub(super) fn clock(epoch_ms: f64) -> String {
    let options = js_sys::Object::new();
    for (key, value) in [
        ("hour", "numeric"),
        ("minute", "2-digit"),
        ("timeZone", FERRY_TIME_ZONE),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    js_sys::Date::new(&JsValue::from_f64(epoch_ms))
        .to_locale_time_string_with_options("en-US", &options)
        .into()
}

/// A date and time on the ferries' clock, e.g. "Oct 29, 11:31 AM".
pub(super) fn date_clock(epoch_ms: f64) -> String {
    let options = js_sys::Object::new();
    for (key, value) in [
        ("month", "short"),
        ("day", "numeric"),
        ("hour", "numeric"),
        ("minute", "2-digit"),
        ("timeZone", FERRY_TIME_ZONE),
    ] {
        let _ = js_sys::Reflect::set(&options, &key.into(), &value.into());
    }
    js_sys::Date::new(&JsValue::from_f64(epoch_ms))
        .to_locale_string("en-US", &options)
        .into()
}

/// Milliseconds from the page's high-resolution clock.
pub(super) fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map_or(0.0, |p| p.now())
}

/// Puts `text` on the clipboard and briefly swaps `button`'s icon to a
/// check mark (see `.copied` in index.html).
pub(super) fn copy_to_clipboard(text: &str, button: &Element) {
    let Some(window) = web_sys::window() else {
        return;
    };
    // The promise rejects only without permission; there is nothing more
    // useful to do then than skip the check mark.
    let _ = window.navigator().clipboard().write_text(text);
    let _ = button.class_list().add_1("copied");
    let button = button.clone();
    let reset = Closure::once_into_js(move || {
        let _ = button.class_list().remove_1("copied");
    });
    let _ = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(reset.unchecked_ref(), COPIED_MS);
}

/// Register `handler` for `event` on `target` for the life of the page.
pub(super) fn listen<E: JsCast + 'static>(
    target: &web_sys::EventTarget,
    event: &str,
    app: &Rc<RefCell<App>>,
    handler: impl Fn(&mut App, E) + 'static,
) {
    let app = app.clone();
    let closure = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        handler(&mut app.borrow_mut(), e.unchecked_into());
    });
    target
        .add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())
        .expect("addEventListener");
    closure.forget();
}

pub(super) fn request_frame(f: &Closure<dyn FnMut(f64)>) {
    web_sys::window()
        .unwrap()
        .request_animation_frame(f.as_ref().unchecked_ref())
        .expect("requestAnimationFrame");
}

pub(super) fn show_error(message: &str) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("error"))
    {
        el.set_text_content(Some(message));
        let _ = el.remove_attribute("hidden");
    }
}
