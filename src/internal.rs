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
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct Key(pub u64);

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
        children: &mut Children,
        ctx: &mut Context,
        dom: &mut impl Dom,
        parent: Option<PrimitiveId>,
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
        ctx.diff_children(children, new_children, dom, parent);
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
pub struct Element(ElementInner, Option<Key>);

impl Element {
    pub fn with_key(self, key: Key) -> Self {
        Self(self.0, Some(key))
    }
}

struct Mounted {
    inner: MountedInner,
    children: Children,
    parent: Option<ParentPrimitiveData>,
}

#[derive(Clone, Copy)]
struct ParentPrimitiveData {
    id: PrimitiveId,
    cursor: u32,
}

struct Children {
    unkeyed: Vec<MountedId>,
    keyed: HashMap<Key, MountedId>,
}

impl<'a> IntoIterator for &'a Children {
    type Item = &'a MountedId;

    type IntoIter = std::iter::Chain<
        core::slice::Iter<'a, MountedId>,
        std::collections::hash_map::Values<'a, Key, MountedId>,
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.unkeyed.iter().chain(self.keyed.values())
    }
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
        MountedRootId(self.mount(e.0, dom, None))
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
                    for cid in &tree.get(&element).unwrap().children {
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
                    parent,
                } = self.tree.remove(&rerender_root).unwrap();
                let c = inner.as_component().unwrap();
                if let Some(data) = &parent {
                    let old_cursor = dom.get_cursor();
                    let mut child_ctx = dom.get_sub_context(data.id);
                    child_ctx.set_cursor(data.cursor);
                    c.update(rerender_root, &mut children, self, &mut child_ctx, Some(data.id));
                    dom.set_cursor(old_cursor);
                } else {
                    c.update(rerender_root, &mut children, self, dom, None);
                };
                self.tree.insert(
                    rerender_root,
                    Mounted {
                        inner,
                        children,
                        parent,
                    },
                );
            }
        }
        dom.commit();
    }

    pub fn msg_count(&self) -> usize {
        self.rx.len()
    }

    fn mount(
        &mut self,
        element: ElementInner,
        dom: &mut impl Dom,
        parent: Option<ParentPrimitiveData>,
    ) -> MountedId {
        match element {
            ElementInner::Primitive(p, c) => {
                let id = dom.mount(p);
                let old_cursor = dom.get_cursor();
                let mut child_ctx = dom.get_sub_context(id);
                let mut keyed = HashMap::new();
                let mut unkeyed = Vec::new();
                for element in c.into_iter() {
                    let data = ParentPrimitiveData {
                        id,
                        cursor: child_ctx.get_cursor(),
                    };
                    if let Some(key) = element.1 {
                        keyed.insert(key, self.mount(element.0, &mut child_ctx, Some(data)));
                    } else {
                        unkeyed.push(self.mount(element.0, &mut child_ctx, Some(data)));
                    }
                }
                dom.set_cursor(old_cursor);
                let mounted_id = MountedId(self.counter);
                self.counter += 1;
                self.tree.insert(
                    mounted_id,
                    Mounted {
                        inner: MountedInner::Primitive(id),
                        children: Children { keyed, unkeyed },
                        parent: parent.map(|data| ParentPrimitiveData {
                            id: data.id,
                            cursor: dom.get_cursor(),
                        }),
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
                let children = c.f.call(
                    &*c.props,
                    Fctx::render_first(
                        self.tx.clone(),
                        mounted_id,
                        &mut state,
                        &mut memos,
                        &mut effects,
                    ),
                );
                let mut keyed = HashMap::new();
                let mut unkeyed = Vec::new();
                for element in children.into_iter() {
                    let data = parent.map(|data| ParentPrimitiveData {
                        id: data.id,
                        cursor: dom.get_cursor(),
                    });
                    let mount_id = self.mount(element.0, dom, data);
                    if let Some(key) = element.1 {
                        keyed.insert(key, mount_id);
                    } else {
                        unkeyed.push(mount_id);
                    }
                }
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
                        children: Children { keyed, unkeyed },
                        parent,
                    },
                );
                mounted_id
            }
        }
    }

    fn unmount(&mut self, this: MountedId, dom: &mut impl Dom) {
        let Mounted {
            inner, children, ..
        } = self.tree.remove(&this).unwrap();
        for &child in &children {
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
            parent,
        } = self.tree.remove(&id).unwrap();
        match (inner, other.0) {
            (MountedInner::Primitive(p_id), ElementInner::Primitive(new, new_children)) => {
                dom.diff_primitive(p_id, new);
                let old_cursor = dom.get_cursor();
                let mut child_ctx = dom.get_sub_context(p_id);
                self.diff_children(
                    &mut children,
                    ComponentOutput::Multiple(new_children),
                    &mut child_ctx,
                    Some(p_id),
                );
                dom.set_cursor(old_cursor);
                self.tree.insert(
                    *id,
                    Mounted {
                        inner: MountedInner::Primitive(p_id),
                        children,
                        parent,
                    },
                );
            }
            (MountedInner::Component(mut old), ElementInner::Component(new)) => {
                if old.f.fn_type_id() == new.f.fn_type_id() {
                    if !old.f.use_memoized(&*old.props, &*new.props) {
                        old.update(*id, &mut children, self, dom, parent.map(|v| v.id));
                    }
                    self.tree.insert(
                        *id,
                        Mounted {
                            inner: MountedInner::Component(old),
                            children,
                            parent,
                        },
                    );
                } else {
                    for child in children.unkeyed.drain(..) {
                        self.unmount(child, dom);
                    }
                    self.tree.insert(
                        *id,
                        Mounted {
                            inner: MountedInner::Component(old),
                            children,
                            parent,
                        },
                    );
                    self.unmount(*id, dom);
                    *id = self.mount(ElementInner::Component(new), dom, parent);
                }
            }
            (inner, new) => {
                self.tree.insert(
                    *id,
                    Mounted {
                        inner,
                        children,
                        parent,
                    },
                );
                self.unmount(*id, dom);
                *id = self.mount(new, dom, parent);
            }
        }
    }

    fn diff_children(
        &mut self,
        old: &mut Children,
        new: ComponentOutput,
        dom: &mut impl Dom,
        parent: Option<PrimitiveId>,
    ) {
        let mut unkeyed = Vec::new();
        let mut keyed = HashMap::new();
        for element in new {
            let data = parent.map(|id| ParentPrimitiveData {
                id,
                cursor: dom.get_cursor(),
            });
            if let Some(key) = element.1 {
                if let Some(mut old_id) = old.keyed.remove(&key) {
                    self.diff(&mut old_id, element, dom);
                    keyed.insert(key, old_id);
                } else {
                    keyed.insert(key, self.mount(element.0, dom, data));
                }
            } else {
                if let Some(mut old_id) = old.unkeyed.pop() {
                    self.diff(&mut old_id, element, dom);
                    unkeyed.push(old_id);
                } else {
                    unkeyed.push(self.mount(element.0, dom, data));
                }
            }
        }
        for removed in std::mem::replace(&mut old.unkeyed, unkeyed)
            .into_iter()
            .chain(
                std::mem::replace(&mut old.keyed, keyed)
                    .into_iter()
                    .map(|(_, v)| v),
            )
        {
            self.unmount(removed, dom);
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
