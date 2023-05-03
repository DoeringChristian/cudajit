use crate::schedule::{Env, ScheduleIr};
use crate::trace::VarType;
use crate::var::ParamType;

pub fn assemble_entry(
    asm: &mut impl std::fmt::Write,
    ir: &ScheduleIr,
    env: &Env,
    entry_point: &str,
) -> std::fmt::Result {
    let n_params = 1 + env.buffers().len() + env.textures().len(); // Add 1 for size
    let n_regs = ir.n_regs();

    /* Special registers:
         %r0   :  Index
         %r1   :  Step
         %r2   :  Size
         %p0   :  Stopping predicate
         %rd0  :  Temporary for parameter pointers
         %rd1  :  Pointer to parameter table in global memory if too big
         %b3, %w3, %r3, %rd3, %f3, %d3, %p3: reserved for use in compound
         statements that must write a temporary result to a register.
    */

    writeln!(asm, ".version {}.{}", 8, 0)?;
    writeln!(asm, ".target {}", "sm_86")?;
    writeln!(asm, ".address_size 64")?;

    writeln!(asm, "")?;

    writeln!(asm, ".const .align 8 .b8 params[{}];", 8 * n_params)?;
    writeln!(asm, ".entry {}(){{", entry_point)?;
    writeln!(asm, "")?;

    writeln!(
        asm,
        "\t.reg.b8   %b <{n_regs}>; .reg.b16 %w<{n_regs}>; .reg.b32 %r<{n_regs}>;"
    )?;
    writeln!(
        asm,
        "\t.reg.b64  %rd<{n_regs}>; .reg.f32 %f<{n_regs}>; .reg.f64 %d<{n_regs}>;"
    )?;
    writeln!(asm, "\t.reg.pred %p <{n_regs}>;")?;
    writeln!(asm, "")?;

    write!(
        asm,
        "\tcall (%r0), _optix_get_launch_index_x, ();\n\
            \tld.const.u32 %r1, [params + 4];\n\
            \tadd.u32 %r0, %r0, %r1;\n\n\
            body:\n"
    )?;

    for id in ir.ids() {
        let var = ir.var(id);
        match var.param_ty {
            ParamType::None => crate::backend::cuda::codegen::assemble_var(
                asm,
                ir,
                id,
                1,
                1 + env.buffers().len(),
                "const",
            )?,
            ParamType::Input => {
                let param_offset = (var.buf.unwrap() + 1) * 8;
                // Load from params
                writeln!(asm, "")?;
                writeln!(asm, "\t// [{}]: {:?} =>", id, var)?;
                if var.is_literal() {
                    writeln!(
                        asm,
                        "\tld.param.{} {}, [params+{}];",
                        var.ty.name_cuda(),
                        var.reg(),
                        param_offset
                    )?;
                    continue;
                } else {
                    writeln!(asm, "\tld.const.u64 %rd0, [params+{}];", param_offset)?;
                }
                if var.size > 1 {
                    writeln!(asm, "\tmad.wide.u32 %rd0, %r0, {}, %rd0;", var.ty.size())?;
                }

                if var.ty == VarType::Bool {
                    writeln!(asm, "\tld.global.cs.u8 %w0, [%rd0];")?;
                    writeln!(asm, "\tsetp.ne.u16 {}, %w0, 0;", var.reg())?;
                } else {
                    writeln!(
                        asm,
                        "\tld.global.cs.{} {}, [%rd0];",
                        var.ty.name_cuda(),
                        var.reg(),
                    )?;
                }
            }
            ParamType::Output => {
                let param_offset = (var.buf.unwrap() + 1) * 8;
                crate::backend::cuda::codegen::assemble_var(
                    asm,
                    ir,
                    id,
                    1,
                    1 + env.buffers().len(),
                    "const",
                )?;
                // let offset = param_idx * 8;
                write!(
                    asm,
                    "\n\t// Store:\n\
                           \tld.const.u64 %rd0, [params + {}]; // rd0 <- params[offset]\n\
                           \tmad.wide.u32 %rd0, %r0, {}, %rd0; // rd0 <- Index * ty.size() + \
                           params[offset]\n",
                    param_offset,
                    var.ty.size(),
                )?;

                if var.ty == VarType::Bool {
                    writeln!(asm, "\tselp.u16 %w0, 1, 0, {};", var.reg())?;
                    writeln!(asm, "\tst.global.cs.u8 [%rd0], %w0;")?;
                } else {
                    writeln!(
                               asm,
                               "\tst.global.cs.{} [%rd0], {}; // (Index * ty.size() + params[offset])[Index] <- var",
                               var.ty.name_cuda(),
                               var.reg(),
                           )?;
                }
            }
        }
    }

    write!(
        asm,
        "\n\tret;\n\
       }}\n"
    )?;
    Ok(())
}