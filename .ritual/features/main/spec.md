# Feature: Ritual TUI v0.1

## Goal
<!-- One paragraph: what should exist when this is done, and why. -->

## Behavior (the contract - WHAT, not HOW)
<!-- Inputs, outputs, invariants. Be concrete. -->

- **Panel focus is always reachable.** On every tab, the user can move focus between the left panel (pipeline) and the right panel; focus is never trapped on one side.
  - On the Live tab, even while the input field holds text or has focus, there is a way to move focus back to the left panel and to select items in the right panel — typing into the input must not permanently capture navigation.
  - On tabs other than Live, focus is not stuck on the left pipeline panel: the user can move to and interact with the right panel as well.

## Edge cases & failure modes
<!-- What must not break. What happens on bad input. -->

## Out of scope
<!-- Explicitly not part of this feature. -->
