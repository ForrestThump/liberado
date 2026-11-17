//! Signal writes that resume after an `.await`.
//!
//! `Signal::set`, `write`, and `with_mut` unwrap the generational box. A
//! `spawn_local` future or a timer callback that resumes after the owning
//! component unmounts then panics with `ValueDroppedError`. These helpers use
//! `try_write` and leave the value alone when the box is gone.

use dioxus::prelude::*;

/// Whether a late write changed the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyOutcome {
    Applied,
    /// The generational box was dropped, or a write borrow was already held.
    Skipped,
}

/// Run `apply` only when `slot` is still a live value.
pub(crate) fn apply_if_alive<T, E>(slot: Result<T, E>, apply: impl FnOnce(T)) -> ApplyOutcome {
    match slot {
        Ok(value) => {
            apply(value);
            ApplyOutcome::Applied
        }
        Err(_) => ApplyOutcome::Skipped,
    }
}

/// Replace `signal` when the owning component is still mounted.
pub(crate) fn set_if_alive<T: 'static>(signal: &mut Signal<T>, value: T) -> ApplyOutcome {
    apply_if_alive(signal.try_write(), |mut slot| *slot = value)
}

/// Mutate `signal` when the owning component is still mounted.
pub(crate) fn with_mut_if_alive<T: 'static>(
    signal: &mut Signal<T>,
    apply: impl FnOnce(&mut T),
) -> ApplyOutcome {
    apply_if_alive(signal.try_write(), |mut slot| apply(&mut *slot))
}

/// Push `item` when the owning component is still mounted.
pub(crate) fn push_if_alive<T: 'static>(signal: &mut Signal<Vec<T>>, item: T) -> ApplyOutcome {
    with_mut_if_alive(signal, |list| list.push(item))
}

/// Add one when the owning component is still mounted.
pub(crate) fn bump_if_alive<T>(signal: &mut Signal<T>) -> ApplyOutcome
where
    T: Copy + std::ops::AddAssign<T> + From<u8> + 'static,
{
    with_mut_if_alive(signal, |value| *value += T::from(1u8))
}

/// Call an existing method when the owning component is still mounted.
pub(crate) fn call_if_alive<T: 'static>(signal: &mut Signal<T>, apply: fn(&mut T)) -> ApplyOutcome {
    with_mut_if_alive(signal, apply)
}

/// Restart `resource` when its value is still mounted.
///
/// `Resource::restart` writes an inner signal. A dropped resource panics there.
pub(crate) fn restart_if_alive<T: 'static>(resource: &mut Resource<T>) -> ApplyOutcome {
    if resource.try_peek().is_err() {
        return ApplyOutcome::Skipped;
    }
    resource.restart();
    ApplyOutcome::Applied
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use dioxus::prelude::*;

    use super::*;

    thread_local! {
        static HELD: RefCell<Option<Signal<i32>>> = const { RefCell::new(None) };
        static HELD_ROWS: RefCell<Option<Signal<Vec<i32>>>> = const { RefCell::new(None) };
    }

    #[test]
    fn a_live_slot_receives_the_value() {
        let mut slot = 0;
        let outcome = apply_if_alive(Ok::<&mut i32, &str>(&mut slot), |place| *place = 4);
        assert_eq!(outcome, ApplyOutcome::Applied);
        assert_eq!(slot, 4);
    }

    #[test]
    fn a_dropped_slot_is_left_alone() {
        let mut slot = 0;
        let outcome = apply_if_alive(Err("Dropped (ValueDroppedError)"), |_: &mut i32| {
            slot = 9;
        });
        assert_eq!(outcome, ApplyOutcome::Skipped);
        assert_eq!(slot, 0);
    }

    fn set_eleven(value: &mut i32) {
        *value = 11;
    }

    #[component]
    fn Keeper() -> Element {
        let signal = use_signal(|| 1);
        let rows = use_signal(Vec::<i32>::new);
        HELD.with(|cell| *cell.borrow_mut() = Some(signal));
        HELD_ROWS.with(|cell| *cell.borrow_mut() = Some(rows));
        rsx! { "{signal}" }
    }

    #[test]
    fn set_if_alive_updates_a_mounted_signal_and_skips_a_dropped_one() {
        let mut dom = dioxus_core::VirtualDom::new(|| {
            let generation = dioxus::core::generation();
            if generation.is_multiple_of(2) {
                rsx! { Keeper {} }
            } else {
                rsx! { "" }
            }
        });
        dom.rebuild_in_place();
        let mut signal = HELD
            .with(|cell| cell.borrow_mut().take())
            .expect("mounted signal");
        let mut rows = HELD_ROWS
            .with(|cell| cell.borrow_mut().take())
            .expect("mounted rows");
        assert_eq!(set_if_alive(&mut signal, 7), ApplyOutcome::Applied);
        assert_eq!(*signal.peek(), 7);
        assert_eq!(bump_if_alive(&mut signal), ApplyOutcome::Applied);
        assert_eq!(*signal.peek(), 8);
        assert_eq!(
            call_if_alive(&mut signal, set_eleven),
            ApplyOutcome::Applied
        );
        assert_eq!(*signal.peek(), 11);
        assert_eq!(
            with_mut_if_alive(&mut signal, |value| *value += 1),
            ApplyOutcome::Applied
        );
        assert_eq!(*signal.peek(), 12);
        assert_eq!(push_if_alive(&mut rows, 4), ApplyOutcome::Applied);
        assert_eq!(rows.peek().as_slice(), &[4]);

        dom.mark_dirty(dioxus_core::ScopeId::APP);
        dom.render_immediate(&mut dioxus_core::NoOpMutations);

        assert_eq!(set_if_alive(&mut signal, 9), ApplyOutcome::Skipped);
        assert_eq!(bump_if_alive(&mut signal), ApplyOutcome::Skipped);
        assert_eq!(push_if_alive(&mut rows, 5), ApplyOutcome::Skipped);
        assert_eq!(
            call_if_alive(&mut signal, set_eleven),
            ApplyOutcome::Skipped
        );
        assert_eq!(
            with_mut_if_alive(&mut signal, |value| *value = 3),
            ApplyOutcome::Skipped
        );
    }
}
