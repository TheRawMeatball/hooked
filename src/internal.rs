use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::{HashMap, HashSet},
    hash::Hash,
    rc::Rc,
};

use crossbeam_channel::{Receiver, Sender};

use crate::dom::{Dom, Primitive, PrimitiveId};
use crate::fctx::Memo;

use crate::fctx::Fctx;

pub(crate) type Tx = Sender<EffectResolver>;
pub(crate) type Rx = Receiver<EffectResolver>;

pub(crate) struct EffectResolver {
    pub(crate) f: Box<dyn FnOnce(&mut dyn Any) + Send>,
    pub(crate) target_component: MountedId,
    pub(crate) target_state: u64,
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub(crate) struct MountedId(pub u64);

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct MountedRootId(MountedId);

pub trait ComponentFunc<P, M>: 'static {
    fn e(&self, p: P) -> Element;
    fn memo_e(&self, p: P) -> Element
    where
        P: PartialEq;
    fn call(&self, p: &P, ctx: Fctx) -> ComponentOutput;
    fn fn_type_id(&self) -> TypeId;
    fn dyn_clone(&self) -> Box<dyn ComponentFunc<P, M>>;
}

trait DynComponentFunc {
    fn call(&self, p: &dyn Prop, ctx: Fctx) -> ComponentOutput;
    fn fn_type_id(&self) -> TypeId;
    fn dyn_clone(&self) -> Box<dyn DynComponentFunc>;
    fn use_memoized(&self, old: &dyn Prop, new: &dyn Prop) -> bool;
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
}

impl Component {
    fn update(
        &mut self,
        id: MountedId,
        children: &mut Vec<MountedId>,
        ctx: &mut Context,
        dom: &mut impl Dom,
    ) {
        let new_children = self.f.call(
            &*self.props,
            Fctx::update(
                ctx.tx.clone(),
                id,
                &mut self.state,
                &mut self.memos,
                &mut self.effects,
            ),
        );
        let mut new_children = new_children.into_iter();
        let mut remove_index = -1;
        for child in children.iter_mut() {
            if let Some(new_child) = new_children.next() {
                ctx.diff(child, new_child, dom);
                remove_index += 1;
            }
        }
        for child in children.drain((remove_index + 1) as usize..) {
            ctx.unmount(child, dom);
        }
        for remaining in new_children {
            children.push(ctx.mount(remaining, dom));
        }
        for effect in self.effects.iter_mut() {
            replace_with::replace_with_or_abort(effect, |effect| match effect.f {
                EffectStage::Effect(e) => Effect {
                    eq_cache: effect.eq_cache,
                    f: EffectStage::Destructor(e()),
                },
                EffectStage::Destructor(_) => effect,
            });
        }
    }
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
pub struct Element(ElementInner, Option<u64>);

impl Element {
    pub fn with_key(self, key: u64) -> Self {
        Self(self.0, Some(key))
    }
}

struct Mounted {
    inner: MountedInner,
    children: Vec<MountedId>,
}

enum MountedInner {
    Primitive(PrimitiveId),
    Component(Component),
}

impl MountedInner {
    fn as_component(&mut self) -> Option<&mut Component> {
        match self {
            MountedInner::Primitive(_) => None,
            MountedInner::Component(c) => Some(c),
        }
    }
}

pub struct Context {
    tree: HashMap<MountedId, Mounted>,
    counter: u64,
    tx: Tx,
    rx: Rx,
}

impl Context {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let tree = HashMap::new();
        Self {
            tree,
            tx,
            rx,
            counter: 0,
        }
    }
    pub fn mount_root(&mut self, e: Element, dom: &mut impl Dom) -> MountedRootId {
        MountedRootId(self.mount(e, dom))
    }
    pub fn unmount_root(&mut self, id: MountedRootId, dom: &mut impl Dom) {
        self.unmount(id.0, dom);
    }
    pub fn process_messages(&mut self, dom: &mut impl Dom) {
        dom.start();
        let mut roots = HashSet::new();
        let mut flagged = HashSet::new();
        while !self.rx.is_empty() {
            for resolver in self.rx.try_iter() {
                let component = self
                    .tree
                    .get_mut(&resolver.target_component)
                    .unwrap()
                    .inner
                    .as_component()
                    .unwrap();
                let rc = &mut component.state[resolver.target_state as usize];
                let state = Rc::get_mut(rc).unwrap();
                (resolver.f)(state);

                let id = resolver.target_component;
                if flagged.contains(&id) {
                    continue;
                }

                fn recursive(
                    element: MountedId,
                    roots: &mut HashSet<MountedId>,
                    flagged: &mut HashSet<MountedId>,
                    tree: &HashMap<MountedId, Mounted>,
                ) {
                    for cid in tree.get(&element).unwrap().children.iter() {
                        roots.remove(cid);
                        if !flagged.insert(*cid) {
                            continue;
                        }
                        recursive(*cid, roots, flagged, tree);
                    }
                }

                roots.insert(id);
                recursive(id, &mut roots, &mut flagged, &self.tree);
            }
            flagged.clear();
            for rerender_root in roots.drain() {
                let Mounted {
                    mut inner,
                    mut children,
                } = self.tree.remove(&rerender_root).unwrap();
                let c = inner.as_component().unwrap();
                c.update(rerender_root, &mut children, self, dom);
                self.tree.insert(rerender_root, Mounted { inner, children });
            }
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
                self.tree.insert(
                    mounted_id,
                    Mounted {
                        inner: MountedInner::Primitive(id),
                        children,
                    },
                );
                mounted_id
            }
            ElementInner::Component(c) => {
                let mut state = Vec::new();
                let mut memos = Vec::new();
                let mut effects = Vec::new();
                let mounted_id = MountedId(self.counter);
                self.counter += 1;
                let ro = c.f.call(
                    &*c.props,
                    Fctx::render_first(
                        self.tx.clone(),
                        mounted_id,
                        &mut state,
                        &mut memos,
                        &mut effects,
                    ),
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

                let component = Component {
                    f: c.f,
                    props: c.props,
                    state,
                    memos,
                    effects,
                };
                self.tree.insert(
                    mounted_id,
                    Mounted {
                        inner: MountedInner::Component(component),
                        children,
                    },
                );
                mounted_id
            }
        }
    }

    fn unmount(&mut self, this: MountedId, dom: &mut impl Dom) {
        let Mounted { inner, children } = self.tree.remove(&this).unwrap();
        for child in children {
            self.unmount(child, dom);
        }
        match inner {
            MountedInner::Primitive(id) => {
                dom.remove(id);
            }
            MountedInner::Component(c) => {
                for effect in c.effects.into_iter() {
                    match effect.f {
                        EffectStage::Destructor(d) => d(),
                        _ => {}
                    }
                }
            }
        }
    }

    fn diff(&mut self, id: &mut MountedId, other: Element, dom: &mut impl Dom) {
        let Mounted {
            inner,
            mut children,
        } = self.tree.remove(&id).unwrap();
        match (inner, other.0) {
            (MountedInner::Primitive(p_id), ElementInner::Primitive(new, new_children)) => {
                dom.diff_primitive(p_id, new);
                let mut dom = dom.get_sub_context(p_id);
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
                self.tree.insert(
                    *id,
                    Mounted {
                        inner: MountedInner::Primitive(p_id),
                        children,
                    },
                );
            }
            (MountedInner::Component(mut old), ElementInner::Component(new)) => {
                if old.f.fn_type_id() == new.f.fn_type_id() {
                    if !old.f.use_memoized(&*old.props, &*new.props) {
                        old.update(*id, &mut children, self, dom);
                    }
                    self.tree.insert(
                        *id,
                        Mounted {
                            inner: MountedInner::Component(old),
                            children,
                        },
                    );
                } else {
                    for child in children.drain(..) {
                        self.unmount(child, dom);
                    }
                    self.tree.insert(
                        *id,
                        Mounted {
                            inner: MountedInner::Component(old),
                            children,
                        },
                    );
                    self.unmount(*id, dom);
                    *id = self.mount(Element(ElementInner::Component(new), other.1), dom);
                }
            }
            (inner, new) => {
                self.tree.insert(*id, Mounted { inner, children });
                self.unmount(*id, dom);
                *id = self.mount(Element(new, other.1), dom);
            }
        }
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
                }), None)
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

            fn memo_e(&self, props: ($($ident,)*)) -> Element
            where
                ($($ident,)*): PartialEq
            {
                Element(ElementInner::Component(ComponentTemplate {
                    // Why must I have such horrible double-boxing :(
                    f: Box::new(MemoizableComponentFunc(
                        Box::new(*self) as Box<dyn ComponentFunc<($($ident,)*), Out>>
                    )),
                    props: Box::new(props),
                }), None)
            }
        }

        #[allow(non_snake_case)]
        impl<Func: Fn($($ident,)*) -> Element + 'static, $($ident,)*> ComponentFunc<($($ident,)*), ()> for Func {
            fn e(&self, ($($ident,)*): ($($ident,)*)) -> Element {
                self($($ident,)*)
            }
            fn memo_e(&self, ($($ident,)*): ($($ident,)*)) -> Element
            where
                ($($ident,)*): PartialEq {
                self($($ident,)*)
            }
            fn call(&self, _: &($($ident,)*), _: Fctx) -> ComponentOutput { unreachable!() }
            fn fn_type_id(&self) -> TypeId { unreachable!() }
            fn dyn_clone(&self) -> Box<dyn ComponentFunc<($($ident,)*), ()>> { unreachable!() }
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

    fn use_memoized(&self, _: &dyn Prop, _: &dyn Prop) -> bool {
        false
    }
}

struct MemoizableComponentFunc<P: PartialEq + Any, M>(Box<dyn ComponentFunc<P, M>>);

impl<P: PartialEq + Any, M: 'static> DynComponentFunc for MemoizableComponentFunc<P, M> {
    fn call(&self, p: &dyn Prop, ctx: Fctx) -> ComponentOutput {
        (&*self.0).call(p.as_any().downcast_ref().unwrap(), ctx)
    }
    fn fn_type_id(&self) -> TypeId {
        (&*self.0).fn_type_id()
    }

    fn dyn_clone(&self) -> Box<dyn DynComponentFunc> {
        Box::new((&*self.0).dyn_clone())
    }

    fn use_memoized(&self, old: &dyn Prop, new: &dyn Prop) -> bool {
        old.as_any()
            .downcast_ref::<P>()
            .zip(new.as_any().downcast_ref::<P>())
            .map(|(a, b)| a == b)
            .unwrap_or(false)
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
    Element(
        ElementInner::Primitive(Primitive::Panel, children.into()),
        None,
    )
}

pub fn text(text: impl Into<String>) -> Element {
    Element(
        ElementInner::Primitive(Primitive::Text(text.into()), vec![]),
        None,
    )
}
