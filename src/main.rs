use std::sync::Arc;

use crate::trace::Trace;
use crate::var::ReduceOp;

use self::backend::optix::optix_core;
use self::jit::Jit;

mod backend;
mod jit;
mod schedule;
mod trace;
mod var;

fn main() {
    pretty_env_logger::init();

    let ir = Trace::default();
    ir.set_backend("optix");

    let x = ir.buffer_u32(&[1, 2, 3]);
    // let i = ir.index(3);
    let x = x.add(&x);
    // let y = x.add(&x);

    ir.schedule(&[&x]);
    let mut jit = Jit::default();
    jit.eval(&mut ir.borrow_mut());

    assert_eq!(x.to_host_u32(), vec![2, 4, 6]);

    // // let backend = backend::optix::optix::Backend::new().unwrap();
    // let instance = Arc::new(optix_core::Instance::new().unwrap());
    // let device = optix_core::Device::create(&instance, 0).unwrap();
    //
    // let miss_minimal = ".version 6.0 .target sm_50 .address_size 64 \
    //                      .entry __miss__dr() { ret; }";
    //
    // let mco = optix_rs::OptixModuleCompileOptions {
    //     optLevel: optix_rs::OptixCompileOptimizationLevel::OPTIX_COMPILE_OPTIMIZATION_LEVEL_3,
    //     debugLevel: optix_rs::OptixCompileDebugLevel::OPTIX_COMPILE_DEBUG_LEVEL_NONE,
    //     ..Default::default()
    // };
    // let pco = optix_rs::OptixPipelineCompileOptions {
    //     numAttributeValues: 2,
    //     pipelineLaunchParamsVariableName: b"params" as *const _ as *const _,
    //     exceptionFlags: optix_rs::OptixExceptionFlags::OPTIX_EXCEPTION_FLAG_NONE as _,
    //     ..Default::default()
    // };
    // let miss = optix_core::Module::create(&device, miss_minimal, mco, pco).unwrap();
    //
    // let miss_group = optix_core::ProgramGroup::create(
    //     &device,
    //     optix_core::ProgramGroupDesc::Miss {
    //         module: &miss,
    //         entry_point: "__miss__dr",
    //     },
    // )
    // .unwrap();
    //
    // drop(miss);
    // drop(miss_group);
}
