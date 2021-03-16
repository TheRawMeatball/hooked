use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
};

use crossbeam_channel::{Receiver, Sender};

use crate::dom::{Dom, Primitive, PrimitiveId};
use crate::{components::Components, fctx::Memo};

use crate::fctx::Fctx;

pub(crate) type Tx = Sender<EffectResolver>;
pub(crate) type Rx = Receiver<EffectResolver>;

pub(crate) struct EffectResolver {
    pub(crate) f: Box<dyn FnOnce(&mut dyn Any) + Send>,
    pub(crate) target_component: ComponentId,
    pub(crate) target_state: u64,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct ComponentId(pub u64);
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct MountedId(pub u64);

pub trait ComponentFunc<P, M>: 'static {
    fn e(&self, p: P) -> Element;
    fn call(&self, p: &P, ctx: Fctx) -> ComponentOutput;
    fn fn_type_id(&self) -> TypeId;
    fn dyn_clone(&self) -> Box<dyn ComponentFunc<P, M>>;
}

trait DynComponentFunc {
    fn call(&self, p: &dyn Prop, ctx: Fctx) -> ComponentOutput;
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
    state: Vec<Rc<dyn Any>>,
    memos: Vec<Rc<RefCell<Memo>>>,
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

#[derive(Clone, Copy)]
enum Mounted {
    Primitive(PrimitiveId),
    Component(ComponentId),
}

pub struct Context {
    root: MountedId,
    components: Components,
    tree: HashMap<MountedId, (Mounted, Vec<MountedId>)>,
    counter: u64,
    tx: Tx,
    rx: Rx,
}

impl Context {
    pub fn new(element: Element, dom: &mut impl Dom) -> Self {
        let components = Components::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let tree = HashMap::new();
        let mut ctx = Self {
            root: MountedId(0),
            components,
            tree,
            tx,
            rx,
            counter: 0,
        };
        ctx.root = ctx.mount(element, dom);
        ctx
    }
    pub fn process_messages(&mut self, dom: &mut impl Dom) {
        dom.start();
        while !self.rx.is_empty() {
            for resolver in self.rx.try_iter() {
                // TODO: cull unnecessary effects
                // TODO: start rerenders from the leaves
                let component = &mut self.components.get(resolver.target_component);
                let rc = &mut component.state[resolver.target_state as usize];
                let state = Rc::get_mut(rc).unwrap();
                (resolver.f)(state);
                component.dirty = true;
            }
            let mut root = self.root;
            self.rerender_flagged(&mut root, dom);
            self.root = root;
        }
        dom.commit();
    }

    pub fn msg_count(&self) -> usize {
        self.rx.len()
    }

    fn mount(&mut self, element: Element, dom: &mut impl Dom) -> MountedId {
        match element.0 {
            ElementInner::Primitive(p, c) => {
                let id = dom.mount(p);
                let mut child_ctx = dom.get_sub_context(id);
                let children = c
                    .into_iter()
                    .map(|v| self.mount(v, &mut child_ctx))
                    .collect();
                let mounted_id = MountedId(self.counter);
                self.counter += 1;
                self.tree
                    .insert(mounted_id, (Mounted::Primitive(id), children));
                mounted_id
            }
            ElementInner::Component(c) => {
                let id = self.components.allocate();
                let mut state = Vec::new();
                let mut memos = Vec::new();
                let mut effects = Vec::new();
                let ro = c.f.call(
                    &*c.props,
                    Fctx::render_first(self.tx.clone(), id, &mut state, &mut memos, &mut effects),
                );
                let children = ro.into_iter().map(|e| self.mount(e, dom)).collect();
                for effect in effects.iter_mut() {
                    replace_with::replace_with_or_abort(effect, |effect| match effect.f {
                        EffectStage::Effect(e) => Effect {
                            eq_cache: effect.eq_cache,
                            f: EffectStage::Destructor(e()),
                        },
                        EffectStage::Destructor(_) => effect,
                    });
                }
                self.components.insert(
                    id,
                    Component {
                        f: c.f,
                        props: c.props,
                        state,
                        memos,
                        effects,
                        dirty: false,
                    },
                );
                let mounted_id = MountedId(self.counter);
                self.counter += 1;
                self.tree
                    .insert(mounted_id, (Mounted::Component(id), children));
                mounted_id
            }
        }
    }

    fn unmount(&mut self, this: MountedId, dom: &mut impl Dom) {
        let (this, children) = self.tree.remove(&this).unwrap();
        for child in children {
            self.unmount(child, dom);
        }
        match this {
            Mounted::Primitive(id) => {
                dom.remove(id);
            }
            Mounted::Component(c) => {
                for effect in self.components.take(c).effects.into_iter() {
                    match effect.f {
                        EffectStage::Destructor(d) => d(),
                        _ => {}
                    }
                }
            }
        }
    }

    fn diff(&mut self, this_id: &mut MountedId, other: Element, dom: &mut impl Dom) {
        let (this, mut children) = self.tree.remove(&this_id).unwrap();
        match (this, other.0) {
            (Mounted::Primitive(id), ElementInner::Primitive(new, new_children)) => {
                dom.diff_primitive(id, new);
                let mut dom = dom.get_sub_context(id);
                let mut new_children = new_children.into_iter();
                for child in children.iter_mut() {
                    if let Some(new_child) = new_children.next() {
                        self.diff(child, new_child, &mut dom);
                    } else {
                        self.unmount(*child, &mut dom);
                    }
                }
                for remaining in new_children {
                    children.push(self.mount(remaining, &mut dom));
                }
                self.tree.insert(*this_id, (this, children));
            }
            (Mounted::Component(id), ElementInner::Component(new)) => {
                if self.components.get(id).f.fn_type_id() == new.f.fn_type_id() {
                    let mut old = self.components.take(id);
                    let new_children = old.f.call(
                        &*new.props,
                        Fctx::update(
                            self.tx.clone(),
                            id,
                            &mut old.state,
                            &mut old.memos,
                            &mut old.effects,
                        ),
                    );
                    let mut new_children = new_children.into_iter();
                    let mut remove_index = -1i32;
                    for child in children.iter_mut() {
                        if let Some(new_child) = new_children.next() {
                            self.diff(child, new_child, dom);
                            remove_index += 1;
                        }
                    }
                    for child in children.drain((remove_index + 1) as usize..) {
                        self.unmount(child, dom);
                    }
                    for remaining in new_children {
                        children.push(self.mount(remaining, dom));
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
                    self.components.insert(id, old);
                    self.tree.insert(*this_id, (this, children));
                } else {
                    for child in children.drain(..) {
                        self.unmount(child, dom);
                    }
                    self.tree.insert(*this_id, (this, children));
                    self.unmount(*this_id, dom);
                    *this_id = self.mount(Element(ElementInner::Component(new)), dom);
                }
            }
            (_, new) => {
                self.tree.insert(*this_id, (this, children));
                self.unmount(*this_id, dom);
                *this_id = self.mount(Element(new), dom);
            }
        }
    }

    fn rerender_flagged(&mut self, this_id: &mut MountedId, dom: &mut impl Dom) {
        let (this, mut children) = self.tree.remove(&this_id).unwrap();
        match this {
            Mounted::Component(id) => {
                let mut updated_children = false;
                if self.components.get(id).dirty {
                    let mut c = self.components.take(id);
                    updated_children = true;
                    let new_children = c.f.call(
                        &*c.props,
                        Fctx::update(
                            self.tx.clone(),
                            id,
                            &mut c.state,
                            &mut c.memos,
                            &mut c.effects,
                        ),
                    );
                    let mut new_children = new_children.into_iter();
                    let mut remove_index = -1isize;
                    for child in children.iter_mut() {
                        if let Some(new_child) = new_children.next() {
                            self.diff(child, new_child, dom);
                            remove_index += 1;
                        }
                    }
                    for child in children.drain((remove_index + 1) as usize..) {
                        self.unmount(child, dom);
                    }
                    for remaining in new_children {
                        children.push(self.mount(remaining, dom));
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
                    self.components.insert(id, c);
                }
                if !updated_children {
                    for child in children.iter_mut() {
                        self.rerender_flagged(child, dom);
                    }
                }
            }
            Mounted::Primitive(id) => {
                for child in children.iter_mut() {
                    let mut dom = dom.get_sub_context(id);
                    self.rerender_flagged(child, &mut dom);
                }
            }
        }
        self.tree.insert(*this_id, (this, children));
    }
}

macro_rules! impl_functions {
    ($($ident: ident),*) => {
        #[allow(non_snake_case)]
        impl<Func, Out, $($ident,)*> ComponentFunc<($($ident,)*), Out> for Func
        where
            $($ident: Any + Clone,)*
            Func: Fn(Fctx, $(&$ident,)*) -> Out + Copy + 'static,
            ComponentOutput: From<Out>,
            Out: 'static,
        {
            fn e(&self, props: ($($ident,)*)) -> Element {
                Element(ElementInner::Component(ComponentTemplate {
                    // Why must I have such horrible double-boxing :(
                    f: Box::new(Box::new(*self) as Box<dyn ComponentFunc<($($ident,)*), Out>>),
                    props: Box::new(props),
                }))
            }

            fn call(&self, ($($ident,)*): &($($ident,)*), ctx: Fctx) -> ComponentOutput {
                ComponentOutput::from(self(ctx, $($ident,)*))
            }

            fn fn_type_id(&self) -> TypeId {
                std::any::TypeId::of::<Func>()
            }

            fn dyn_clone(&self) -> Box<dyn ComponentFunc<($($ident,)*), Out>> {
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

impl<P: Any, M: 'static> DynComponentFunc for Box<dyn ComponentFunc<P, M>> {
    fn call(&self, p: &dyn Prop, ctx: Fctx) -> ComponentOutput {
        (&**self).call(p.as_any().downcast_ref().unwrap(), ctx)
    }
    fn fn_type_id(&self) -> TypeId {
        (&**self).fn_type_id()
    }

    fn dyn_clone(&self) -> Box<dyn DynComponentFunc> {
        Box::new((&**self).dyn_clone())
    }
}

pub enum ComponentOutput {
    None,
    Single(Element),
    Multiple(Vec<Element>),
}

impl IntoIterator for ComponentOutput {
    type Item = Element;

    type IntoIter = ComponentOutputIterator;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            ComponentOutput::None => ComponentOutputIterator::OptionIterator(None.into_iter()),
            ComponentOutput::Single(s) => {
                ComponentOutputIterator::OptionIterator(Some(s).into_iter())
            }
            ComponentOutput::Multiple(v) => {
                ComponentOutputIterator::MultipleIterator(v.into_iter())
            }
        }
    }
}

pub enum ComponentOutputIterator {
    OptionIterator(<Option<Element> as IntoIterator>::IntoIter),
    MultipleIterator(<Vec<Element> as IntoIterator>::IntoIter),
}

impl Iterator for ComponentOutputIterator {
    type Item = Element;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            ComponentOutputIterator::OptionIterator(v) => v.next(),
            ComponentOutputIterator::MultipleIterator(v) => v.next(),
        }
    }
}

impl From<Element> for ComponentOutput {
    fn from(v: Element) -> Self {
        Self::Single(v)
    }
}

impl From<Vec<Element>> for ComponentOutput {
    fn from(v: Vec<Element>) -> Self {
        Self::Multiple(v)
    }
}

impl From<Option<Element>> for ComponentOutput {
    fn from(v: Option<Element>) -> Self {
        v.map(|v| Self::Single(v)).unwrap_or(ComponentOutput::None)
    }
}
pub fn panel(children: impl Into<Vec<Element>>) -> Element {
    Element(ElementInner::Primitive(Primitive::Panel, children.into()))
}

pub fn text(text: impl Into<String>) -> Element {
    Element(ElementInner::Primitive(
        Primitive::Text(text.into()),
        vec![],
    ))
}
