use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::Debug;
use std::rc::Rc;

use owo_colors::OwoColorize;

use crate::{Blob, Block, Op, Prog, UpValue, Value};
use crate::error::{Error, ErrorKind};
use crate::RustFunction;
pub use crate::Type;

macro_rules! error {
    ( $thing:expr, $kind:expr) => {
        return Err($thing.error($kind, None));
    };
    ( $thing:expr, $kind:expr, $msg:expr) => {
        return Err($thing.error($kind, Some($msg)));
    };
}

#[derive(Debug)]
struct Frame {
    stack_offset: usize,
    block: Rc<RefCell<Block>>,
    ip: usize,
}

pub struct VM {
    upvalues: HashMap<usize, Rc<RefCell<UpValue>>>,

    stack: Vec<Value>,
    frames: Vec<Frame>,

    blobs: Vec<Rc<Blob>>,

    print_blocks: bool,
    print_ops: bool,

    extern_functions: Vec<RustFunction>,

}

#[derive(Eq, PartialEq)]
pub enum OpResult {
    Yield,
    Continue,
    Done,
}

impl VM {
    pub fn new() -> Self {
        Self {
            upvalues: HashMap::new(),

            stack: Vec::new(),
            frames: Vec::new(),
            blobs: Vec::new(),
            print_blocks: false,
            print_ops: false,

            extern_functions: Vec::new()
        }
    }

    pub fn print_blocks(mut self, b: bool) -> Self {
        self.print_blocks = b;
        self
    }

    pub fn print_ops(mut self, b: bool) -> Self {
        self.print_ops = b;
        self
    }

    fn drop_upvalue(&mut self, slot: usize, value: Value) {
        if let Entry::Occupied(entry) = self.upvalues.entry(slot) {
            entry.get().borrow_mut().close(value);
            entry.remove();
        } else {
            unreachable!();
        }
    }

    fn find_upvalue(&mut self, slot: usize) -> &mut Rc<RefCell<UpValue>> {
        self.upvalues.entry(slot).or_insert(
            Rc::new(RefCell::new(UpValue::new(slot))))
    }

    fn pop(&mut self) -> Value {
        self.stack.pop().unwrap()
    }

    fn pop_twice(&mut self) -> (Value, Value) {
        let (a, b) = (self.stack.remove(self.stack.len() - 1),
                      self.stack.remove(self.stack.len() - 1));
        (b, a)  // this matches the order they were on the stack
    }

    fn _peek_up(&self, amount: usize) -> Option<&Value> {
        self.stack.get(self.stack.len() - amount)
    }

    fn frame(&self) -> &Frame {
        let last = self.frames.len() - 1;
        &self.frames[last]
    }

    fn frame_mut(&mut self) -> &mut Frame {
        let last = self.frames.len() - 1;
        &mut self.frames[last]
    }

    fn op(&self) -> Op {
        let ip = self.frame().ip;
        self.frame().block.borrow().ops[ip].clone()
    }

    fn error(&self, kind: ErrorKind, message: Option<String>) -> Error {
        let frame = self.frames.last().unwrap();
        Error {
            kind,
            file: frame.block.borrow().file.clone(),
            line: frame.block.borrow().line(frame.ip),
            message,
        }
    }

    fn eval_op(&mut self, op: Op) -> Result<OpResult, Error> {
        match op {
            Op::Illegal => {
                error!(self, ErrorKind::InvalidProgram);
            }

            Op::Unreachable => {
                error!(self, ErrorKind::Unreachable);
            }

            Op::Pop => {
                self.stack.pop().unwrap();
            }

            Op::Yield => {
                self.frame_mut().ip += 1;
                return Ok(OpResult::Yield);
            }

            Op::PopUpvalue => {
                let value = self.stack.pop().unwrap();
                let slot = self.stack.len();
                self.drop_upvalue(slot, value);
            }

            Op::Constant(value) => {
                let offset = self.frame().stack_offset;
                let value = match value {
                    Value::Function(_, block) => {
                        let mut ups = Vec::new();
                        for (slot, is_up, _) in block.borrow().ups.iter() {
                            let up = if *is_up {
                                if let Value::Function(local_ups, _) = &self.stack[offset] {
                                    Rc::clone(&local_ups[*slot])
                                } else {
                                    unreachable!()
                                }
                            } else {
                                let slot = self.frame().stack_offset + slot;
                                Rc::clone(self.find_upvalue(slot))
                            };
                            ups.push(up);
                        }
                        Value::Function(ups, block)
                    },
                    _ => value.clone(),
                };
                self.stack.push(value);
            }

            Op::Get(field) => {
                let inst = self.stack.pop();
                if let Some(Value::BlobInstance(ty, values)) = inst {
                    let slot = self.blobs[ty].name_to_field.get(&field).unwrap().0;
                    self.stack.push(values.borrow()[slot].clone());
                } else {
                    error!(self, ErrorKind::RuntimeTypeError(Op::Get(field.clone()), vec![inst.unwrap()]));
                }
            }

            Op::Set(field) => {
                let value = self.stack.pop().unwrap();
                let inst = self.stack.pop();
                if let Some(Value::BlobInstance(ty, values)) = inst {
                    let slot = self.blobs[ty].name_to_field.get(&field).unwrap().0;
                    values.borrow_mut()[slot] = value;
                } else {
                    error!(self, ErrorKind::RuntimeTypeError(Op::Get(field.clone()), vec![inst.unwrap()]));
                }
            }

            Op::Neg => {
                match self.stack.pop().unwrap() {
                    Value::Float(a) => self.stack.push(Value::Float(-a)),
                    Value::Int(a) => self.stack.push(Value::Int(-a)),
                    a => error!(self, ErrorKind::RuntimeTypeError(op, vec![a])),
                }
            }

            Op::Add => {
                match self.pop_twice() {
                    (Value::Float(a), Value::Float(b)) => self.stack.push(Value::Float(a + b)),
                    (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Int(a + b)),
                    (Value::String(a), Value::String(b)) => {
                        self.stack.push(Value::String(Rc::from(format!("{}{}", a, b))))
                    }
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Sub => {
                match self.pop_twice() {
                    (Value::Float(a), Value::Float(b)) => self.stack.push(Value::Float(a - b)),
                    (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Int(a - b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Mul => {
                match self.pop_twice() {
                    (Value::Float(a), Value::Float(b)) => self.stack.push(Value::Float(a * b)),
                    (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Int(a * b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Div => {
                match self.pop_twice() {
                    (Value::Float(a), Value::Float(b)) => self.stack.push(Value::Float(a / b)),
                    (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Int(a / b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Equal => {
                match self.pop_twice() {
                    (Value::Float(a), Value::Float(b)) => self.stack.push(Value::Bool(a == b)),
                    (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Bool(a == b)),
                    (Value::String(a), Value::String(b)) => self.stack.push(Value::Bool(a == b)),
                    (Value::Bool(a), Value::Bool(b)) => self.stack.push(Value::Bool(a == b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Less => {
                match self.pop_twice() {
                    (Value::Float(a), Value::Float(b)) => self.stack.push(Value::Bool(a < b)),
                    (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Bool(a < b)),
                    (Value::String(a), Value::String(b)) => self.stack.push(Value::Bool(a < b)),
                    (Value::Bool(a), Value::Bool(b)) => self.stack.push(Value::Bool(a < b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Greater => {
                match self.pop_twice() {
                    (Value::Float(a), Value::Float(b)) => self.stack.push(Value::Bool(a > b)),
                    (Value::Int(a), Value::Int(b)) => self.stack.push(Value::Bool(a > b)),
                    (Value::String(a), Value::String(b)) => self.stack.push(Value::Bool(a > b)),
                    (Value::Bool(a), Value::Bool(b)) => self.stack.push(Value::Bool(a > b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::And => {
                match self.pop_twice() {
                    (Value::Bool(a), Value::Bool(b)) => self.stack.push(Value::Bool(a && b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Or => {
                match self.pop_twice() {
                    (Value::Bool(a), Value::Bool(b)) => self.stack.push(Value::Bool(a || b)),
                    (a, b) => error!(self, ErrorKind::RuntimeTypeError(op, vec![a, b])),
                }
            }

            Op::Not => {
                match self.stack.pop().unwrap() {
                    Value::Bool(a) => self.stack.push(Value::Bool(!a)),
                    a => error!(self, ErrorKind::RuntimeTypeError(op, vec![a])),
                }
            }

            Op::Jmp(line) => {
                self.frame_mut().ip = line;
                return Ok(OpResult::Continue);
            }

            Op::JmpFalse(line) => {
                if matches!(self.stack.pop(), Some(Value::Bool(false))) {
                    self.frame_mut().ip = line;
                    return Ok(OpResult::Continue);
                }
            }

            Op::Assert => {
                if matches!(self.stack.pop(), Some(Value::Bool(false))) {
                    error!(self, ErrorKind::Assert);
                }
                self.stack.push(Value::Bool(true));
            }

            Op::ReadUpvalue(slot) => {
                let offset = self.frame().stack_offset;
                let value = match &self.stack[offset] {
                    Value::Function(ups, _) => {
                        ups[slot].borrow().get(&self.stack)
                    }
                    _ => unreachable!(),
                };
                self.stack.push(value);
            }

            Op::AssignUpvalue(slot) => {
                let offset = self.frame().stack_offset;
                let value = self.stack.pop().unwrap();
                let slot = match &self.stack[offset] {
                    Value::Function(ups, _) => Rc::clone(&ups[slot]),
                    _ => unreachable!(),
                };
                slot.borrow_mut().set(&mut self.stack, value);
            }

            Op::ReadLocal(slot) => {
                let slot = self.frame().stack_offset + slot;
                self.stack.push(self.stack[slot].clone());
            }

            Op::AssignLocal(slot) => {
                let slot = self.frame().stack_offset + slot;
                self.stack[slot] = self.stack.pop().unwrap();
            }

            Op::Define(_) => {}

            Op::Call(num_args) => {
                let new_base = self.stack.len() - 1 - num_args;
                match self.stack[new_base].clone() {
                    Value::Blob(blob_id) => {
                        let blob = &self.blobs[blob_id];

                        let mut values = Vec::with_capacity(blob.name_to_field.len());
                        for _ in 0..values.capacity() {
                            values.push(Value::Nil);
                        }

                        self.stack.pop();
                        self.stack.push(Value::BlobInstance(blob_id, Rc::new(RefCell::new(values))));
                    }
                    Value::Function(_, block) => {
                        let inner = block.borrow();
                        let args = inner.args();
                        if args.len() != num_args {
                            error!(self,
                                ErrorKind::InvalidProgram,
                                format!("Invalid number of arguments, got {} expected {}.",
                                    num_args, args.len()));
                        }

                        if self.print_blocks {
                            inner.debug_print();
                        }
                        self.frames.push(Frame {
                            stack_offset: new_base,
                            block: Rc::clone(&block),
                            ip: 0,
                        });
                        return Ok(OpResult::Continue);
                    }
                    Value::ExternFunction(slot) => {
                        let extern_func = self.extern_functions[slot];
                        let res = match extern_func(&self.stack[new_base+1..], false) {
                            Ok(value) => value,
                            Err(ek) => error!(self, ek, "Wrong arguments to external function".to_string()),
                        };
                        self.stack.truncate(new_base);
                        self.stack.push(res);
                    }
                    _ => {
                        unreachable!()
                    }
                }
            }

            Op::Print => {
                println!("PRINT: {:?}", self.stack.pop().unwrap());
            }

            Op::Return => {
                let last = self.frames.pop().unwrap();
                if self.frames.is_empty() {
                    return Ok(OpResult::Done);
                } else {
                    self.stack[last.stack_offset] = self.stack.pop().unwrap();
                    for slot in last.stack_offset+1..self.stack.len() {
                        if self.upvalues.contains_key(&slot) {
                            let value = self.stack[slot].clone();
                            self.drop_upvalue(slot, value);
                        }
                    }
                    self.stack.truncate(last.stack_offset + 1);
                }
            }
        }
        self.frame_mut().ip += 1;
        Ok(OpResult::Continue)
    }

    pub fn print_stack(&self) {
        let start = self.frame().stack_offset;
        print!("    {:3} [", start);
        for (i, s) in self.stack.iter().skip(start).enumerate() {
            if i != 0 {
                print!(" ");
            }
            print!("{:?}", s.green());
        }
        println!("]");

        println!("{:5} {:05} {:?}",
            self.frame().block.borrow().line(self.frame().ip).red(),
            self.frame().ip.blue(),
            self.frame().block.borrow().ops[self.frame().ip]);
    }

    pub fn init(&mut self, prog: &Prog) {
        let block = Rc::clone(&prog.blocks[0]);
        self.blobs = prog.blobs.clone();
        self.extern_functions = prog.functions.clone();
        self.stack.clear();
        self.frames.clear();

        self.stack.push(Value::Function(Vec::new(), Rc::clone(&block)));

        self.frames.push(Frame {
            stack_offset: 0,
            block,
            ip: 0
        });
    }

    pub fn run(&mut self) -> Result<OpResult, Error> {

        if self.print_blocks {
            println!("\n    [[{}]]\n", "RUNNING".red());
            self.frame().block.borrow().debug_print();
        }

        loop {
            if self.print_ops {
                self.print_stack()
            }

            let op = self.eval_op(self.op())?;
            if matches!(op, OpResult::Done | OpResult::Yield) {
                return Ok(op);
            }
        }
    }

    fn check_op(&mut self, op: Op) -> Result<(), Error> {
        match op {
            Op::Unreachable => {}

            Op::Jmp(_line) => {}

            Op::Yield => {}

            Op::Constant(ref value) => {
                match value.clone() {
                    Value::Function(_, block) => {
                        self.stack.push(Value::Function(Vec::new(), block.clone()));

                        let mut types = Vec::new();
                        for (slot, is_up, ty) in block.borrow().ups.iter() {
                            if *is_up {
                                types.push(ty.clone());
                            } else {
                                types.push(self.stack[*slot].as_type());
                            }
                        }

                        let mut block_mut = block.borrow_mut();
                        for (i, (_, is_up, ty)) in block_mut.ups.iter_mut().enumerate() {
                            if *is_up { continue; }

                            let suggestion = &types[i];
                            if ty.is_unkown() {
                                *ty = suggestion.clone();
                            } else {
                                if ty != suggestion {
                                    error!(self,
                                           ErrorKind::TypeError(op.clone(),
                                                    vec![ty.clone(), suggestion.clone()]),
                                           "Failed to infer type.".to_string());
                                }
                            }
                        };
                    },
                    _ => {
                        self.stack.push(value.clone());
                    }
                }
            }

            Op::Get(field) => {
                let inst = self.stack.pop();
                if let Some(Value::BlobInstance(ty, _)) = inst {
                    let value = self.blobs[ty].name_to_field.get(&field).unwrap().1.as_value();
                    self.stack.push(value);
                } else {
                    self.stack.push(Value::Nil);
                    error!(self, ErrorKind::RuntimeTypeError(Op::Get(field.clone()), vec![inst.unwrap()]));
                }
            }

            Op::Set(field) => {
                let value = self.stack.pop().unwrap();
                let inst = self.stack.pop();
                if let Some(Value::BlobInstance(ty, _)) = inst {
                    let ty = &self.blobs[ty].name_to_field.get(&field).unwrap().1;
                    if ty != &Type::from(&value) {
                        error!(self, ErrorKind::RuntimeTypeError(Op::Set(field.clone()), vec![inst.unwrap()]));
                    }
                } else {
                    error!(self, ErrorKind::RuntimeTypeError(Op::Set(field.clone()), vec![inst.unwrap()]));
                }
            }

            Op::PopUpvalue => {
                self.stack.pop().unwrap();
            }

            Op::ReadUpvalue(slot) => {
                let value = self.frame().block.borrow().ups[slot].2.as_value();
                self.stack.push(value);
            }

            Op::AssignUpvalue(slot) => {
                let var = self.frame().block.borrow().ups[slot].2.clone();
                let up = self.stack.pop().unwrap().as_type();
                if var != up {
                    error!(self, ErrorKind::TypeError(op, vec![var, up]),
                                  "Incorrect type for upvalue.".to_string());
                }
            }

            Op::Return => {
                let a = self.stack.pop().unwrap();
                let inner = self.frame().block.borrow();
                let ret = inner.ret();
                if a.as_type() != *ret {
                    error!(self, ErrorKind::TypeError(op, vec![a.as_type(),
                                                               ret.clone()]),
                                                      "Not matching return type.".to_string());
                }
            }

            Op::Print => {
                self.pop();
            }

            Op::Define(ref ty) => {
                let top_type = self.stack.last().unwrap().as_type();
                match (ty, top_type) {
                    (Type::UnknownType, top_type)
                        if top_type != Type::UnknownType => {}
                    (a, b) if a != &b => {
                        error!(self,
                            ErrorKind::TypeError(
                                op.clone(),
                                vec![a.clone(), b.clone()]),
                                format!("Tried to assign a type {:?} to type {:?}.", a, b)
                        );
                    }
                    _ => {}
                }
            }

            Op::Call(num_args) => {
                let new_base = self.stack.len() - 1 - num_args;
                match self.stack[new_base].clone() {
                    Value::Blob(blob_id) => {
                        let blob = &self.blobs[blob_id];

                        let mut values = Vec::with_capacity(blob.name_to_field.len());
                        for _ in 0..values.capacity() {
                            values.push(Value::Nil);
                        }

                        for (slot, ty) in blob.name_to_field.values() {
                            values[*slot] = ty.as_value();
                        }

                        self.stack.pop();
                        self.stack.push(Value::BlobInstance(blob_id, Rc::new(RefCell::new(values))));
                    }
                    Value::Function(_, block) => {
                        let inner = block.borrow();
                        let args = inner.args();
                        if args.len() != num_args {
                            error!(self,
                                ErrorKind::InvalidProgram,
                                format!("Invalid number of arguments, got {} expected {}.",
                                    num_args, args.len()));
                        }

                        let stack_args = &self.stack[self.stack.len() - args.len()..];
                        let stack_args: Vec<_> = stack_args.iter().map(|x| x.as_type()).collect();
                        if args != &stack_args {
                            error!(self,
                                ErrorKind::TypeError(op.clone(), vec![]),
                                format!("Expected args of type {:?} but got {:?}.",
                                    args, stack_args));
                        }

                        self.stack[new_base] = block.borrow().ret().as_value();

                        self.stack.truncate(new_base + 1);
                    }
                    Value::ExternFunction(slot) => {
                        let extern_func = self.extern_functions[slot];
                        let res = match extern_func(&self.stack[new_base+1..], false) {
                            Ok(value) => value,
                            Err(ek) => {
                                self.stack.truncate(new_base);
                                self.stack.push(Value::Nil);
                                error!(self, ek, "Wrong arguments to external function".to_string())
                            }
                        };
                        self.stack.truncate(new_base);
                        self.stack.push(res);
                    }
                    _ => {
                        error!(self,
                            ErrorKind::TypeError(op.clone(), vec![self.stack[new_base].as_type()]),
                            format!("Tried to call non-function {:?}", self.stack[new_base]));
                    }
                }
            }

            Op::JmpFalse(_) => {
                match self.pop() {
                    Value::Bool(_) => {},
                    a => { error!(self, ErrorKind::TypeError(op.clone(), vec![a.as_type()])) },
                }
            }
            _ => {
                self.eval_op(op)?;
                return Ok(())
            }
        }
        self.frame_mut().ip += 1;
        Ok(())
    }

    fn typecheck_block(&mut self, block: Rc<RefCell<Block>>) -> Vec<Error> {
        self.stack.clear();
        self.frames.clear();

        self.stack.push(Value::Function(Vec::new(), Rc::clone(&block)));
        for arg in block.borrow().args() {
            self.stack.push(arg.as_value());
        }

        self.frames.push(Frame {
            stack_offset: 0,
            block,
            ip: 0
        });

        if self.print_blocks {
            println!("\n    [[{}]]\n", "TYPECHECK".purple());
            self.frame().block.borrow().debug_print();
        }

        let mut errors = Vec::new();
        loop {
            let ip = self.frame().ip;
            if ip >= self.frame().block.borrow().ops.len() {
                break;
            }

            if self.print_ops {
                self.print_stack()
            }

            if let Err(e) = self.check_op(self.op()) {
                errors.push(e);
                self.frame_mut().ip += 1;
            }

            if !self.stack.is_empty() {
                let ident = self.stack.pop().unwrap().identity();
                self.stack.push(ident);
            }
        }
        errors
    }

    pub fn typecheck(&mut self, prog: &Prog) -> Result<(), Vec<Error>> {
        let mut errors = Vec::new();

        self.blobs = prog.blobs.clone();
        self.extern_functions = prog.functions.clone();
        for block in prog.blocks.iter() {
            errors.append(&mut self.typecheck_block(Rc::clone(block)));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[cfg(test)]
mod tests {
    mod typing {
        use crate::error::ErrorKind;
        use crate::test_string;

        test_string!(uncallable_type, "
                 f := fn i: int {
                     i()
                 }",
                 [ErrorKind::TypeError(_, _)]);

        test_string!(wrong_params, "
                 f : fn -> int = fn a: int -> int {}",
                 [ErrorKind::TypeError(_, _), ErrorKind::TypeError(_, _)]);

        test_string!(wrong_ret, "
                 f : fn -> int = fn {}",
                 [ErrorKind::TypeError(_, _)]);
    }
}
