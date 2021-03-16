use std::thread;

use crate::prelude::*;

pub fn counter(ctx: Fctx) -> Element {
    let (state, state_setter) = ctx.use_state(|| 0);
    ctx.use_effect(Some(()), || {
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            state_setter.set(|state| {
                *state += 1;
            });
            if rx.try_recv().is_ok() {
                break;
            }
        });
        move || tx.send(()).unwrap()
    });

    e::text(format!("{} seconds since creation!", &*state))
}
