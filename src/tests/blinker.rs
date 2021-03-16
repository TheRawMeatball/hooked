use std::thread;

use crate::prelude::*;

pub fn blinker(ctx: Fctx, period: &u64) -> Element {
    let (is_on, set_is_on) = ctx.use_state(|| false);
    let period = *period;
    ctx.use_effect(Some(period), move || {
        let (tx, rx) = crossbeam_channel::bounded(1);
        thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(period));
            set_is_on.set(|state| {
                *state = !*state;
            });
            if rx.try_recv().is_ok() {
                break;
            }
        });
        move || tx.send(()).unwrap()
    });

    if *is_on {
        e::text(format!("Yay! - Period = {}", period))
    } else {
        e::text(format!("Nay! - Period = {}", period))
    }
}
