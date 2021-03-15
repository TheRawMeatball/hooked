use std::{
    any::Any,
    cell::{Cell, RefCell},
    marker::PhantomData,
};

use append_vec::AppendVec;

use crate::internal::{ComponentId, Effect, EffectResolver, EffectStage, Tx};

pub struct Fctx<'a> {
    tx: Tx,
    id: ComponentId,
    states: &'a AppendVec<dyn Any>,
    effects: Option<RefCell<&'a mut Vec<Effect>>>,
    state_selector: Cell<usize>,
    effect_selector: Cell<usize>,
    init: bool,
}

impl<'a> Fctx<'a> {
    // Internal stuff
    pub(crate) fn render_first(
        tx: Tx,
        id: ComponentId,
        states: &'a AppendVec<dyn Any>,
        effects: &'a mut Vec<Effect>,
    ) -> Self {
        Self {
            tx,
            id,
            states,
            effects: Some(RefCell::new(effects)),
            state_selector: Cell::new(0),
            effect_selector: Cell::new(0),
            init: true,
        }
    }

    pub(crate) fn update(
        tx: Tx,
        id: ComponentId,
        states: &'a AppendVec<dyn Any>,
        effects: &'a mut Vec<Effect>,
    ) -> Self {
        Self {
            tx,
            id,
            states,
            effects: Some(RefCell::new(effects)),
            state_selector: Cell::new(0),
            effect_selector: Cell::new(0),
            init: false,
        }
    }

    // User facing hooks

    pub fn use_state<T: 'static, F: Fn() -> T>(&self, default: F) -> (&T, Setter<T>) {
        let state = if self.init {
            self.states.push(Box::new(default()));
            let state = self.states.last().unwrap();
            state.downcast_ref().unwrap()
        } else {
            let state = self.states.get(self.state_selector.get()).unwrap();
            state.downcast_ref().unwrap()
        };
        self.state_selector.set(self.state_selector.get() + 1);
        (
            state,
            Setter {
                tx: self.tx.clone(),
                component: self.id,
                state: self.state_selector.get() - 1,
                _m: PhantomData,
            },
        )
    }

    pub fn use_effect<F, D, X>(&self, eq_cache: Option<X>, f: F)
    where
        F: FnOnce() -> D + Send + Sync + 'static,
        D: FnOnce() + Send + Sync + 'static,
        X: PartialEq + 'static,
    {
        if let Some(effects) = &self.effects {
            let mut effects = effects.borrow_mut();
            if self.init {
                effects.push(Effect {
                    eq_cache: eq_cache.map(|x| Box::new(x) as Box<dyn Any>),
                    f: EffectStage::Effect(Box::new(move || Box::new(f()))),
                });
            } else {
                if effects
                    .get(self.effect_selector.get())
                    .and_then(|v| v.eq_cache.as_ref())
                    .and_then(|v| (&**v).downcast_ref::<X>())
                    .zip(eq_cache.as_ref())
                    .map(|(v, f)| v != f)
                    .unwrap_or(true)
                {
                    let old = effects.get_mut(self.effect_selector.get()).unwrap();
                    replace_with::replace_with_or_abort(old, move |v| {
                        match v.f {
                            EffectStage::Effect(_) => {}
                            EffectStage::Destructor(d) => {
                                d();
                            }
                        }
                        Effect {
                            eq_cache: eq_cache.map(|x| Box::new(x) as Box<dyn Any>),
                            f: EffectStage::Effect(Box::new(move || Box::new(f()))),
                        }
                    });
                }
                self.state_selector.set(self.state_selector.get() + 1);
            }
        }
    }
}

pub struct Setter<T> {
    tx: Tx,
    component: ComponentId,
    state: usize,
    _m: PhantomData<fn() -> T>,
}

impl<T: 'static> Setter<T> {
    pub fn set<F: FnOnce(&mut T) + Send + Sync + 'static>(&self, f: F) {
        let id = self.component;
        let state = self.state;
        self.tx
            .send(EffectResolver {
                f: Box::new(|c| f(c.downcast_mut().unwrap())),
                target_component: id,
                target_state: state as u64,
            })
            .unwrap();
    }
}
