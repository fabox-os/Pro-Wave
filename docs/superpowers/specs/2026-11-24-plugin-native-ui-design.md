# Plugin native UI on Linux/X11

## Intent

Issue #189 reports that external audio plugins render as generic normalized sliders instead of their designed UI. Users should be able to see the plugin's own UI when the host can provide it. The slider variant must remain available and must be the default.

## Goals

- On Linux/X11, show hosted **CLAP** and **VST3** plugin editors in a separate native window when the user opts in.
- Add a persisted preference: **Sliders** (default) or **Editor**.
- Keep the existing per-window **Controls** / **Editor** toggle, initialized from the preference.
- Fall back to sliders when the plugin has no editor, has crashed, is still loading, or the platform cannot host the editor.
- Keep audio processing and project state untouched.

## Non-goals

- Embedding the native editor inside the egui window. That is a follow-up.
- Windows HWND support, WebAssembly native editors, and Audio Unit UI outside macOS.
- Rebuilding generic sliders from richer parameter metadata. That improves the fallback but is not the plugin's designed UI.

## Constraints

- `prowave-plugin-host` owns unsafe plugin/FFI code; unsafe additions need SAFETY comments and host tests.
- UI code must not mutate the audio engine directly; view changes stay in `UiState` and existing command/automation paths.
- No allocation or locking on the audio thread.
- Plugin processes must remain sandboxed; a crashed editor must not take down Pro Wave.
- WASM cannot host native plugin GUIs; sliders remain the only viable path there.

## Approaches considered

1. **Native editor in a separate OS window (chosen).**  
   Mirror the existing macOS editor path, but with an X11 window owned by the sandboxed plugin process. Fastest route to the designed UI while keeping the slider default. It is not visually embedded in the DAW window.

2. **Embed the editor into the main window.**  
   Best final UX, closest to Logic. Requires raw-window-handle passing from winit, cross-process event/resize forwarding, and careful focus/z-order handling. Deferred.

3. **Richer generic parameter UI.**  
   Portable and testable, including WASM, but still not the plugin's designed UI. Keep as the fallback path, not as the fix for #189.

## Design

### Preference

Add a small persisted preference following the existing config helpers in `audio_prefs.rs` / `ui_scale.rs`:

```text
PluginUiMode::Sliders  // default
PluginUiMode::Editor   // opt-in
```

Initialize `MixerView` from this preference instead of hard-coding `plugin_controls: false`. Opening an insert from the mixer or channel strip uses the preference. The window's **Editor** / **Controls** buttons explicitly switch the current window and do not rewrite the preference unless the user changes the preference control itself.

The preference control lives in Plugin Manager, next to Rescan, with copy explaining that Sliders is the portable default and Editor uses the plugin's own window where supported.

### View selection

Keep view selection in one pure helper so it can be tested without a GUI:

```text
view(instance, crashed, preference, explicit_override)
  if crashed or instance has no editor -> Sliders
  if explicit_override -> explicit_override
  else -> preference
```

This replaces the current `!plugin_controls && has_editor` check in `fx_ui.rs`. Editor grace period, power-click handling, close detection, and fallback-to-sliders behavior stay as they are.

### Host changes

On Linux/X11:

- Add a new `x11.rs` window helper in `prowave-plugin-host`, behind `target_os = "linux"`, that creates, shows, hides, titles, sizes, and closes an X11 top-level window, plus a small event pump for close/power events.
- Extend the CLAP GUI bridge from Cocoa-only to also provide X11 display/window data to the plugin through `clap_window_t`.
- Extend the VST3 view bridge from Cocoa-only to attach `IPlugView` to the X11 window using the Linux/X11 platform id and to service the event pump/run loop the plugin needs.
- Change the sandbox editor capability gate from macOS-only to macOS-or-Linux-X11.
- Reuse the existing editor IPC messages (`editor open/close`, editor-open event, power event, resize polling) so `prowave-app` needs no new transport.
- If `DISPLAY` is absent or window creation fails, report no editor and let the app show sliders.

The window helper must expose enough data for both formats without leaking it into `prowave-app`: CLAP receives a display pointer and window id; VST3 receives the X11 window id as its parent handle.

### Files likely touched

- `crates/prowave-app/src/plugin_ui_prefs.rs` (new)
- `crates/prowave-app/src/main.rs`
- `crates/prowave-app/src/mixer.rs`
- `crates/prowave-app/src/fx_ui.rs`
- `crates/prowave-app/src/plugin_manager.rs`
- `crates/prowave-plugin-host/src/lib.rs`
- `crates/prowave-plugin-host/src/sandbox.rs`
- `crates/prowave-plugin-host/src/clap.rs`
- `crates/prowave-plugin-host/src/vst3.rs`
- `crates/prowave-plugin-host/src/x11.rs` (new)
- `crates/prowave-plugin-host/Cargo.toml`

### Tests

Deterministic:

- `PluginUiMode` defaults to `Sliders`.
- Preference save/load round-trips with test-safe config root.
- `MixerView::default()` uses the persisted preference.
- Pure view-selection tests cover: default sliders, explicit editor, explicit sliders, no editor, crashed plugin.
- Existing `fx_ui` egui harness remains green.
- `prowave-plugin-host` unit test: no `DISPLAY` means no editor capability.

Manual / ignored:

- X11 session with an installed CLAP plugin that has an editor: opt in, verify designed UI appears, power toggles bypass, close returns to Pro Wave, crash falls back to sliders.
- Same flow with an installed VST3 plugin that has an `IPlugView`.

### Verification

```bash
cargo test -p prowave-app plugin_ui -- --nocapture
cargo test -p prowave-plugin-host
cargo clippy -p prowave-app -p prowave-plugin-host --all-targets -- -D warnings
cargo fmt --check
git diff --check
```

## What is left to the builder

Implement the preference and pure selector first, then the shared X11 window path for CLAP and VST3. Keep embedding explicitly deferred. If either format's editor path proves unstable in the sandbox, report it rather than silently forcing sliders for that format.
