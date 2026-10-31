//! Notices: the dots on the map and the notes they open.

use std::collections::HashMap;

use web_sys::Element;

use super::{NOTE_FLIP_PX, NOTE_UP_FRACTION};

/// A notice to show at a dot: what it's about, and each notice in it.
pub(super) struct Note {
    /// Names the boat or terminal across frames.
    pub(super) key: String,
    /// The dot's center and diameter, in CSS px.
    pub(super) at: (f64, f64),
    pub(super) size: f64,
    /// Faded back while something else is highlighted.
    pub(super) dim: bool,
    pub(super) heading: String,
    pub(super) items: Vec<NoteItem>,
}

pub(super) struct NoteItem {
    pub(super) title: String,
    /// Paragraphs, blank lines apart; may be empty.
    pub(super) text: String,
    pub(super) when: Option<String>,
}

/// The notice dots over the map (#notes): each a button with a panel that
/// shows on hover or keyboard focus, and stays open once clicked, until
/// clicked again, Escape, or a click on the map. Built with text nodes
/// only: WSF's text never becomes markup.
pub(super) struct Notes {
    pub(super) root: Element,
    /// Each note's element, and the content it was built with.
    pub(super) items: HashMap<String, (Element, String)>,
    pub(super) open: Option<String>,
}

impl Notes {
    /// Shows `notes` in a window `size` CSS px, and hides the rest.
    pub(super) fn place(&mut self, notes: &[Note], size: (f64, f64)) {
        let document = self.root.owner_document().expect("document");
        for note in notes {
            let content = note_content(note);
            let (el, built) = self.items.entry(note.key.clone()).or_insert_with(|| {
                let el = document.create_element("div").expect("div");
                let _ = el.set_attribute("data-key", &note.key);
                let _ = self.root.append_child(&el);
                (el, String::new())
            });
            if *built != content {
                build_note(&document, el, note);
                *built = content;
            }
            let (x, y) = note.at;
            let mut class = String::from("note");
            if x > size.0 - NOTE_FLIP_PX {
                class += " flip";
            }
            if y > size.1 * NOTE_UP_FRACTION {
                class += " up";
            }
            if self.open.as_deref() == Some(note.key.as_str()) {
                class += " open";
            }
            if note.dim {
                class += " dim";
            }
            el.set_class_name(&class);
            let _ = el.set_attribute(
                "style",
                &format!(
                    "transform: translate({x:.1}px, {y:.1}px); --dot: {:.2}px",
                    note.size
                ),
            );
            let _ = el.remove_attribute("hidden");
        }
        for (key, (el, _)) in &self.items {
            if !notes.iter().any(|n| n.key == *key) {
                let _ = el.set_attribute("hidden", "");
            }
        }
        self.mark_open();
    }

    /// Opens the note `key`, or closes it if it's open.
    pub(super) fn toggle(&mut self, key: &str) {
        self.open = if self.open.as_deref() == Some(key) {
            None
        } else {
            Some(key.to_string())
        };
        self.mark_open();
    }

    /// Closes the open note; says whether one was.
    pub(super) fn close(&mut self) -> bool {
        let was_open = self.open.take().is_some();
        self.mark_open();
        was_open
    }

    fn mark_open(&self) {
        for (key, (el, _)) in &self.items {
            let open = self.open.as_deref() == Some(key.as_str());
            let _ = el.class_list().toggle_with_force("open", open);
            if let Some(dot) = el.first_element_child() {
                let _ = dot.set_attribute("aria-expanded", if open { "true" } else { "false" });
            }
        }
    }
}

/// What a note's element shows, to tell when it needs building again.
fn note_content(note: &Note) -> String {
    let mut content = note.heading.clone();
    for item in &note.items {
        for part in [&item.title, &item.text, item.when.as_deref().unwrap_or("")] {
            content.push('\u{1f}');
            content += part;
        }
    }
    content
}

/// Fills `el` with a note's dot and panel.
fn build_note(document: &web_sys::Document, el: &Element, note: &Note) {
    let child = |parent: &Element, tag: &str, class: &str, text: &str| {
        let e = document.create_element(tag).expect("element");
        if !class.is_empty() {
            e.set_class_name(class);
        }
        if !text.is_empty() {
            e.set_text_content(Some(text));
        }
        let _ = parent.append_child(&e);
        e
    };
    el.set_text_content(None);
    let dot = child(el, "button", "dot", "i");
    let _ = dot.set_attribute("type", "button");
    let count = match note.items.len() {
        1 => "a notice".to_string(),
        n => format!("{n} notices"),
    };
    let _ = dot.set_attribute("aria-label", &format!("{}: {count}", note.heading));
    let panel = child(el, "div", "panel", "");
    child(&panel, "div", "about", &note.heading);
    for item in &note.items {
        let section = child(&panel, "section", "", "");
        child(&section, "h3", "", &item.title);
        if let Some(when) = &item.when {
            child(&section, "div", "when", when);
        }
        if !item.text.is_empty() {
            child(&section, "p", "", &item.text);
        }
    }
}

pub(super) fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
