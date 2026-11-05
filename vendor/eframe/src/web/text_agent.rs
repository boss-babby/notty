//! The text agent is a hidden `<input>` element used to capture
//! IME and mobile keyboard input events.

use std::cell::Cell;

use wasm_bindgen::prelude::*;

use super::{AppRunner, WebRunner};

/// How long the app may stop editing text before the soft keyboard is dismissed.
pub const KEYBOARD_GRACE_SECS: f64 = 0.35;

pub struct TextAgent {
    input: web_sys::HtmlInputElement,
    prev_ime_output: Cell<Option<egui::output::IMEOutput>>,
}

impl TextAgent {
    /// Attach the agent to the document.
    pub fn attach(runner_ref: &WebRunner) -> Result<Self, JsValue> {
        let document = web_sys::window().unwrap().document().unwrap();

        // create an `<input>` element
        let input = document
            .create_element("input")?
            .dyn_into::<web_sys::HtmlElement>()?;
        input.set_autofocus(true)?;
        let input = input.dyn_into::<web_sys::HtmlInputElement>()?;
        input.set_type("text");
        input.set_attribute("autocapitalize", "off")?;
        input.set_attribute("autocomplete", "off")?;
        input.set_attribute("autocorrect", "off")?;
        input.set_attribute("spellcheck", "false")?;
        input.set_attribute("aria-hidden", "true")?;
        input.set_attribute("tabindex", "-1")?;

        // append it to `<body>` and hide it outside of the viewport
        let style = input.style();
        style.set_property("background-color", "transparent")?;
        style.set_property("border", "none")?;
        style.set_property("outline", "none")?;
        style.set_property("width", "1px")?;
        style.set_property("height", "1px")?;
        style.set_property("caret-color", "transparent")?;
        // >= 16px: iOS Safari zooms the page into any focused input with a smaller font.
        style.set_property("font-size", "16px")?;
        style.set_property("opacity", "0")?;
        style.set_property("pointer-events", "none")?;
        style.set_property("padding", "0")?;
        style.set_property("margin", "0")?;
        style.set_property("z-index", "-1")?;
        style.set_property("position", "absolute")?;
        style.set_property("top", "0")?;
        style.set_property("left", "0")?;
        document.body().unwrap().append_child(&input)?;

        // attach event listeners

        let on_input = {
            let input = input.clone();
            move |event: web_sys::InputEvent, runner: &mut AppRunner| {
                let text = input.value();
                // (eframe used to `blur(); focus();` here on every keystroke to clear Gboard suggestions. That makes the
                // on-screen keyboard close and re-open while typing on phones, so it is gone.)
                // if `is_composing` is true, then user is using IME, for example: emoji, pinyin, kanji, hangul, etc.
                // In that case, the browser emits both `input` and `compositionupdate` events,
                // and we need to ignore the `input` event.
                if !text.is_empty() && !event.is_composing() {
                    input.set_value("");
                    let event = egui::Event::Text(text);
                    runner.input.raw.events.push(event);
                    runner.needs_repaint.repaint_asap();
                }
            }
        };

        let on_composition_start = {
            let input = input.clone();
            move |_: web_sys::CompositionEvent, runner: &mut AppRunner| {
                input.set_value("");
                let event = egui::Event::Ime(egui::ImeEvent::Enabled);
                runner.input.raw.events.push(event);
                // Repaint moves the text agent into place,
                // see `move_to` in `AppRunner::handle_platform_output`.
                runner.needs_repaint.repaint_asap();
            }
        };

        let on_composition_update = {
            move |event: web_sys::CompositionEvent, runner: &mut AppRunner| {
                let Some(text) = event.data() else { return };
                let event = egui::Event::Ime(egui::ImeEvent::Preedit(text));
                runner.input.raw.events.push(event);
                runner.needs_repaint.repaint_asap();
            }
        };

        let on_composition_end = {
            let input = input.clone();
            move |event: web_sys::CompositionEvent, runner: &mut AppRunner| {
                let Some(text) = event.data() else { return };
                input.set_value("");
                let event = egui::Event::Ime(egui::ImeEvent::Commit(text));
                runner.input.raw.events.push(event);
                runner.needs_repaint.repaint_asap();
            }
        };

        runner_ref.add_event_listener(&input, "input", on_input)?;
        runner_ref.add_event_listener(&input, "compositionstart", on_composition_start)?;
        runner_ref.add_event_listener(&input, "compositionupdate", on_composition_update)?;
        runner_ref.add_event_listener(&input, "compositionend", on_composition_end)?;

        // The canvas doesn't get keydown/keyup events when the text agent is focused,
        // so we need to forward them to the runner:
        runner_ref.add_event_listener(&input, "keydown", super::events::on_keydown)?;
        runner_ref.add_event_listener(&input, "keyup", super::events::on_keyup)?;

        Ok(Self {
            input,
            prev_ime_output: Default::default(),
        })
    }

    pub fn move_to(
        &self,
        ime: Option<egui::output::IMEOutput>,
        canvas: &web_sys::HtmlCanvasElement,
        zoom_factor: f32,
    ) -> Result<(), JsValue> {
        // Don't move the text agent unless the position actually changed:
        if self.prev_ime_output.get() == ime {
            return Ok(());
        }
        self.prev_ime_output.set(ime);

        let Some(ime) = ime else { return Ok(()) };

        let mut canvas_rect = super::canvas_content_rect(canvas);
        // Fix for safari with virtual keyboard flapping position
        if is_mobile_safari() {
            canvas_rect.min.y = canvas.offset_top() as f32;
        }
        let cursor_rect = ime.cursor_rect.translate(canvas_rect.min.to_vec2());

        let style = self.input.style();

        // This is where the IME input will point to:
        style.set_property(
            "left",
            &format!("{}px", cursor_rect.center().x * zoom_factor),
        )?;
        style.set_property(
            "top",
            &format!("{}px", cursor_rect.center().y * zoom_factor),
        )?;

        Ok(())
    }

    pub fn set_focus(&self, on: bool) {
        if on {
            self.focus();
        } else {
            self.blur();
        }
    }

    pub fn has_focus(&self) -> bool {
        super::has_focus(&self.input)
    }

    pub fn focus(&self) {
        if self.has_focus() {
            return;
        }

        log::trace!("Focusing text agent");

        let opts = web_sys::FocusOptions::new();
        opts.set_prevent_scroll(true);
        if let Err(err) = self.input.focus_with_options(&opts) {
            log::error!("failed to set focus: {}", super::string_from_js_value(&err));
        };
    }

    pub fn blur(&self) {
        if !self.has_focus() {
            return;
        }

        log::trace!("Blurring text agent");

        if let Err(err) = self.input.blur() {
            log::error!("failed to set focus: {}", super::string_from_js_value(&err));
        };
    }
}

impl Drop for TextAgent {
    fn drop(&mut self) {
        self.input.remove();
    }
}

/// Returns `true` if the app is likely running on a mobile device on navigator Safari.
fn is_mobile_safari() -> bool {
    (|| {
        let user_agent = web_sys::window()?.navigator().user_agent().ok()?;
        let is_ios = user_agent.contains("iPhone")
            || user_agent.contains("iPad")
            || user_agent.contains("iPod");
        let is_safari = user_agent.contains("Safari");
        Some(is_ios && is_safari)
    })()
    .unwrap_or(false)
}
