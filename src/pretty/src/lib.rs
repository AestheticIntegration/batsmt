
//! Pretty printing infrastructure
//!
//! Objects that can be rendered nicely as trees should implement `Pretty`.
//! This way, they get a `Display` instance for free.

extern crate pretty;

use {
    std::{
        fmt, borrow::{Borrow,ToOwned},
        collections::VecDeque,
    },
    pretty::{DocAllocator,DocBuilder,Doc,Arena},
};

// operations
#[derive(Debug,Clone)]
enum Op {
    Open(usize),
    Close,
    Newline,
    Space,
    SStatic(&'static str),
    Text(String),
}

/// The context used to print objects
pub struct Ctx {
    ops: VecDeque<Op>,
}

type StackItem<'a> = DocBuilder<'a, Arena<'a,()>>;

// a stack of document builders
struct Stack<'a> {
    pub st: Vec<StackItem<'a>>, // queue of operations
    pub boxes: Vec<usize>, // indentation levels
}

impl<'a> Stack<'a> {
    fn new() -> Self {
        Stack { st: Vec::new(), boxes: Vec::new(), }
    }

    fn enter_box(&mut self, n: usize, start: StackItem<'a>) {
        self.boxes.push(n);
        self.st.push(start); // to be combined with the rest
    }
    fn exit_box(&mut self) -> usize {
        debug_assert!(self.boxes.len() > 0);
        self.boxes.pop().expect("no box to exit")
    }

    // push `d` onto the stack
    fn push(&mut self, d: StackItem<'a>) {
        // in a box?
        if self.boxes.len() > 0 {
            match self.st.pop() {
                None => self.st.push(d),
                Some(d2) => self.st.push(d2.append(d)),
            }
        } else {
            self.st.push(d);
        }
    }

    fn pop(&mut self) -> StackItem<'a> {
        self.st.pop().expect("cannot pop from empty stack")
    }

    // assuming there's only one element remaining, pop it
    fn pop_last(&mut self) -> StackItem<'a> {
        debug_assert!(self.boxes.len() == 0, "all boxes should be closed");
        if self.st.len() == 1 {
            self.st.pop().unwrap()
        } else {
            panic!("pretty: ill formed document (expected 1 doc, not {})",
                self.st.len())
        }
    }
}

impl Ctx {
    // Allocate a new local printing context
    fn new() -> Self {
        Ctx { ops: VecDeque::new(), }
    }

    fn into_str(mut self, width: usize) -> String {
        let arena = Arena::new();

        // wrap into toplevel box
        self.ops.push_front(Op::Open(0));
        self.ops.push_back(Op::Close);

        // temporary docs
        let mut stack = Stack::new();

        while let Some(op) = self.ops.pop_front() {
            //println!("process op {:?} (stack len {} nboxes {})", op, stack.st.len(), stack.boxes.len());

            match op {
                Op::Open(n) => {
                    stack.enter_box(n, arena.nil());
                },
                Op::Newline => {
                    stack.push(arena.newline());
                },
                Op::Space => {
                    stack.push(arena.space());
                },
                Op::Close => {
                    let n = stack.exit_box();
                    let mut d = stack.pop();
                    d = d.group();
                    if n > 0 { d = d.nest(n) }
                    stack.push(d) // might combine with previous box
                },
                Op::SStatic(str) => {
                    stack.push(arena.text(str));
                },
                Op::Text(s) => {
                    stack.push(arena.text(s));
                },
            }
        }

        // extract top doc
        let d : Doc<_> = stack.pop_last().into();

        // render to a string
        let mut s = Vec::new();
        d.render(width, &mut s).unwrap();
        String::from_utf8(s).unwrap()
    }
}

// Re-export stuff from the pretty printer lib
impl Ctx {
    fn push_(&mut self, op: Op) -> &mut Self { self.ops.push_back(op); self }
    pub fn str(&mut self, s: &'static str) -> &mut Self { self.push_(Op::SStatic(s)) }
    pub fn text<U>(&mut self, u: &U) -> &mut Self
        where U:ToOwned<Owned=String>, String:Borrow<U>
    { self.push_(Op::Text(u.to_owned())) }
    pub fn text_string(&mut self, s: String) -> &mut Self { self.push_(Op::Text(s)) }
    pub fn newline(&mut self) -> &mut Self { self.push_(Op::Newline) }
    pub fn space(&mut self) -> &mut Self { self.push_(Op::Space) }
    fn open_indent(&mut self, u: usize) -> &mut Self { self.push_(Op::Open(u)); self }
    fn close(&mut self) -> &mut Self { self.push_(Op::Close); self }

    pub fn pp<T:Pretty>(&mut self, x: &T) -> &mut Self { x.pp(self); self }

    /// Call `f` in a box with given indentation
    pub fn with_indent<F,U>(&mut self, n: usize, f: F) -> &mut Self
        where F: FnOnce(&mut Ctx) -> U
    {
        self.open_indent(n);
        f(self);
        self.close();
        self
    }

    pub fn with_box<F>(&mut self, f: F) -> &mut Self where F: FnOnce(&mut Ctx) { self.with_indent(0,f) }

    pub fn sexp<F,U>(&mut self, f: F) -> &mut Self
        where F: FnOnce(&mut Ctx) -> U
    { self.str("("); self.with_indent(1,f); self.str(")"); self }

    /// `ctx.array(sep, arr)` prints elements of `arr` with `str` in between
    pub fn array<Sep: Pretty, U:Pretty>(&mut self, sep: Sep, arr: &[U]) -> &mut Self 
    {
        for (i,x) in arr.iter().enumerate() {
            if i > 0 { sep.pp(self); }
            x.pp(self)
        }
        self
    }

    /// `ctx.array(sep, arr)` prints elements of `arr` with `str` in between
    pub fn iter<Sep, I, U>(&mut self, sep: Sep, iter: I) -> &mut Self 
        where Sep: Pretty, U: Pretty, I: Iterator<Item=U>
    {
        for (i,x) in iter.enumerate() {
            if i > 0 { sep.pp(self); }
            x.pp(self)
        }
        self
    }
}

/// Default printing width, in case one wants to overload `Pretty.width`
pub const WIDTH : usize = 80;

/// A pretty-printable type.
///
/// Pretty printing is done via `pp`, which mutates the context
/// passed as an argument.
pub trait Pretty {
    /// Pretty print itself into the given context
    fn pp(&self, ctx: &mut Ctx);

    /// Width for printing. Default is `WIDTH`
    fn width(&self) -> usize { WIDTH }

    /// Automatic display into a formatter. This can be used to implement `Debug` or `Display`.
    fn pp_fmt(&self, out: &mut fmt::Formatter) -> fmt::Result {
        let mut ctx = Ctx::new();
        self.pp(&mut ctx);
        let s = ctx.into_str(self.width());
        write!(out, "{}", &s)
    }
}

// ability to use `Op` directly as a printable object
impl Pretty for Op {
    fn pp(&self, ctx: &mut Ctx) { ctx.push_(self.clone()); }
}

/// Display a newline
pub fn newline() -> impl Pretty { Op::Newline }

/// Display a space (or break)
pub fn space() -> impl Pretty { Op::Space }

/// Display a static string
pub fn str(s: &'static str) -> impl Pretty { Op::SStatic(s) }

/// Display a dynamic (owned) string
pub fn string(s: String) -> impl Pretty { Op::Text(s) }

/// Display a dynamic (owned) string
pub fn text<U:Into<String>>(u: U) -> impl Pretty { Op::Text(u.into()) }

impl<'a, T: Pretty> Pretty for &'a T {
    fn pp(&self, ctx: &mut Ctx) { (*self).pp(ctx) }
}

/// Print arrays as S-expressions
impl<T> Pretty for [T] where T : Pretty {
    fn pp(&self, ctx: &mut Ctx) {
        ctx.sexp(|ctx| { ctx.array(" ", &self); });
    }
}

impl<T> Pretty for Vec<T> where T : Pretty {
    fn pp(&self, ctx: &mut Ctx) { self.as_slice().pp(ctx) }
}

/// Automatic definition of `Display` from `Pretty`
#[macro_export]
macro_rules! pretty_display {
    ($t:ty) => {
        impl fmt::Display for $t {
            fn fmt(&self, out: &mut fmt::Formatter) -> fmt::Result
                { Pretty::pp_fmt(&self,out) }
        }
    };
    /* TODO: find how to define Display automatically for parametrized types
    ($t:ty ; $( $param:ident ),*) => {
        impl fmt::Display<$($param : fmt::Pretty),*> for $t< $($param),*> {
            fn fmt(&self, out: &mut fmt::Formatter) -> fmt::Result
                { Pretty::pp_fmt(&self,out) }
        }
    };
    */
}

// Implementations

impl<'a> Pretty for &'a str {
    fn pp(&self, ctx: &mut Ctx) { ctx.text_string(self.to_string()); }
}
impl Pretty for String {
    fn pp(&self, ctx: &mut Ctx) { ctx.text_string(self.clone()); }
}

#[test]
fn test_display() {
    #[derive(Copy,Clone)]
    struct Foo(u32);

    impl Pretty for Foo {
        fn pp(&self, ctx: &mut Ctx) { ctx.text_string(self.0.to_string()); }
    };
    pretty_display!(Foo);

    let foo = Foo(42);
    let s = format!("{}", &foo);

    assert_eq!("42", s);

    struct V<T>(Vec<T>);
    impl<T:Pretty> Pretty for V<T> {
        fn pp(&self, ctx: &mut Ctx) { self.0.pp(ctx) }
    };

    /* FIXME
    pretty_display!(V; T);
    */
    impl<T:Pretty> fmt::Display for V<T> {
        fn fmt(&self, out: &mut fmt::Formatter) -> fmt::Result { Pretty::pp_fmt(&self,out) }
    }

    let s2 = format!("{}", &V(vec![Foo(1), Foo(23), Foo(105)]));

    assert_eq!("(1 23 105)", s2);
}