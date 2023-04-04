use std::borrow::BorrowMut;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

use crate::backend::Kernel;
use crate::schedule::ScheduleIr;
use crate::trace::{Internal, IR};
use crate::var::{Op, VarId};

///
/// This is the default Just In Time Compiler (JIT).
///
pub static JIT: Lazy<Mutex<Jit>> = Lazy::new(|| Mutex::new(Jit::default()));

// TODO: pooling for paralel exectution
///
/// The Jit Compiler first generates schedules (Intermediate Representation) from a Trace.
/// It then assembles and compiles a Kernel depending on the Backend.
///
/// Ir -> [Schedule; N] -> [Kernel; N]
///
/// Where N is the number of schedule groups.
/// These are extracted from the scheduled variables in the Ir.
///
#[derive(Debug, Default)]
pub struct Jit {
    pub schedules: Vec<ScheduleIr>,
    pub kernels: Vec<Box<dyn Kernel>>,
}

#[derive(Debug)]
struct Pass {
    ids: HashSet<VarId>,
    deps: HashSet<VarId>,
    // access: HashMap<VarId, Access>,
    size: usize,
}

impl Pass {
    ///
    /// Try to merge `other` into self.
    /// This is only possible if `other.size` == `self.size`
    /// and `other` does not depend on `self`.
    ///
    fn try_merge(&mut self, other: &Self) -> Option<()> {
        if self.size != other.size {
            return None;
        }
        if !self.ids.is_disjoint(&other.deps) {
            return None;
        }
        self.ids = self.ids.union(&other.ids).cloned().collect();
        self.deps = self.deps.union(&other.deps).cloned().collect();
        Some(())
    }
}

///
///  Evaluates a Ir by first constructing Schedules, which are then compiled and assembled
///  into kernels.
///
///  A the end, all scheduled variables are overwritten with the calculated data.
///
pub fn eval() {
    let mut jit = JIT.lock(); // always lock JIT before IR
    let mut ir = IR.lock();
    jit.eval(&mut ir);
}

impl Jit {
    ///
    /// Evaluates a Ir by first constructing Schedules, which are then compiled and assembled
    /// into kernels.
    ///
    /// A the end, all scheduled variables are overwritten with the calculated data.
    ///
    pub fn eval(&mut self, ir: &mut Internal) {
        // For every scheduled variable (destination) we have to create a new buffer (except if it
        // is void)
        for id in ir.scheduled.clone() {
            let var = ir.var(id);
            // Do not reallocate on scheduled variables (don't yet know if this is right)
            if var.buffer.is_none() {
                let size = ir.var(id).size;
                let ty_size = ir.var(id).ty.size();
                let buffer = ir.backend.as_ref().unwrap().buffer_uninit(size * ty_size);

                let mut var = ir.var_mut(id);
                var.buffer = Some(buffer);
            }
        }

        self.compile(ir);
        let n_kernels = self.kernels.len();
        for i in 0..n_kernels {
            let (kernel, schedule) = self.schedule_kernel(i);
            kernel.execute_async(schedule);
            ir.backend.as_ref().unwrap().synchronize(); // TODO: improve by sorting
        }

        // After executing the kernels, the Ir is cleaned up.
        // To do so, we first decrement the refcount and then set the ParamType to Input and op to
        // Data
        for id in ir.scheduled.clone() {
            let var = ir.var_mut(id);

            // Set op and type for next kernel:
            // var.param_ty = ParamType::Input;
            var.op = Op::Data;

            // Clear dependencies:
            let deps = var.deps.clone();
            var.deps.clear();

            for dep in deps {
                ir.dec_rc(dep);
            }

            ir.dec_rc(id);
        }
        ir.scheduled.clear();
    }
    ///
    /// Collect neccesary passes from `ir`.
    /// NOTE: We would need to correctly assign dependencies on scatter
    /// NOTE: Passes are in ordered (DAG ordering)
    ///
    pub fn passes(&self, ir: &Internal) -> Vec<Pass> {
        ///
        /// Gets the dependencies of `id` in `scheduled`
        ///
        fn dependencies_in(
            ir: &Internal,
            id: VarId,
            scheduled: &HashSet<VarId>,
            deps: &mut HashSet<VarId>,
        ) {
            if scheduled.contains(&id) {
                deps.insert(id);
            }
            let var = ir.var(id);
            for dep in var.deps.iter() {
                dependencies_in(ir, *dep, scheduled, deps);
            }
        }
        let scheduled = ir.scheduled.iter().cloned().collect::<HashSet<_>>();
        let passes = ir
            .scheduled
            .iter()
            .map(|id| {
                let mut deps = HashSet::new();
                dependencies_in(ir, *id, &scheduled, &mut deps);
                let deps = deps.difference(&HashSet::from([*id])).cloned().collect();
                Pass {
                    ids: HashSet::from([*id]),
                    deps,
                    size: ir.var(*id).size,
                }
            })
            .collect::<Vec<_>>();

        // TODO: use passes to flatten dependencies and stop traversal eraly.

        passes
    }
    ///
    /// Compiles the computation graph of all scheduled variables in a Ir.
    ///
    /// First, all scheduled variables with the same size are grouped.
    /// Then, a Schedule Intermediate Representation is constructed from the groups.
    /// In the end a set of Kernels is assembled and compiled.
    ///
    fn compile(&mut self, ir: &Internal) {
        if ir.scheduled.len() == 0 {
            return;
        }
        self.schedules.clear();
        self.kernels.clear();

        // The problem we try to solve is the following:
        //
        // The nodes in `ir` are represent DAG, so do the `passes`.
        // They are also stored in a topological ordering.
        // We try to merge as many passes as possible.
        // Passes can only be merged if tey have the same size.
        //
        // This algorithm is not perfect and results in a potentially sub optimal result.
        // Also, using hash sets might not be the best approach.
        let mut passes = self.passes(&ir);
        for i in (0..passes.len()).rev() {
            for j in (0..i).rev() {
                // Try to merge p1 into p0
                // SAFETY: We know that i and j are distinct indices and this does the same as
                // get_multiple_mut which is not stable yet.
                let p0 = unsafe { &mut *(&passes[j] as *const _ as *mut Pass) };
                let p1 = unsafe { &mut *(&passes[i] as *const _ as *mut Pass) };
                if p0.try_merge(p1).is_some() {
                    passes.remove(i);
                    break;
                }
            }
        }
        let first_register = ir.backend.as_ref().unwrap().first_register();
        self.schedules = passes
            .iter()
            .map(|pass| {
                let mut s = ScheduleIr::new(first_register, pass.size);
                s.collect_vars(ir, &pass.ids.iter().cloned().collect::<Vec<_>>());
                s
            })
            .collect::<Vec<_>>();

        // TODO: paralelize by populating kernels first.
        self.kernels = self
            .schedules
            .iter()
            .map(|mut s| {
                let mut kernel = ir.backend.as_ref().unwrap().new_kernel();
                kernel.assemble(&mut s);
                // kernel.compile();
                kernel
            })
            .collect::<Vec<_>>();
        for kernel in self.kernels.iter_mut() {
            kernel.compile();
        }
    }
    pub fn schedule_kernel(&mut self, i: usize) -> (&mut Box<dyn Kernel>, &mut ScheduleIr) {
        (&mut self.kernels[i], &mut self.schedules[i])
    }
    ///
    /// Writes the kernel assemblies into a string which can then be checked by snapshot testing
    /// tools such as insta.
    ///
    pub fn kernel_debug(&self) -> String {
        let mut string = String::new();
        for (i, k) in self.kernels.iter().enumerate() {
            writeln!(string, "===============================================").unwrap();
            writeln!(string, "Kernel {}:", i).unwrap();
            writeln!(string, "").unwrap();
            write!(string, "{}", k.assembly()).unwrap();
        }
        string
    }
}

#[cfg(test)]
mod test {}
