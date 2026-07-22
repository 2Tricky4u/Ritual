# Panel focus is always reachable (Ritual TUI v0.1)

## Context

The TUI has no explicit focus model. "Focus" is implicit per-tab inside `nav()` (`src/ui/app.rs:1538`): j/k reaches the left pipeline sidebar only on the Live tab while the stream is empty; on every other tab (and Live-with-stream) j/k drives the right panel and the sidebar cursor is unreachable except through the `i` stage-detail overlay. Meanwhile the sidebar selection highlight is always painted bright (`src/ui/dashboard.rs:352-376`), so it *looks* focused when it isn't. Worst trap: the spec/plan chat (`App.chat`, opened with `s`) replaces the right panel on any tab and its input captures every key (`chat_input`, `src/ui/app.rs:1963-2069`) — the only exit is Esc, which destroys the draft and is refused while an edit is in flight.

The spec (`.ritual/features/main/spec.md`) requires: on every tab, focus can move between the left pipeline panel and the right panel; the chat input must not permanently capture navigation; focus is never trapped.

User decisions (asked and answered):
- **Chat escape**: a modifier chord unfocuses the input; **Esc keeps its current close-the-chat meaning** (two-stage Esc rejected).
- **Focus keys**: `h`/`←` → focus-left, `l`/`→` → focus-right (all four currently unbound).

## Design

One derived predicate `App::pipeline_focused()` is the single source of truth consumed by `nav`, `ScrollTop`/`Follow`, `on_enter`, `draw_sidebar`, and `whichkey_sections`, so behavior, rendering, and help can never disagree.

### Focus model
- `enum PanelFocus { Pipeline, Main }` + `App.focus: PanelFocus`, default `Main`, persistent across tab switches. `Main` default + persistence preserves every current per-tab behavior byte-for-byte.
- `pipeline_focused()`: chat open → `!chat.input_focused && !sidebar_hidden()` (unfocused chat = sidebar mode only while the sidebar is actually rendered); otherwise `focus == Pipeline && !sidebar_hidden()`.
- The Live greeter (`stream.is_empty()`) keeps its existing "j/k moves pipeline" behavior as the fallback arm inside Main focus — startup is unchanged and greeter snapshots must not change.
- **Hidden sidebar** (terminal < 70 cols, or < 100 with chat): `ViewMax` (`src/ui/app.rs:383-388`) gains the last drawn terminal width (`term_width: Cell<u16>`), written by `dashboard::draw()` — the existing sanctioned renderer→input channel. One shared pure fn `sidebar_hidden(term_width, chat_open) -> bool` holds the threshold: `draw()` uses it for layout, `App::sidebar_hidden()` evaluates it at event time. The chat open/close threshold flip (70↔100) is therefore synchronous — only a real terminal resize is one frame stale, the same contract the scroll extents already have. `pipeline_focused()` checks it, so focus can never land on an invisible panel; `FocusLeft` then refuses with a status message. **Narrow-layout contract (spec edge case): when the sidebar is not rendered there is no left panel to focus — the refusal message IS the behavior; the user is still never trapped (unfocused chat scrolls the transcript; Esc/q close when idle).** Update the `ViewMax` doc comment ("viewport extents + terminal width").

### Keymap (route: architecture §Extension seams "New TUI input/feature")
- `Action::FocusLeft` / `Action::FocusRight` in `src/keymap.rs:12-58`.
- ACTIONS rows (`src/keymap.rs:61-141`): `("focus-left", …, "focus: pipeline sidebar")`, `("focus-right", …, "focus: main panel")` — palette entries come free.
- Defaults (`src/keymap.rs:262-302`): `h`, `left`, `alt+h`, `alt+left` → `focus-left`; `l`, `right`, `alt+l`, `alt+right` → `focus-right`. Multiple chords per action is existing precedent (`q`/`ctrl+c` → quit). The alt variants exist so the same action is reachable from inside the chat text surface. (`parse_chord` already accepts `alt+` — `src/keymap.rs:220`.)
- `[keys]` user overrides work automatically — but an override is a REBIND that strips the action's defaults (`with_overrides`, `src/keymap.rs:317-332`), so the chat escape must not depend on them: see the reserved emergency chords below.

### Focus-aware navigation (`src/ui/app.rs`)
- `dispatch`: `FocusLeft` → refuse with status msg if `sidebar_hidden`, else `focus = Pipeline`; `FocusRight` → `focus = Main`.
- `nav()` (`:1538`): prepend guard — `pipeline_focused()` → move `self.selected` (rem_euclid over `PIPELINE`), return. Per-tab match below stays untouched, including the Live-empty greeter arm.
- `ScrollTop`/`Follow` (`:1446-1463`): pipeline-focused → `selected = 0` / `PIPELINE.len()-1`; else existing arms.
- `on_enter` (`:1583`): Findings detail only when `!pipeline_focused()`; everything else falls through to the existing stage launch (Enter-runs-selected-stage from anywhere is existing advertised behavior; keep it).
- `spawn_headless` (`:~1800`, already forces `Tab::Live` + clears stream): also set `focus = Main` so j/k follows the fresh stream even if the user launched from the sidebar.

### Chat: chord out, sidebar mode, Esc unchanged
- `ChatState.input_focused: bool`, `true` in `open_chat` (`:1865`).
- `chat_input` (`:1963`): for alt/ctrl-modified keys that don't already have a chat meaning, resolve through the keymap — `FocusLeft` → `input_focused = false` (draft and cursor kept; allowed while in flight, it closes nothing). **Reserved emergency escapes**: the default chords `alt+←`/`alt+h` unfocus the input regardless of `[keys]` overrides (same class as the hardcoded `ctrl+x` cancel) — a rebind like `focus-left = "h"` must never re-trap the chat. Only modifier chords are resolved, so plain `h`/`l` still type. Esc behavior is **unchanged** (close when idle, refused in flight with the ctrl+x hint).
- New `chat_unfocused_input` (modeled on `stage_detail_input`, `:2905-2928`), routed from `on_input` (`:1283`) when chat is open and `!input_focused`. It resolves through the keymap first and matches on `Action` — raw key handling is limited to the invariant emergency controls (Esc, `ctrl+x`) — so user rebinds keep working:
  - `Up`/`Down` → move `self.selected` directly; if `sidebar_hidden`, scroll the chat transcript instead (never drive an invisible panel).
  - `FocusRight` (`l`/`→`) or `SpecChat` (`s`) → `input_focused = true`.
  - `Esc`/`q` → close chat if `!in_flight`, else the existing refusal message. `ctrl+x` → `chat_cancel()`.
  - `StageDetail` (`i`) → overlay (already draws above chat); `Help` (`?`) → help.
  - Everything else swallowed — no tab switches, no `Confirm`/stage launches, no palette (the chat hides the tabufline and right panel; invisible state changes are forbidden).
- `on_paste` (`:2076`): insert into the chat draft only when `input_focused`; when unfocused, discard with a status message ("paste ignored — press l to edit the chat input") so clipboard data is never silently lost.
- Closing the chat leaves `App.focus` untouched: if it was `Pipeline` before opening, the sidebar highlight visibly brightens again on close — a defined, tested transition, no hidden restoration bookkeeping.
- In-flight guards untouched: Esc-close blocked, Tab retarget frozen, Enter queues — all live inside `chat_input`, reachable only when focused.

### Rendering (`src/ui/dashboard.rs`, pure render)
- `draw()` writes `view_max.term_width`; the sidebar-drop condition (`:216-234`) moves into the shared `sidebar_hidden()` fn so renderer and input can never disagree.
- `draw_sidebar` (`:352-376`): bright selection row (`on_accent`/`bg_selection`) only when `pipeline_focused()` or the greeter fallback is active (Live + empty stream + no chat); otherwise a dimmed selection via existing semantic accessors. Theme colors only via `theme.rs` names.
- `draw_chat_panel` (`:678-813`): unfocused input renders dimmed and caretless; footer gains states — focused: advertise `alt+←/alt+h sidebar` (alt+h is the ESC-prefixed chord that survives terminals with broken CSI-modifier arrow encoding); unfocused: `j/k pipeline · l edit input · i stage · ctrl+x cancel · esc close`.
- `whichkey_sections` (`:~1926-2035`): main "move" section gains focus-left/focus-right; new chat-unfocused context section. Focused chat never reaches help (`?` types).

## Tests (implementation goes through /tdd — this is the derivable red-first list)

Unit, `src/keymap.rs`: `focus_actions_bound_and_in_palette` (ALL 8 default chords asserted individually — h/←/alt+h/alt+← → FocusLeft, l/→/alt+l/alt+→ → FocusRight; describe + palette entries); `focus_chords_match_real_key_events` (exact crossterm `KeyEvent` modifier combos incl. SHIFT-folding normalization; unmatched multi-modifier combos fall through).

Unit, `src/ui/app.rs` (idioms: `test_app()`, `dispatch(Action, &tx)`, `on_input(Event::Key(..), &tx)`; chat-routing tests must go through `on_input`):
- `focus_left_makes_jk_move_pipeline_on_every_tab` (Findings/History/Plan/Guide/Live-with-stream: `selected` moves, tab buffer frozen)
- `focus_right_returns_jk_to_the_tab_panel`; `focus_persists_across_tab_switches`
- `g_and_G_jump_the_pipeline_cursor_when_focused`
- `enter_on_findings_is_focus_aware` (Main → detail; Pipeline → the SAME guarded launch path — assert an existing guard fires, e.g. refusal while a run is active, not merely "no detail opened")
- `spawning_a_headless_run_resets_focus_to_main`
- `focus_left_refuses_when_sidebar_hidden`
- `open_chat_then_immediate_focus_left_at_80_cols` (the 70↔100 threshold flip is synchronous — no stale-frame trap)
- `closing_chat_keeps_prior_focus` (Pipeline before open → highlight brightens again on close)
- `chat_escape_survives_adversarial_override` (`[keys] focus-left = "h"`: plain h still types; alt+←/alt+h still unfocus)
- `chat_unfocused_honors_rebound_actions` (non-default bindings for FocusRight/SpecChat still refocus)
- `alt_left_unfocuses_chat_keeping_draft` (also while in flight; Esc semantics unchanged)
- `chat_unfocused_jk_moves_pipeline_and_l_refocuses` (and `l`/`s` don't type into the draft)
- `chat_unfocused_swallows_tab_switch_run_and_typing`
- `chat_esc_still_closes_when_idle_and_refuses_in_flight` (from both focused and unfocused)
- `paste_ignored_while_chat_unfocused` (discarded + status message emitted, no other state mutated); `stage_detail_opens_over_unfocused_chat`

Snapshots, `tests/ui_snapshots.rs`:
- Honesty test (`:357-459`): "move" count 6→8; add the chat-unfocused context assertion; extend the no-phantom loop.
- New: `dashboard_chat_unfocused_sidebar_mode` (setup_chat_app + `input_focused=false`, ≥100 cols); `dashboard_findings_pipeline_focused`.
- Boundary widths: 69 and 70 cols (no chat), 99 and 100 cols (chat open), each in both focus states where meaningful — the hidden/visible flip and its focus cue are snapshot-pinned at the exact thresholds.
- Panic-fuzz matrix (`:1015-1044`): add chat-unfocused and `focus=Pipeline` states.

Known breakage to fix alongside: `ChatState` literal constructors gain `input_focused: true` (`tests/ui_snapshots.rs:49`, `app.rs:6204`, `:6574`); help-overlay and spec-chat snapshots (footer/move rows); all non-Live-tab snapshots (sidebar selection now dimmed under Main focus — that change is the point). Live-greeter snapshots must NOT change (self-check). `esc_and_tab_are_frozen_while_a_chat_edit_streams` and `chat_tab_cycles_targets_and_esc_closes` keep passing (Esc semantics unchanged) — only re-verify.

## Execution order

1. `src/keymap.rs`: actions, ACTIONS rows, default chords.
2. `src/ui/app.rs` state: `PanelFocus`, `App.focus`, `ViewMax.term_width`, shared `sidebar_hidden(term_width, chat_open)`, `ChatState.input_focused`, `pipeline_focused()`.
3. `src/ui/app.rs` behavior: dispatch arms, `nav`/`ScrollTop`/`Follow` guards, `on_enter`, `spawn_headless`.
4. `src/ui/app.rs` chat: chord-out in `chat_input`, `chat_unfocused_input`, `on_input` routing, `on_paste` guard.
5. `src/ui/dashboard.rs`: `term_width` write + shared `sidebar_hidden()` adoption, focus-aware sidebar highlight, chat input/footer states, which-key sections; update `//!` contract lines (`app.rs` ViewMax doc, `dashboard.rs` header).
6. Tests red→green per /tdd; `cargo insta review` for snapshot churn; `./check.sh` full.

## Verification

- `./check.sh` (fmt + clippy -D warnings + full cargo test incl. insta) green.
- Manual smoke: `cargo run` in a scratch project — on each tab press `h`, verify j/k moves the sidebar and the highlight brightens; `l` returns; open chat with `s`, type a draft, `alt+←`, verify j/k moves the pipeline with the draft intact, `l` refocuses, Esc still closes only when idle.
- Terminal matrix: repeat the chat-escape smoke (alt+← AND alt+h) in a plain local terminal, under tmux, and over SSH — alt-arrow CSI-modifier encoding varies; alt+h (plain ESC-prefixed) is the fallback that survives. Known residual risk, documented: an exotic terminal may deliver alt+← as bare Esc then Left, closing an idle chat and losing the draft — alt+h is the advertised robust alternative.
- Live-greeter snapshots unchanged (`git diff tests/snapshots/` shows no greeter churn).

## Deliverables

- [x] D1: `focus-left`/`focus-right` actions with defaults (`h`/`←`/`alt+h`/`alt+←`, `l`/`→`/`alt+l`/`alt+→`) and palette entries - accept: keymap unit test resolves ALL 8 default chords individually to the right Action (incl. real-KeyEvent modifier combos) and finds both palette entries - route: src/keymap.rs
- [x] D2: explicit focus state: `PanelFocus` + `App.focus` + `ChatState.input_focused` + `ViewMax.term_width` + shared `sidebar_hidden(term_width, chat_open)` + `pipeline_focused()` - accept: `focus_persists_across_tab_switches`, `focus_left_refuses_when_sidebar_hidden`, `open_chat_then_immediate_focus_left_at_80_cols`, and `closing_chat_keeps_prior_focus` unit tests pass - route: src/ui/app.rs
- [x] D3: focus-aware `nav`/`ScrollTop`/`Follow` - accept: with Pipeline focus, j/k/g/G move `App.selected` on every tab while the tab's own buffer stays frozen (unit test) - route: src/ui/app.rs §nav/dispatch
- [x] D4: focus-aware `on_enter` on Findings - accept: Main focus → finding detail opens; Pipeline focus → the same guarded stage-launch path, with an existing guard (e.g. active-run refusal) asserted to fire (unit test) - route: src/ui/app.rs §on_enter
- [x] D5: chat unfocus chord + sidebar mode - accept: `alt+←` from the focused input keeps the draft and enables j/k pipeline movement even in flight; emergency escapes `alt+←`/`alt+h` survive an adversarial `[keys]` rebind (`focus-left = "h"`); `l`/`s` refocus without typing; unfocused handler resolves rebound Actions; Tab/Enter/plain chars swallowed while unfocused; Esc close semantics unchanged (unit tests via `on_input`) - route: src/ui/app.rs §chat_input/chat_unfocused_input
- [x] D6: paste guarded by input focus - accept: `paste_ignored_while_chat_unfocused` passes — paste discarded with a status message, no other state mutated - route: src/ui/app.rs §on_paste
- [x] D7: headless spawn resets focus to Main - accept: `spawning_a_headless_run_resets_focus_to_main` unit test passes - route: src/ui/app.rs §spawn_headless
- [x] D8: focus-visible rendering: dimmed sidebar selection when unfocused, greeter fallback stays bright, chat input dim/caretless + footer states, `term_width` written by `draw()` - accept: new snapshots `dashboard_chat_unfocused_sidebar_mode` + `dashboard_findings_pipeline_focused` + the 69/70 and 99/100-col boundary snapshots approved; Live-greeter snapshots byte-identical - route: src/ui/dashboard.rs
- [x] D9: which-key/help honesty: move section + chat-unfocused context advertised - accept: updated `whichkey_sections_advertise_exactly_what_works` passes with move=8 and the new context asserted - route: tests/ui_snapshots.rs §honesty test + src/ui/dashboard.rs §whichkey_sections
- [x] D10: module contracts current - accept: `ViewMax` doc mentions sidebar visibility; dashboard `//!` header mentions the focus cue; grep confirms - route: src/ui/app.rs + src/ui/dashboard.rs doc comments
- [ ] D11: full gate green - accept: `./check.sh` exits 0 (fmt, clippy -D warnings, all tests incl. insta) - route: check.sh
