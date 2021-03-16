use std::collections::HashMap;

use crate::internal::{Component, ComponentId};

pub(crate) struct Components {
    counter: u64,
    inner: HashMap<ComponentId, Component>,
}

impl Components {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
            counter: 0,
        }
    }

    pub fn get(&mut self, id: ComponentId) -> &mut Component {
        self.inner.get_mut(&id).unwrap()
    }

    pub fn take(&mut self, id: ComponentId) -> Component {
        self.inner.remove(&id).unwrap()
    }

    pub fn with<F: FnOnce(&mut Component, &mut Self)>(&mut self, id: ComponentId, f: F) {
        let mut val = self.take(id);
        f(&mut val, self);
        self.inner.insert(id, val);
    }

    pub fn allocate(&mut self) -> ComponentId {
        let id = ComponentId(self.counter);
        self.counter += 1;
        id
    }

    pub fn fill(&mut self, id: ComponentId, component: Component) {
        self.inner.insert(id, component);
    }
}
