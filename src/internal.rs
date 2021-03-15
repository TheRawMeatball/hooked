use std::any::{Any, TypeId};

use append_vec::AppendVec;
use crossbeam_channel::{Receiver, Sender};

use crate::components::Components;
use crate::dom::{Dom, Primitive, PrimitiveId};

use crate::Fctx;

pub(crate) type Tx = Sender<EffectResolver>;
pub(crate) type Rx = Receiver<EffectResolver>;

pub(crate) struct EffectResolver {
    pub(crate) f: Box<dyn FnOnce(&mut dyn Any) + Send>,
    pub(crate) target_component: ComponentId,
    pub(crate) target_state: u64,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct ComponentId(pub u64);

pub trait ComponentFunc<P>: 'static {
    fn e(&self, p: P) -> Element;
    fn call(&self, p: &P, ctx: Fctx) -> Vec<Element>;
    fn fn_type_id(&self) -> TypeId;
    fn dyn_clone(&self) -> Box<dyn ComponentFunc<P>>;
}

trait DynComponentFunc {
    fn call(&self, p: &dyn Prop, ctx: Fctx) -> Vec<Element>;
    fn fn_type_id(&self) -> TypeId;
    fn dyn_clone(&self) -> Box<dyn DynComponentFunc>;
}

pub(crate) struct Effect {
    pub(crate) eq_cache: Option<Box<dyn Any>>,
    pub(crate) f: EffectStage,
}

pub(crate) enum EffectStage {
    Effect(Box<dyn FnOnce() -> Box<dyn FnOnce()>>),
    Destructor(Box<dyn FnOnce()>),
}

pub(crate) struct Component {
    f: Box<dyn DynComponentFunc>,
    props: Box<dyn Prop>,
    state: AppendVec<dyn Any>,
    effects: Vec<Effect>,
    dirty: bool,
}

#[derive(Clone)]
struct ComponentTemplate {
    f: Box<dyn DynComponentFunc>,
    props: Box<dyn Prop>,
}

impl Clone for Box<dyn DynComponentFunc> {
    fn clone(&self) -> Self {
        (&**self).dyn_clone()
    }
}

trait Prop {
    fn dyn_clone(&self) -> Box<dyn Prop>;
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any + Clone> Prop for T {
    fn as_any(&self) -> &dyn Any {
        println!("{}", std::any::type_name::<Self>());
        self
    }

    fn dyn_clone(&self) -> Box<dyn Prop> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn Prop> {
    fn clone(&self) -> Self {
        (**self).dyn_clone()
    }
}

#[derive(Clone)]
enum ElementInner {
    Component(ComponentTemplate),
    Primitive(Primitive, Vec<Element>),
}

#[derive(Clone)]
pub struct Element(ElementInner);

enum Mounted {
    Primitive(PrimitiveId, Vec<Mounted>),
    Component(ComponentId, Vec<Mounted>),
}

pub struct Context {
    root: Mounted,
    components: Components,
    tx: Tx,
    rx: Rx,
}

impl Context {
    pub fn new(element: Element, dom: &mut impl Dom) -> Self {
        let mut components = Components::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            root: Mounted::mount(element, dom, &mut components, &tx),
            components,
            tx,
            rx,
        }
    }
    pub fn process_messages(&mut self, dom: &mut impl Dom) {
        dom.start();
        while !self.rx.is_empty() {
            for resolver in self.rx.try_iter() {
                // TODO: cull unnecessary effects
                // TODO: start rerenders from the leaves
                let component = &mut self.components.get(resolver.target_component);
                let state = &mut component.state.inner()[resolver.target_state as usize];
                (resolver.f)(&mut **state);
                component.dirty = true;
            }
            self.root
                .rerender_flagged(dom, &mut self.components, &self.tx);
        }
        dom.commit();
    }

    pub fn msg_count(&self) -> usize {
        self.rx.len()
    }
}

impl Mounted {
    fn mount(element: Element, dom: &mut impl Dom, components: &mut Components, tx: &Tx) -> Self {
        match element.0 {
            ElementInner::Primitive(p, c) => {
                let id = dom.mount(p);
                let mut child_ctx = dom.get_sub_list(id);
                let children = c
                    .into_iter()
                    .map(|v| Self::mount(v, &mut child_ctx, components, tx))
                    .collect();
                Self::Primitive(id, children)
            }
            ElementInner::Component(c) => {
                let id = components.allocate();
                let state = AppendVec::new();
                let mut effects = Vec::new();
                let ro = c.f.call(
                    &*c.props,
                    Fctx::render_first(tx.clone(), id, &state, &mut effects),
                );
                let children = ro
                    .into_iter()
                    .map(|e| Self::mount(e, dom, components, tx))
                    .collect();
                for effect in effects.iter_mut() {
                    replace_with::replace_with_or_abort(effect, |effect| match effect.f {
                        EffectStage::Effect(e) => Effect {
                            eq_cache: effect.eq_cache,
                            f: EffectStage::Destructor(e()),
                        },
                        EffectStage::Destructor(_) => effect,
                    });
                }
                components.fill(
                    id,
                    Component {
                        f: c.f,
                        props: c.props,
                        state,
                        effects,
                        dirty: false,
                    },
                );
                Self::Component(id, children)
            }
        }
    }

    fn diff(&mut self, other: Element, dom: &mut impl Dom, components: &mut Components, tx: &Tx) {
        let mut this = self;
        match (&mut this, other.0) {
            (Mounted::Primitive(id, children), ElementInner::Primitive(new, new_children)) => {
                dom.diff_primitive(*id, new);
                let mut remove_index = -1isize;
                let mut dom = dom.get_sub_list(*id);
                let mut new_children = new_children.into_iter();
                for (i, child) in children.iter_mut().enumerate() {
                    if let Some(new_child) = new_children.next() {
                        child.diff(new_child, &mut dom, components, tx);
                        remove_index = i as isize;
                    }
                }
                for child in children.drain((remove_index + 1) as usize..) {
                    child.unmount(&mut dom, components);
                }
                for remaining in new_children {
                    children.push(Self::mount(remaining, &mut dom, components, tx));
                }
            }
            (Mounted::Component(id, children), ElementInner::Component(new)) => {
                if components.get(*id).f.fn_type_id() == new.f.fn_type_id() {
                    components.with(*id, |old, components| {
                        let new_children = old.f.call(
                            &*new.props,
                            Fctx::update(tx.clone(), *id, &old.state, &mut old.effects),
                        );
                        let mut new_children = new_children.into_iter();
                        let mut remove_index = -1isize;
                        for child in children.iter_mut() {
                            if let Some(new_child) = new_children.next() {
                                child.diff(new_child, dom, components, tx);
                                remove_index += 1;
                            }
                        }
                        for child in children.drain((remove_index + 1) as usize..) {
                            child.unmount(dom, components);
                        }
                        for remaining in new_children {
                            children.push(Self::mount(remaining, dom, components, tx));
                        }
                        for effect in old.effects.iter_mut() {
                            replace_with::replace_with_or_abort(effect, |effect| match effect.f {
                                EffectStage::Effect(e) => Effect {
                                    eq_cache: effect.eq_cache,
                                    f: EffectStage::Destructor(e()),
                                },
                                EffectStage::Destructor(_) => effect,
                            });
                        }
                    });
                } else {
                    for child in children.drain(..) {
                        child.unmount(dom, components);
                    }
                    replace_with::replace_with_or_abort(this, |v| {
                        v.unmount(dom, components);
                        Self::mount(Element(ElementInner::Component(new)), dom, components, tx)
                    });
                }
            }
            (_, new) => {
                replace_with::replace_with_or_abort(this, |v| {
                    v.unmount(dom, components);
                    Self::mount(Element(new), dom, components, tx)
                });
            }
        }
    }

    fn rerender_flagged(&mut self, dom: &mut impl Dom, components: &mut Components, tx: &Tx) {
        match self {
            Mounted::Component(id, children) => {
                let mut updated_children = false;
                if components.get(*id).dirty {
                    components.with(*id, |c, components| {
                        updated_children = true;
                        let new_children = c.f.call(
                            &*c.props,
                            Fctx::update(tx.clone(), *id, &c.state, &mut c.effects),
                        );
                        let mut new_children = new_children.into_iter();
                        let mut remove_index = -1isize;
                        for child in children.iter_mut() {
                            if let Some(new_child) = new_children.next() {
                                child.diff(new_child, dom, components, tx);
                                remove_index += 1;
                            }
                        }
                        for child in children.drain((remove_index + 1) as usize..) {
                            child.unmount(dom, components);
                        }
                        for remaining in new_children {
                            children.push(Self::mount(remaining, dom, components, tx));
                        }
                        for effect in c.effects.iter_mut() {
                            replace_with::replace_with_or_abort(effect, |effect| match effect.f {
                                EffectStage::Effect(e) => Effect {
                                    eq_cache: effect.eq_cache,
                                    f: EffectStage::Destructor(e()),
                                },
                                EffectStage::Destructor(_) => effect,
                            });
                        }
                    });
                }
                if !updated_children {
                    for child in children.iter_mut() {
                        child.rerender_flagged(dom, components, tx);
                    }
                }
            }
            Mounted::Primitive(id, children) => {
                for child in children {
                    let mut dom = dom.get_sub_list(*id);
                    child.rerender_flagged(&mut dom, components, tx);
                }
            }
        }
    }

    fn unmount(self, dom: &mut impl Dom, components: &mut Components) {
        match self {
            Mounted::Primitive(id, children) => {
                for child in children.into_iter() {
                    child.unmount(dom, components)
                }
                dom.remove(id);
            }
            Mounted::Component(c, children) => {
                for child in children.into_iter() {
                    child.unmount(dom, components)
                }
                for effect in components.take(c).effects.into_iter() {
                    match effect.f {
                        EffectStage::Destructor(d) => d(),
                        _ => {}
                    }
                }
            }
        }
    }
}

macro_rules! impl_functions {
    ($($ident: ident),*) => {
        #[allow(non_snake_case)]
        impl<Func, $($ident,)*> ComponentFunc<($($ident,)*)> for Func
        where
            $($ident: Any + Clone,)*
            Func: Fn(Fctx, $(&$ident,)*) -> Vec<Element> + Copy + 'static,
        {
            fn e(&self, props: ($($ident,)*)) -> Element {
                Element(ElementInner::Component(ComponentTemplate {
                    // Why must I have such horrible double-boxing :(
                    f: Box::new(Box::new(*self) as Box<dyn ComponentFunc<($($ident,)*)>>),
                    props: Box::new(props),
                }))
            }

            fn call(&self, ($($ident,)*): &($($ident,)*), ctx: Fctx) -> Vec<Element> {
                self(ctx, $($ident,)*)
            }

            fn fn_type_id(&self) -> TypeId {
                std::any::TypeId::of::<Func>()
            }

            fn dyn_clone(&self) -> Box<dyn ComponentFunc<($($ident,)*)>> {
                Box::new(*self)
            }
        }
    };
}

impl_functions!();
impl_functions!(A);
impl_functions!(A, B);
impl_functions!(A, B, C);
impl_functions!(A, B, C, D);
impl_functions!(A, B, C, D, E);
impl_functions!(A, B, C, D, E, F);
impl_functions!(A, B, C, D, E, F, G);
impl_functions!(A, B, C, D, E, F, G, H);
impl_functions!(A, B, C, D, E, F, G, H, I);
impl_functions!(A, B, C, D, E, F, G, H, I, J);
impl_functions!(A, B, C, D, E, F, G, H, I, J, K);
impl_functions!(A, B, C, D, E, F, G, H, I, J, K, L);

impl<P: Any> DynComponentFunc for Box<dyn ComponentFunc<P>> {
    fn call(&self, p: &dyn Prop, ctx: Fctx) -> Vec<Element> {
        (&**self).call(p.as_any().downcast_ref().unwrap(), ctx)
    }
    fn fn_type_id(&self) -> TypeId {
        (&**self).fn_type_id()
    }

    fn dyn_clone(&self) -> Box<dyn DynComponentFunc> {
        Box::new((&**self).dyn_clone())
    }
}

pub fn text(_: Fctx, data: &String) -> Vec<Element> {
    vec![Element(ElementInner::Primitive(
        Primitive::Text(data.to_owned()),
        vec![],
    ))]
}

pub fn panel(_: Fctx, children: &Vec<Element>) -> Vec<Element> {
    vec![Element(ElementInner::Primitive(
        Primitive::Panel,
        children.clone(),
    ))]
}