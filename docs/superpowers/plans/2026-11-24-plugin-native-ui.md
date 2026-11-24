# Plugin Native UI on Linux/X11 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render external CLAP/VST3 plugin editors as their designed native UI on Linux/X11, with a persisted slider mode as the default.

**Architecture:** Keep the existing sandboxed plugin process and editor IPC. Add a shared X11 window helper, extend CLAP and VST3 view bridges to attach to it, and replace the app's hard-coded Editor preference with a persisted Sliders/Editor choice plus a pure view selector. The native editor is a separate OS window; embedding into egui is deferred.

**Tech Stack:** Rust 2021, egui/eframe, `x11-dl` 2.21, hand-written CLAP/VST3 C ABIs, sandboxed child process IPC.

**Spec:** `docs/superpowers/specs/2026-11-24-plugin-native-ui-design.md`

## Global Constraints

- Default plugin UI mode is **Sliders**; Editor is opt-in.
- `prowave-plugin-host` owns all unsafe plugin/FFI code; every unsafe block needs a SAFETY comment.
- UI code must not mutate the audio engine directly; view changes stay in `UiState`.
- No allocation or locking on the audio thread.
- Plugin processes remain sandboxed; a crashed editor must not take down Pro Wave.
- On Linux, CLAP uses X11 API id `"x11"`; VST3 uses platform id `"X11EmbedWindowID"`.
- If `DISPLAY` is absent or window creation fails, report no editor and show sliders.
- WASM cannot host native plugin GUIs; sliders remain the only path there.

## Review Focus

- Headless Linux/no `DISPLAY`: must report no editor and keep sliders without panicking — Task 3 tests `available_from_env(None)` and empty display.
- Editor preference with no usable editor or crashed plugin: must fall back to sliders — Task 2 tests `plugin_view(false, ...)` and `plugin_view(true, true, ...)`.
- VST3 editors needing XEmbed and `IRunLoop`: must attach and receive fd/timer callbacks — Task 5 tests the X11 platform id/run-loop iid and Task 6 wires `pump()` into sandbox tick; manual VST3 ignored test covers a real plugin.
- Native window close/resize/power events: must report close, honor requested size, and toggle bypass without losing audio — Task 3 tests `take_closed`/`take_power_click` state transitions; Task 6 wires existing resize/close/power IPC.
- Audio must be unaffected: app/plugin-host tests and clippy in Tasks 2 and 6.

---

### Task 1: Branch and persisted plugin UI preference

**Files:**
- Create: `crates/prowave-app/src/plugin_ui_prefs.rs`
- Modify: `crates/prowave-app/src/main.rs`
- Commit: `docs/superpowers/specs/2026-11-24-plugin-native-ui-design.md`, `docs/superpowers/plans/2026-11-24-plugin-native-ui.md`

**Interfaces:**
- Consumes: `crate::recent_files::config_dir()`
- Produces:
  - `pub(crate) enum PluginUiMode { Sliders, Editor }` with `#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]`; `Sliders` is `#[default]`.
  - `pub(crate) fn parse(text: &str) -> PluginUiMode`
  - `pub(crate) fn to_text(mode: PluginUiMode) -> String`
  - `pub(crate) fn load() -> PluginUiMode`
  - `pub(crate) fn save(mode: PluginUiMode)`
  - `pub(crate) fn load_from(dir: &std::path::Path) -> PluginUiMode`
  - `pub(crate) fn save_to(dir: &std::path::Path, mode: PluginUiMode)`

- [ ] **Step 1: Create the feature branch from the fork's main**

Run:

```bash
git fetch origin
git switch -c feature/plugin-native-ui-x11 origin/main
git status --short --branch
```

Expected: clean branch based on upstream `origin/main`; the two design docs remain untracked/carried over.

- [ ] **Step 2: Write the failing tests in `plugin_ui_prefs.rs`**

```rust
#[test]
fn sliders_are_the_default() {
    assert_eq!(PluginUiMode::default(), PluginUiMode::Sliders);
}

#[test]
fn saved_mode_round_trips() {
    for mode in [PluginUiMode::Sliders, PluginUiMode::Editor] {
        assert_eq!(PluginUiMode::parse(&PluginUiMode::to_text(mode)), mode);
    }
}

#[test]
fn unknown_or_empty_text_keeps_the_default() {
    assert_eq!(PluginUiMode::parse(""), PluginUiMode::Sliders);
    assert_eq!(PluginUiMode::parse("mode=nonsense\n"), PluginUiMode::Sliders);
}

#[test]
fn load_from_and_save_to_use_the_config_directory() {
    let dir = std::env::temp_dir().join(format!("prowave-plugin-ui-{}-{}", std::process::id(), line!()));
    let _ = std::fs::create_dir_all(&dir);
    save_to(&dir, PluginUiMode::Editor);
    assert_eq!(load_from(&dir), PluginUiMode::Editor);
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p prowave-app plugin_ui_prefs -- --nocapture`  
Expected: FAIL because `plugin_ui_prefs` does not exist.

- [ ] **Step 4: Implement the preference module**

Use `plugin-ui.txt` under `recent_files::config_dir()`. Parse/save one line `mode=sliders` or `mode=editor`. `save()` must be a no-op under `cfg!(test)` so menu tests never touch the user's settings. Use best-effort `std::fs::write` and `tracing::warn!` on failure, matching `ui_scale.rs`.

- [ ] **Step 5: Register the module in `main.rs`**

Add `mod plugin_ui_prefs;` beside the other app modules.

- [ ] **Step 6: Run the tests and checks**

Run: `cargo test -p prowave-app plugin_ui_prefs -- --nocapture`  
Expected: PASS with 4 tests.

Run: `cargo fmt --check`  
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/prowave-app/src/main.rs crates/prowave-app/src/plugin_ui_prefs.rs docs/superpowers/specs/2026-11-24-plugin-native-ui-design.md docs/superpowers/plans/2026-11-24-plugin-native-ui.md
git commit -m "feat: persist plugin UI mode with sliders default"
```

---

### Task 2: App view selector and preference control

**Files:**
- Modify: `crates/prowave-app/src/mixer.rs:80-105`, `crates/prowave-app/src/mixer.rs:116-134`, `crates/prowave-app/src/mixer.rs:921-942`, `crates/prowave-app/src/mixer.rs:997-1038`
- Modify: `crates/prowave-app/src/fx_ui.rs:215-297`, `crates/prowave-app/src/fx_ui.rs:299-396`
- Modify: `crates/prowave-app/src/plugin_manager.rs:152-228`

**Interfaces:**
- Consumes: `PluginUiMode` from Task 1.
- Produces:
  - `pub(crate) enum PluginView { Sliders, Editor }` in `fx_ui.rs`
  - `pub(crate) fn plugin_view(has_editor: bool, crashed: bool, preferred: PluginUiMode, explicit: Option<PluginUiMode>) -> PluginView` in `fx_ui.rs`
  - `MixerView::with_plugin_ui(mode: PluginUiMode) -> Self`
  - `MixerView.preferred_plugin_ui: PluginUiMode`
  - `MixerView.plugin_view_override: Option<PluginUiMode>`

- [ ] **Step 1: Write the failing selector tests in `fx_ui.rs`**

```rust
#[test]
fn plugin_view_defaults_to_the_preference() {
    assert_eq!(
        plugin_view(true, false, PluginUiMode::Sliders, None),
        PluginView::Sliders
    );
    assert_eq!(
        plugin_view(true, false, PluginUiMode::Editor, None),
        PluginView::Editor
    );
}

#[test]
fn plugin_view_honors_an_explicit_choice() {
    assert_eq!(
        plugin_view(true, false, PluginUiMode::Sliders, Some(PluginUiMode::Editor)),
        PluginView::Editor
    );
    assert_eq!(
        plugin_view(true, false, PluginUiMode::Editor, Some(PluginUiMode::Sliders)),
        PluginView::Sliders
    );
}

#[test]
fn plugin_view_falls_back_when_there_is_no_usable_editor() {
    assert_eq!(
        plugin_view(false, false, PluginUiMode::Editor, None),
        PluginView::Sliders
    );
    assert_eq!(
        plugin_view(true, true, PluginUiMode::Editor, Some(PluginUiMode::Editor)),
        PluginView::Sliders
    );
}
```

- [ ] **Step 2: Write the failing `MixerView` test**

```rust
#[test]
fn mixer_defaults_to_the_saved_plugin_ui_mode() {
    assert_eq!(
        MixerView::with_plugin_ui(PluginUiMode::Editor).preferred_plugin_ui,
        PluginUiMode::Editor
    );
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p prowave-app plugin_view mixer_defaults -- --nocapture`  
Expected: FAIL because the selector and constructor do not exist.

- [ ] **Step 4: Implement the selector and `MixerView` fields**

Replace `plugin_controls: bool` with `preferred_plugin_ui: PluginUiMode` and `plugin_view_override: Option<PluginUiMode>`. `MixerView::default()` calls `Self::with_plugin_ui(plugin_ui_prefs::load())`. `plugin_view` returns Sliders first when `crashed || !has_editor`, otherwise explicit override, otherwise preference.

- [ ] **Step 5: Wire `plugin_window` and the mixer actions**

In `plugin_window`, compute `has_editor`, call `plugin_view(...)`, and set `wants_editor` from `PluginView::Editor`. The **Editor** button sets `plugin_view_override = Some(Editor)` and clears `plugin_editor`; add a **Controls** action in the insert context menu that sets `Some(Sliders)`. **Open** and double-click clear the override so the preference decides. Native editor mode keeps the existing grace period, close detection, power-click handling, and fallback.

- [ ] **Step 6: Add the preference control to Plugin Manager**

In the bottom panel, add a compact mode picker bound to `s.mixer.preferred_plugin_ui`. On change, call `plugin_ui_prefs::save(mode)` and clear `s.mixer.plugin_view_override`. Label it as plug-in UI mode with Sliders as the portable default.

- [ ] **Step 7: Run tests and checks**

Run: `cargo test -p prowave-app plugin_view mixer_defaults fx_ui -- --nocapture`  
Expected: PASS.

Run: `cargo clippy -p prowave-app --all-targets -- -D warnings`  
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/prowave-app/src/fx_ui.rs crates/prowave-app/src/mixer.rs crates/prowave-app/src/plugin_manager.rs
git commit -m "feat: select plugin UI mode from preference"
```

---

### Task 3: X11 window helper

**Files:**
- Modify: `crates/prowave-plugin-host/Cargo.toml`
- Modify: `crates/prowave-plugin-host/src/lib.rs`
- Create: `crates/prowave-plugin-host/src/x11.rs`

**Interfaces:**
- Consumes: `x11-dl` 2.21, `libc` 0.2
- Produces in `lib.rs`:
  - `pub(crate) enum EditorParent { Cocoa(*mut std::ffi::c_void), X11 { display: *mut std::ffi::c_void, window: u64 } }`
- Produces in `x11.rs`:
  - `pub(crate) fn available_from_env(display: Option<&std::ffi::OsStr>) -> bool`
  - `pub(crate) struct Window`
  - `pub(crate) fn Window::new(title: &str) -> Option<Window>`
  - `pub(crate) fn Window::editor_parent(&self) -> EditorParent`
  - `pub(crate) fn Window::set_title(&self, title: &str)`
  - `pub(crate) fn Window::set_plugin_size(&self, width: u32, height: u32)`
  - `pub(crate) fn Window::show(&self)`
  - `pub(crate) fn Window::hide(&self)`
  - `pub(crate) fn Window::pump(&self)`
  - `pub(crate) fn Window::take_closed(&self) -> bool`
  - `pub(crate) fn Window::take_power_click(&self) -> bool`

- [ ] **Step 1: Write the failing display-gating tests in `x11.rs`**

```rust
#[test]
fn no_display_means_no_editor() {
    assert!(!available_from_env(None));
    assert!(!available_from_env(Some(std::ffi::OsStr::new(""))));
}

#[test]
fn a_display_name_allows_an_editor() {
    assert!(available_from_env(Some(std::ffi::OsStr::new(":0"))));
}
```

- [ ] **Step 2: Write the failing event-state tests**

```rust
#[test]
fn close_and_power_clicks_are_latched() {
    // Use the helper's test-only event injector, not a real display.
    let state = EventState::default();
    state.note_close();
    state.note_power_click();
    assert!(state.take_closed());
    assert!(state.take_power_click());
    assert!(!state.take_power_click());
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p prowave-plugin-host x11 -- --nocapture`  
Expected: FAIL because the module does not exist.

- [ ] **Step 4: Add the dependency and module**

Add under `[target.'cfg(target_os = "linux")'.dependencies]`:

```toml
x11-dl = "2.21"
```

Add `#[cfg(target_os = "linux")] pub(crate) mod x11;` to `lib.rs` and define `EditorParent` there.

- [ ] **Step 5: Implement the X11 helper**

Create a top-level X11 window with a 28-pixel host footer containing a clickable power rectangle. The plugin view parent is the content area. Handle `WM_DELETE_WINDOW`, `ClientMessage`, `ConfigureNotify`, `ButtonPress`, XEmbed `_XEMBED`/`_XEMBED_INFO` atoms, and child `CreateNotify`/`PropertyNotify` events. `pump()` drains pending events without blocking. Every unsafe block gets a SAFETY comment.

- [ ] **Step 6: Run tests and checks**

Run: `cargo test -p prowave-plugin-host x11 -- --nocapture`  
Expected: PASS with 3 tests.

Run: `cargo clippy -p prowave-plugin-host --all-targets -- -D warnings`  
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/prowave-plugin-host/Cargo.toml crates/prowave-plugin-host/src/lib.rs crates/prowave-plugin-host/src/x11.rs
git commit -m "feat: add X11 editor window helper"
```

---

### Task 4: CLAP X11 editor bridge

**Files:**
- Modify: `crates/prowave-plugin-host/src/clap.rs`
- Modify: `crates/prowave-plugin-host/src/lib.rs`

**Interfaces:**
- Consumes: `EditorParent` from Task 3.
- Produces:
  - `pub(crate) const CLAP_X11_API: &CStr = c"x11"` on Linux
  - `ClapX11Window` struct with layout `{ display: *mut c_void, window: u64 }`

- [ ] **Step 1: Write the failing CLAP X11 tests in `clap.rs`**

```rust
#[cfg(target_os = "linux")]
#[test]
fn clap_uses_the_x11_window_api() {
    assert_eq!(CLAP_X11_API.to_bytes(), b"x11");
}

#[cfg(target_os = "linux")]
#[test]
fn clap_x11_window_handle_has_the_abi_layout() {
    assert_eq!(
        std::mem::size_of::<ClapX11Window>(),
        std::mem::size_of::<*mut std::ffi::c_void>() + std::mem::size_of::<u64>()
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p prowave-plugin-host clap_x11 -- --nocapture`  
Expected: FAIL because the API constant and struct do not exist.

- [ ] **Step 3: Implement the CLAP bridge**

On Linux, make `WINDOW_API` resolve to `CLAP_X11_API`. In `main_editor_open`, accept `EditorParent::X11 { display, window }`, build a `GuiWindow` whose `handle` points to a `ClapX11Window { display, window }`, and keep the existing create/set_parent/get_size/show sequence. On macOS keep `EditorParent::Cocoa`. Every unsafe block gets a SAFETY comment.

- [ ] **Step 4: Run tests and checks**

Run: `cargo test -p prowave-plugin-host -- --nocapture`  
Expected: PASS.

Run: `cargo clippy -p prowave-plugin-host --all-targets -- -D warnings`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/prowave-plugin-host/src/clap.rs crates/prowave-plugin-host/src/lib.rs
git commit -m "feat: attach CLAP editors to X11 windows"
```

---

### Task 5: VST3 X11 view and Linux run loop

**Files:**
- Modify: `crates/prowave-plugin-host/src/vst3.rs`

**Interfaces:**
- Consumes: `EditorParent` from Task 3.
- Produces:
  - `pub(crate) const VST3_X11_PLATFORM: &CStr = c"X11EmbedWindowID"` on Linux
  - `const IID_RUNLOOP: Tuid = iid(0x18C35366, 0x97764F1A, 0x9C5B8385, 0x7A871389)`
  - `RunLoop` with `pump(&self)`

- [ ] **Step 1: Write the failing VST3 X11 tests in `vst3.rs`**

```rust
#[cfg(target_os = "linux")]
#[test]
fn vst3_uses_the_x11_embed_platform() {
    assert_eq!(VST3_X11_PLATFORM.to_bytes(), b"X11EmbedWindowID");
}

#[cfg(target_os = "linux")]
#[test]
fn vst3_runloop_iid_is_stable() {
    assert_ne!(IID_RUNLOOP, IID_UNKNOWN);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p prowave-plugin-host vst3_x11 vst3_runloop -- --nocapture`  
Expected: FAIL because the constant and IID do not exist.

- [ ] **Step 3: Implement the VST3 view bridge**

On Linux, make `main_has_editor()` return true for a live controller. In `main_editor_open`, accept `EditorParent::X11 { window, .. }`, create the view with `c"editor"`, check `supported` with `VST3_X11_PLATFORM`, call `set_frame`, and call `attached(window as *mut c_void, VST3_X11_PLATFORM.as_ptr())`. Keep the macOS `NSView` path unchanged. Every unsafe block gets a SAFETY comment.

- [ ] **Step 4: Implement the Linux `IRunLoop`**

Add `IRunLoop`, `IEventHandler`, and `ITimerHandler` vtables and a `RunLoop` registry with `register_event_handler`, `unregister_event_handler`, `register_timer`, and `unregister_timer`. `Frame::query_interface` answers `IID_RUNLOOP` with the run-loop object; `Frame::pump()` polls registered fds and calls due timers. Keep `Frame::wanted` resize behavior unchanged.

- [ ] **Step 5: Run tests and checks**

Run: `cargo test -p prowave-plugin-host -- --nocapture`  
Expected: PASS.

Run: `cargo clippy -p prowave-plugin-host --all-targets -- -D warnings`  
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/prowave-plugin-host/src/vst3.rs
git commit -m "feat: attach VST3 editors to X11 with run loop"
```

---

### Task 6: Sandbox editor lifecycle and final verification

**Files:**
- Modify: `crates/prowave-plugin-host/src/sandbox.rs`
- Modify: `crates/prowave-plugin-host/src/lib.rs`
- Modify: `crates/prowave-plugin-host/tests/installed.rs`

**Interfaces:**
- Consumes: `x11::Window`, `EditorParent`, `RunLoop::pump()`.
- Produces: `editor_capable(os: EditorOs, display_available: bool, has_view: bool) -> bool`

- [ ] **Step 1: Write the failing capability test in `sandbox.rs`**

```rust
#[test]
fn editors_require_a_supported_os_and_a_view() {
    assert!(editor_capable(EditorOs::Linux, true, true));
    assert!(!editor_capable(EditorOs::Linux, false, true));
    assert!(!editor_capable(EditorOs::Linux, true, false));
    assert!(editor_capable(EditorOs::Macos, true, true));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p prowave-plugin-host editor_capable -- --nocapture`  
Expected: FAIL because the helper does not exist.

- [ ] **Step 3: Implement the sandbox Linux editor lifecycle**

On `editor open`, create `x11::Window::new(title)`, pass `window.editor_parent()` to `main_editor_open`, size it from the returned `(width, height)`, show it, and report `e\teditor\t1`. On close, hide/drop the window and report `e\teditor\t0`. In `tick()`, call `window.pump()`, and on Linux call the VST3 `RunLoop::pump()` as well. Keep resize, close, and power IPC unchanged. Set `has_editor` from `editor_capable(...)`, using `x11::available_from_env(std::env::var_os("DISPLAY").as_deref())` on Linux.

- [ ] **Step 4: Add the ignored manual X11 tests**

In `tests/installed.rs`, add one ignored test for a CLAP plugin with an editor and one for a VST3 plugin with an editor. Each asserts only that the host reports `has_editor()` on a display; visual verification is manual. Keep them ignored by default.

- [ ] **Step 5: Run full verification**

Run: `cargo test -p prowave-app -- --nocapture`  
Expected: PASS with no new failures.

Run: `cargo test -p prowave-plugin-host -- --nocapture`  
Expected: PASS; ignored manual tests remain ignored.

Run: `cargo clippy -p prowave-app -p prowave-plugin-host --all-targets -- -D warnings`  
Expected: PASS.

Run: `cargo fmt --check` and `git diff --check`  
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/prowave-plugin-host/src/sandbox.rs crates/prowave-plugin-host/src/lib.rs crates/prowave-plugin-host/tests/installed.rs
git commit -m "feat: host CLAP and VST3 editors on Linux X11"
```
