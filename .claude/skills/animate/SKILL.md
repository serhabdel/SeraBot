---
name: animate
description: 'Build one animation from scratch in any UI stack (Rust desktop with Freya, egui, gpui, Slint, Iced; web; mobile) making the decisions in the order that decides whether it feels right: should it animate at all, what purpose, which primitive, which properties, which curve and duration, how it interrupts, how it exits, how it behaves under reduced motion. Writes the implementation in the project''s idiom so it passes a strict motion review the first time. Use when the user asks to animate, add a transition, make something slide, fade, spring or feel smoother, or to implement a row from find-animation-opportunities or a plan from improve-animations. Usually invoked by the definitive-ux router; can be called directly.'
---

# Animate

> **Stack:** load `stack-map` first; the motion primitives, easing names and interruption model come from the framework's map. Rules come from `definitive-ux/references/motion-standards.md`. Adapted from Emil Kowalski's `animate` skill.

A construction skill. It does one thing: turn a request for motion into an implementation that survives review. It does not sweep a codebase (`find-animation-opportunities`), audit existing motion (`improve-animations`) or judge a diff (`better-interface` polish domain).

## Posture

Senior design engineer building it yourself. Two failure modes, the first worse:

1. Animating something that should not animate. The gate below sometimes produces zero lines of code. That is a success.
2. Animating the right thing with wrong ingredients: ease-in on an entrance, scale from zero, a restart-only timeline on a toggle, a duration that makes a menu feel sluggish, an animation that fires on first render.

Never present motion options as a menu. Make the call, state the reason in one line, write the code.

## Decisions, in order

1. **Gate.** Frequency tier and purpose from `motion-standards.md`. Keyboard-driven, 100+/day, data being read, TV/watch/embedded outside focus and feedback: stop and say why.
2. **Primitive.** From the map: Freya `use_animation` + `AnimNum`/`AnimColor`/`AnimSequential` or `use_animation_transition`; egui `animate_bool_with_time` / `animate_value_with_time`; gpui `with_animation` + `Animation`; Slint `animate <property> { duration; easing; }` and `states` + `transitions`; Iced `iced_anim` or a `Subscription`-driven tween; web CSS transition vs keyframes vs `@starting-style`. Extend the project's existing vocabulary (its duration constants, easing tokens, a `reduce_motion` setting); never add a parallel one.
3. **Properties.** Opacity, scale, offset, color, clip. Layout-driving size only when the toolkit makes it cheap or the element is small.
4. **Curve and duration.** From the tables. Enter ease-out, exit ease-out at 70% of the enter time, moves ease-in-out, continuous linear.
5. **Origin and direction.** Anchored at the trigger for popovers and menus; centered for modals; symmetric exits for toasts and sheets.
6. **Interruption.** Retarget from the current value: Freya `.reverse()`, egui per-frame target, Slint property animation (automatic), gpui restart with short duration and say so.
7. **First render.** Seed the final value on mount so existing state does not animate in.
8. **Reduced motion.** Read the project's setting; crossfade instead of slide or scale; never zero duration.
9. **Performance.** Request repaint only while running (egui `request_repaint_after`), no per-frame allocation, measure on the target device.

## Output

- One paragraph: what animates, why (purpose), tier, and the three numbers (duration, curve, properties).
- The code, in the project's idiom, in the file where the element lives, following `rust-engineering` for Rust.
- A reduced-motion branch.
- A one-line verification note: how it was checked (run, snapshot, or not verified).

If the gate says no: say so, name the tier, and offer the non-motion fix (instant state change with a clear visual delta).
