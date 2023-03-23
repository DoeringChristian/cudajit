use std::collections::HashSet;
use std::num::NonZeroU64;
use std::sync::Arc;

use bytemuck::cast_slice;
use cust::prelude::DeviceBuffer;
use cust::util::SliceExt;
use smallvec::{smallvec, SmallVec};

use crate::iterators::DepIter;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ParamType {
    #[default]
    None,
    Input,
    Output,
    Literal,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Op {
    // Data,
    #[default]
    Nop,
    Data,
    Neg,
    Not,
    Sqrt,
    Abs,
    Add, // Add two variables
    Sub,
    Mul,
    Div,
    Mod,
    Mulhi,
    Fma,
    Min,
    Max,
    Cail,
    Floor,
    Round,
    Trunc,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Select,
    Popc,
    Clz,
    Ctz,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Rcp,
    Rsqrt,
    Sin,
    Cos,
    Exp2,
    Log2,
    Cast,
    Bitcast,
    Gather,
    Scatter,
    Idx,
    ConstF32(f32), // Set a constant value
                   // ConstU32(u32), // Set a constant value
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum VarType {
    // Void,
    #[default]
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    Ptr,
    F16,
    F32,
    F64,
}
impl VarType {
    // Returns the register prefix for this variable
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Bool => "%p",
            Self::I8 => "%b",
            Self::U8 => "%b",
            Self::I16 => "%w",
            Self::U16 => "%w",
            Self::I32 => "%r",
            Self::U32 => "%r",
            Self::I64 => "%rd",
            Self::U64 => "%rd",
            Self::Ptr => "%rd",
            Self::F16 => "%h",
            Self::F32 => "%f",
            Self::F64 => "%d",
        }
    }
    // Retuns the cuda/ptx Representation for this type
    pub fn name_cuda(&self) -> &'static str {
        match self {
            Self::Bool => "pred",
            Self::I8 => "s8",
            Self::U8 => "u8",
            Self::I16 => "s16",
            Self::U16 => "u16",
            Self::I32 => "s32",
            Self::U32 => "u32",
            Self::I64 => "s64",
            Self::U64 => "u64",
            Self::Ptr => "u64",
            Self::F16 => "f16",
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
    pub fn name_cuda_bin(&self) -> &'static str {
        match self {
            Self::Bool => "pred",
            Self::I8 => "b8",
            Self::U8 => "b8",
            Self::I16 => "b16",
            Self::U16 => "b16",
            Self::I32 => "b32",
            Self::U32 => "b32",
            Self::I64 => "b64",
            Self::U64 => "b64",
            Self::Ptr => "b64",
            Self::F16 => "b16",
            Self::F32 => "b32",
            Self::F64 => "b64",
        }
    }
    // Returns the size/stride of this variable
    pub fn size(&self) -> usize {
        match self {
            Self::Bool => 1,
            Self::I8 => 1,
            Self::U8 => 1,
            Self::I16 => 2,
            Self::U16 => 2,
            Self::I32 => 4,
            Self::U32 => 4,
            Self::I64 => 8,
            Self::U64 => 8,
            Self::Ptr => 8,
            Self::F16 => 2,
            Self::F32 => 4,
            Self::F64 => 8,
        }
    }
    pub fn is_uint(&self) -> bool {
        match self {
            Self::U8 | Self::U16 | Self::U32 | Self::U64 => true,
            _ => false,
        }
    }
    pub fn is_single(&self) -> bool {
        match self {
            Self::F32 => true,
            _ => false,
        }
    }
    pub fn is_double(&self) -> bool {
        match self {
            Self::F64 => true,
            _ => false,
        }
    }
}

///
///
///
#[derive(Debug, Default)]
pub struct Var {
    pub op: Op, // Operation used to construct the variable
    pub deps: SmallVec<[VarId; 4]>,
    pub ty: VarType,                                         // Type of the variable
    pub buffer: Option<Box<cust::memory::DeviceBuffer<u8>>>, // Optional buffer
    pub size: usize,                                         // number of elements
    pub param_ty: ParamType,                                 // Parameter type
}

impl Var {
    pub fn deps(&self) -> &[VarId] {
        &self.deps
    }
}

///
/// Helper struct for printing register names.
/// <prefix><register_index>
///

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct VarId(pub usize);

impl std::fmt::Display for VarId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ParamId(usize);

impl ParamId {
    pub fn offset(self) -> usize {
        self.0 * 8
    }
}

impl std::ops::Deref for ParamId {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Default)]
pub struct Ir {
    vars: Vec<Var>,
    pub scheduled: Vec<VarId>,
    // pub params: Vec<u64>, // Params vec![size, &buffer0, &buffer1]
}

impl Ir {
    // const FIRST_REGISTER: usize = 4;
    pub fn push_var(&mut self, mut var: Var) -> VarId {
        let id = VarId(self.vars.len());
        // var.reg = self.n_regs;
        // self.n_regs += 1;
        self.vars.push(var);
        id
    }
    pub fn var(&self, id: VarId) -> &Var {
        &self.vars[id.0]
    }
    pub fn var_mut(&mut self, id: VarId) -> &mut Var {
        &mut self.vars[id.0]
    }
    pub fn ids(&self) -> impl Iterator<Item = VarId> {
        (0..self.vars.len()).map(|i| VarId(i))
    }
    pub fn deps(&self, schedule: &[VarId]) -> DepIter {
        DepIter {
            ir: self,
            stack: Vec::from(schedule),
            discovered: HashSet::new(),
        }
    }
    pub fn vars(&self) -> &[Var] {
        &self.vars
    }
    pub fn push_var_intermediate(
        &mut self,
        op: Op,
        deps: &[VarId],
        ty: VarType,
        size: usize,
    ) -> VarId {
        self.push_var(Var {
            op: Op::Add,
            deps: SmallVec::from_slice(deps),
            ty,
            param_ty: ParamType::None,
            buffer: None,
            size,
        })
    }
    pub fn assert_ty(&self, ids: &[VarId]) -> (usize, &VarType) {
        let ty = ids.iter().map(|id| &self.var(*id).ty).reduce(|ty0, ty1| {
            assert_eq!(ty0, ty1);
            ty0
        });
        let size = ids
            .iter()
            .map(|id| &self.var(*id).size)
            .reduce(|s0, s1| s0.max(s1));
        (*size.unwrap(), ty.unwrap())
    }
}
impl Ir {
    pub fn add(&mut self, lhs: VarId, rhs: VarId) -> VarId {
        let (size, ty) = self.assert_ty(&[lhs, rhs]);
        self.push_var_intermediate(Op::Add, &[lhs, rhs], ty.clone(), size)
    }
    pub fn const_f32(&mut self, val: f32) -> VarId {
        self.push_var(Var {
            op: Op::ConstF32(val),
            deps: smallvec![],
            ty: VarType::F32,
            ..Default::default()
        })
    }
    pub fn buffer_f32(&mut self, slice: &[f32]) -> VarId {
        self.push_var(Var {
            param_ty: ParamType::Input,
            buffer: Some(Box::new(slice.as_dbuf().unwrap().cast::<u8>())),
            size: slice.len(),
            ty: VarType::F32,
            ..Default::default()
        })
    }
    pub fn to_host_f32(&mut self, id: VarId) -> Vec<f32> {
        let var = self.var(id);
        assert_eq!(var.ty, VarType::F32);
        let v = var.buffer.as_ref().unwrap().as_host_vec().unwrap();
        Vec::from(cast_slice(&v))
    }
}
