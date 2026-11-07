# Patches on top of eframe 0.31.1 (web backend only)

1. `events.rs` — `pointerdown` / `pointerup` of `pointerType == "touch"` are ignored; the touch listeners deliver the finger. Browsers fire
   the pointer events *before* the touch events, so the press reached egui twice and before the app's gesture layer (`src/touch.rs`)
   knew a finger was down. A scroll swipe therefore started a drag (marquee selection) instead of scrolling.
2. `events.rs` — `touchstart` pushes `Event::Touch` *before* the `PointerButton`; `touchend` runs the app logic synchronously
   (inside the user gesture, which iOS requires to open the keyboard) instead of toggling blur/focus.
3. `app_runner.rs` — the hidden text input is only blurred after `KEYBOARD_GRACE_SECS` without a focused text field. Blurring on every
   frame in which the editor briefly had no focus made the on-screen keyboard open and close in a loop.
4. `text_agent.rs` — no `blur(); focus();` after every keystroke (closed/re-opened the keyboard on Android), `focus({preventScroll})`,
   16 px font (iOS zoom), autocomplete/autocorrect off.
5. `mod.rs` — `native_pixels_per_point()` is the plain `devicePixelRatio` again (5.5 capped it at 2 on touch devices). The cap made the canvas
   (sized in *device* pixels by the ResizeObserver) and the logical coordinates (CSS pixels from touch events) disagree on 3× phones, so a
   tap landed on a different widget than the one under the finger. Never decouple the pixels-per-point from the device pixel ratio.
