mod blinker;
mod counter;

use crate::dom::{Dom, Primitive, PrimitiveId};

use crate::prelude::*;

use std::{collections::HashMap, fmt::Display};

use blinker::*;
use counter::*;

#[derive(Default)]
struct DemoDom {
    counter: u64,
    roots: Vec<PrimitiveId>,
    dom: HashMap<PrimitiveId, (Primitive, Vec<PrimitiveId>)>,
    cursor: u32,
}

impl Display for DemoDom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "The Dom")?;
        writeln!(f, "=======")?;
        fn recursor(
            f: &mut std::fmt::Formatter<'_>,
            element: PrimitiveId,
            nest_level: i32,
            dom: &HashMap<PrimitiveId, (Primitive, Vec<PrimitiveId>)>,
        ) -> std::fmt::Result {
            let (primitive, children) = if let Some(v) = dom.get(&element) {
                v
            } else {
                return Ok(());
            };
            for _ in 0..=nest_level {
                write!(f, "|>")?;
            }
            match primitive {
                Primitive::Text(text) => writeln!(f, "{}", text)?,
                Primitive::Panel => writeln!(f, "[Fancy Panel]")?,
            }
            for child in children.iter() {
                recursor(f, *child, nest_level + 1, dom)?;
            }
            Ok(())
        }

        for (i, root) in self.roots.iter().enumerate() {
            writeln!(f, "Begin root {}", i + 1)?;
            recursor(f, *root, 0, &self.dom)?;
            writeln!(f, "End root {}", i + 1)?;
        }

        Ok(())
    }
}

impl Dom for DemoDom {
    fn diff_primitive(&mut self, old: PrimitiveId, new: Primitive) {
        println!("diffing {:?}! cursor: {}", &new, self.cursor);
        self.dom.get_mut(&old).unwrap().0 = new;
    }
    fn remove(&mut self, id: PrimitiveId) {
        self.dom.remove(&id);
        self.roots.retain(|v| *v != id);
    }

    fn mount_as_child(&mut self, primitive: Primitive, parent: Option<PrimitiveId>) -> PrimitiveId {
        println!("mounting {:?}! cursor: {}", &primitive, self.cursor);
        let id = PrimitiveId(self.counter);
        self.counter += 1;
        self.dom.insert(id, (primitive, Vec::new()));
        if let Some(pid) = parent {
            self.dom
                .get_mut(&pid)
                .unwrap()
                .1
                .insert(self.cursor as usize, id);
        } else {
            self.roots.push(id);
        }
        self.cursor += 1;
        id
    }

    fn get_sub_context(&mut self, id: PrimitiveId) -> (PrimitiveId, &mut dyn Dom) {
        self.cursor = 0;
        (id, self)
    }

    fn set_cursor(&mut self, pos: u32) {
        self.cursor = pos;
    }

    fn get_cursor(&mut self) -> u32 {
        self.cursor
    }
}

#[test]
fn demo() {
    let mut dom = DemoDom::default();

    let mut context = Context::new();
    context.mount_root(app.e(()), &mut dom);
    println!("{}", &dom);
    loop {
        if context.msg_count() > 0 {
            context.process_messages(&mut dom);
            println!("{}", &dom);
        }
    }
}

fn app() -> Element {
    e::panel([
        counter.e(()),
        full_blinker.e(()),
        simple_blinker.e((3,)),
        simple_blinker.e((5,)),
    ])
}
