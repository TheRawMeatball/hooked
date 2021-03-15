#[derive(Clone)]
pub enum Primitive {
    Panel,
    Text(String),
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub struct PrimitiveId(pub u64);

pub trait Dom {
    fn start(&mut self) {}
    fn mount(&mut self, primitive: Primitive) -> PrimitiveId {
        self.mount_as_child(primitive, None)
    }
    fn mount_as_child(&mut self, primitive: Primitive, parent: Option<PrimitiveId>) -> PrimitiveId;
    fn diff_primitive(&mut self, old: PrimitiveId, new: Primitive);
    fn get_sub_list(&mut self, id: PrimitiveId) -> (PrimitiveId, &mut dyn Dom);
    fn remove(&mut self, id: PrimitiveId);
    fn commit(&mut self) {}
}

impl<T: Dom + ?Sized> Dom for (PrimitiveId, &mut T) {
    fn mount(&mut self, primitive: Primitive) -> PrimitiveId {
        self.1.mount_as_child(primitive, Some(self.0))
    }

    fn diff_primitive(&mut self, old: PrimitiveId, new: Primitive) {
        self.1.diff_primitive(old, new)
    }

    fn get_sub_list(&mut self, id: PrimitiveId) -> (PrimitiveId, &mut dyn Dom) {
        (id, self)
    }

    fn remove(&mut self, id: PrimitiveId) {
        self.1.remove(id)
    }

    fn mount_as_child(&mut self, primitive: Primitive, parent: Option<PrimitiveId>) -> PrimitiveId {
        self.1.mount_as_child(primitive, parent)
    }

    fn start(&mut self) {
        panic!("Parent contexts shouldn't call start or commit!")
    }

    fn commit(&mut self) {
        panic!("Parent contexts shouldn't call start or commit!")
    }
}
