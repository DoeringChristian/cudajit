use std::ffi::{CStr, CString};
use std::fmt::{Debug, Write};
use std::sync::{Arc, Mutex};

use crate::backend::cuda::cuda_core::{Event, Stream};
use crate::backend::cuda::{cuda_core, Buffer, Texture};
use crate::backend::{self, CompileOptions};
use crate::schedule::{Env, SVarId, ScheduleIr};
use crate::trace::VarType;
use crate::var::ParamType;
use optix_rs::{
    OptixAccelBufferSizes, OptixAccelBuildOptions, OptixAccelEmitDesc, OptixApi, OptixBuildFlags,
    OptixBuildInput, OptixBuildInputInstanceArray, OptixBuildInputTriangleArray,
    OptixBuildInputType, OptixBuildInput__bindgen_ty_1, OptixBuildOperation, OptixDeviceContext,
    OptixDeviceContextOptions, OptixExceptionFlags, OptixGeometryFlags, OptixIndicesFormat,
    OptixInstance, OptixInstanceFlags, OptixModuleCompileOptions, OptixPipelineCompileOptions,
    OptixProgramGroup, OptixProgramGroupDesc, OptixProgramGroupOptions, OptixTraversableHandle,
    OptixVertexFormat,
};
use thiserror::Error;

use super::optix_core::{self, Device, Module, Pipeline, ProgramGroup};

#[derive(Debug, Error)]
pub enum Error {
    #[error("{}", .0)]
    OptixError(#[from] optix_core::Error),
}

pub struct Backend {
    instance: Arc<optix_core::Instance>,
    device: optix_core::Device,
    stream: Arc<cuda_core::Stream>,
    pub compile_options: backend::CompileOptions,
    pub miss: Option<(String, Arc<Module>)>,
    pub hit: Vec<(String, Arc<Module>)>,
}
impl Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend").finish()
    }
}

impl Backend {
    pub fn set_hit_from_strs(&mut self, hit: &[(&str, &str)]) {
        self.hit.extend(hit.iter().map(|(ep, ptx)| {
            (
                String::from(*ep),
                Arc::new(
                    Module::create(
                        &self.device,
                        ptx,
                        self.compile_options.mco(),
                        self.compile_options.pco(),
                    )
                    .unwrap(),
                ),
            )
        }));
    }
    pub fn set_miss_from_str(&mut self, miss: (&str, &str)) {
        self.miss = Some((
            String::from(miss.0),
            Arc::new(
                Module::create(
                    &self.device,
                    miss.1,
                    self.compile_options.mco(),
                    self.compile_options.pco(),
                )
                .unwrap(),
            ),
        ));
    }
    pub fn new() -> Result<Self, Error> {
        let instance = Arc::new(optix_core::Instance::new()?);
        let device = optix_core::Device::create(&instance, 0)?;
        let stream = Arc::new(
            device
                .cuda_device()
                .create_stream(cuda_rs::CUstream_flags_enum::CU_STREAM_DEFAULT)
                .unwrap(),
        );

        let miss_minimal = ".version 6.0 .target sm_50 .address_size 64\n\
                                    .entry __miss__dr() { ret; }";

        let compile_options = backend::CompileOptions::default();

        let miss = Arc::new(
            Module::create(
                &device,
                &miss_minimal,
                compile_options.mco(),
                compile_options.pco(),
            )
            .unwrap(),
        );
        Ok(Self {
            device,
            instance,
            stream,
            hit: vec![],
            miss: Some(("__miss__dr".into(), miss)),
            compile_options,
        })
    }
}

unsafe impl Sync for Backend {}
unsafe impl Send for Backend {}
impl backend::Backend for Backend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn create_texture(&self, shape: &[usize], n_channels: usize) -> Arc<dyn backend::Texture> {
        Arc::new(Texture::create(
            self.device.cuda_device(),
            shape,
            n_channels,
        ))
    }

    fn buffer_uninit(&self, size: usize) -> Arc<dyn backend::Buffer> {
        Arc::new(Buffer::uninit(self.device.cuda_device(), size))
    }

    fn buffer_from_slice(&self, slice: &[u8]) -> Arc<dyn backend::Buffer> {
        Arc::new(Buffer::from_slice(self.device.cuda_device(), slice))
    }

    fn first_register(&self) -> usize {
        Kernel::FIRST_REGISTER
    }

    fn synchronize(&self) {
        self.stream.synchronize().unwrap();
    }

    fn create_accel(&self, desc: backend::AccelDesc) -> Arc<dyn backend::Accel> {
        Arc::new(Accel::create(&self.device, &self.stream, desc).unwrap())
    }

    fn set_compile_options(&mut self, compile_options: &backend::CompileOptions) {
        self.hit.clear();
        self.compile_options = compile_options.clone();
    }

    fn set_miss_from_str(&mut self, entry_point: &str, source: &str) {
        self.miss = Some((
            String::from(entry_point),
            Arc::new(
                Module::create(
                    &self.device,
                    source,
                    self.compile_options.mco(),
                    self.compile_options.pco(),
                )
                .unwrap(),
            ),
        ));
    }

    fn push_hit_from_str(&mut self, entry_point: &str, source: &str) {
        self.hit.push((
            String::from(entry_point),
            Arc::new(
                Module::create(
                    &self.device,
                    source,
                    self.compile_options.mco(),
                    self.compile_options.pco(),
                )
                .unwrap(),
            ),
        ))
    }

    fn compile_kernel(&self, ir: &ScheduleIr, env: &Env) -> Box<dyn backend::Kernel> {
        if env.accels().is_empty() {
            Box::new(crate::backend::cuda::Kernel::compile(
                self.device.cuda_device(),
                &self.stream,
                ir,
                env,
            ))
        } else {
            Box::new(Kernel::compile(
                &self.device,
                &self.stream,
                &self.compile_options,
                self.miss.as_ref().unwrap(),
                &self.hit,
                ir,
                env,
            ))
        }
    }
}

pub struct Kernel {
    device: Device,
    pub asm: String,
    // entry_point: String,
    pipeline: optix_core::Pipeline,
    stream: Arc<cuda_core::Stream>,
    // compile_options: backend::CompileOptions,
    // miss: (String, Arc<Module>),
    // hit: Vec<(String, Arc<Module>)>,
}
impl Kernel {
    const FIRST_REGISTER: usize = 4;
}
impl Debug for Kernel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kernel")
            .field("device", &self.device)
            // .field("asm", &self.asm)
            // .field("kernel_name", &self.entry_point)
            .finish()
    }
}

impl Kernel {
    pub fn compile(
        device: &Device,
        stream: &Arc<Stream>,
        compile_options: &CompileOptions,
        miss: &(String, Arc<Module>),
        hit: &[(String, Arc<Module>)],
        ir: &ScheduleIr,
        env: &Env,
    ) -> Self {
        let entry_point = "__raygen__cujit";
        // Assemble
        let mut asm = String::new();

        super::codegen::assemble_entry(&mut asm, ir, env, entry_point).unwrap();

        std::fs::write("/tmp/optix.ptx", &asm).unwrap();

        log::trace!("{}", asm);

        // Compile

        // let compile_options = self.compile_options.clone();

        let rgen = Arc::new(
            optix_core::Module::create(device, &asm, compile_options.mco(), compile_options.pco())
                .unwrap(),
        );
        let rgen_pg = optix_core::ProgramGroup::create(
            &device,
            optix_core::ProgramGroupDesc::RayGen {
                module: &rgen,
                entry_point,
            },
        )
        .unwrap();
        let miss_pg = optix_core::ProgramGroup::create(
            &device,
            optix_core::ProgramGroupDesc::Miss {
                entry_point: &miss.0,
                module: &miss.1,
            },
        )
        .unwrap();

        let hit_pgs = hit
            .iter()
            .map(|(ep, ptx)| {
                let pg = optix_core::ProgramGroup::create(
                    &device,
                    optix_core::ProgramGroupDesc::HitGroup {
                        module_ch: Some(ptx),
                        entry_point_ch: Some(ep),
                        module_ah: None,
                        entry_point_ah: None,
                        module_is: None,
                        entry_point_is: None,
                    },
                )
                .unwrap();
                pg
            })
            .collect::<Vec<_>>();

        let pipeline = optix_core::Pipeline::create(
            &device,
            &compile_options.pco(),
            &optix_rs::OptixPipelineLinkOptions {
                maxTraceDepth: 1,
                debugLevel: optix_rs::OptixCompileDebugLevel::OPTIX_COMPILE_DEBUG_LEVEL_NONE,
            },
            rgen_pg,
            hit_pgs,
            [miss_pg],
        )
        .unwrap();
        Self {
            pipeline,
            device: device.clone(),
            stream: stream.clone(),
            asm,
        }
    }
}

unsafe impl Sync for Kernel {}
unsafe impl Send for Kernel {}
impl backend::Kernel for Kernel {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn execute_async(
        &mut self,
        env: &mut crate::schedule::Env,
        size: usize,
    ) -> Arc<dyn backend::DeviceFuture> {
        let params = [size as u32, 0u32];
        let mut params = Vec::from(bytemuck::cast_slice(&params));
        params.extend(
            env.buffers()
                .iter()
                .map(|b| b.as_any().downcast_ref::<Buffer>().unwrap().ptr()),
        );
        params.extend(
            env.textures()
                .iter()
                .map(|b| b.as_any().downcast_ref::<Texture>().unwrap().ptr()),
        );
        params.extend(
            env.accels()
                .iter()
                .map(|a| a.as_any().downcast_ref::<Accel>().unwrap().ptr()),
        );

        log::trace!("params: {:02x?}", bytemuck::cast_slice::<_, u8>(&params));
        log::trace!("Optix Kernel Launch with {size} threads.");

        dbg!(&params);

        let params_buf =
            Buffer::from_slice(&self.device.cuda_device(), bytemuck::cast_slice(&params));
        // let params = Buffer::uninit(&self.device.cuda_device(), 8 * params.len());

        unsafe {
            // let mut d_params = 0;
            // let ctx = self.device.cuda_ctx();
            // ctx.cuMemAlloc_v2(&mut d_params, 8 * params.len())
            //     .check()
            //     .unwrap();
            // ctx.cuMemcpyHtoD_v2(d_params, params.as_ptr() as *const _, params.len() * 8)
            //     .check()
            //     .unwrap(); // TODO: Free somehow...

            self.pipeline
                // .as_ref()
                // .unwrap()
                .launch(
                    &self.stream,
                    params_buf.ptr(),
                    params_buf.size(),
                    size as u32,
                )
                .unwrap();
            // self.stream.synchronize().unwrap();

            // ctx.cuMemFree_v2(d_params).check().unwrap();
        }

        let mut event = Event::create(&self.device.cuda_device()).unwrap();
        event.record(&self.stream).unwrap();
        Arc::new(DeviceFuture {
            event,
            params: params_buf,
        })
    }

    fn assembly(&self) -> &str {
        &self.asm
    }
}

unsafe impl Sync for Accel {}
unsafe impl Send for Accel {}

#[derive(Debug)]
pub struct Accel {
    device: Device,
    tlas: (u64, Buffer),
    blaccels: Vec<(u64, Buffer)>,
    buffers: Vec<Arc<dyn backend::Buffer>>, // Keep buffers arround
}

impl Accel {
    pub fn ptr(&self) -> u64 {
        self.tlas.0
    }
    pub fn create(
        device: &Device,
        stream: &Stream,
        desc: backend::AccelDesc,
    ) -> Result<Self, Error> {
        let build_options = OptixAccelBuildOptions {
            // buildFlags: OptixBuildFlags::OPTIX_BUILD_FLAG_NONE as u32,
            buildFlags: OptixBuildFlags::OPTIX_BUILD_FLAG_PREFER_FAST_TRACE as u32
                | OptixBuildFlags::OPTIX_BUILD_FLAG_ALLOW_COMPACTION as u32,
            operation: OptixBuildOperation::OPTIX_BUILD_OPERATION_BUILD,
            ..Default::default()
        };

        let build_accel = |build_inputs: &[OptixBuildInput]| {
            let mut buffer_size = OptixAccelBufferSizes::default();
            unsafe {
                device
                    .api()
                    .optixAccelComputeMemoryUsage(
                        *device.ctx(),
                        &build_options,
                        build_inputs.as_ptr(),
                        build_inputs.len() as _,
                        &mut buffer_size,
                    )
                    .check()
                    .unwrap()
            };
            let gas_tmp = Buffer::uninit(device.cuda_device(), buffer_size.tempSizeInBytes);
            let gas = Buffer::uninit(device.cuda_device(), buffer_size.outputSizeInBytes);
            dbg!(gas.ptr());
            let mut accel = 0;
            unsafe {
                device
                    .api()
                    .optixAccelBuild(
                        *device.ctx(),
                        stream.raw(),
                        &build_options,
                        build_inputs.as_ptr(),
                        build_inputs.len() as _,
                        gas_tmp.ptr(),
                        buffer_size.tempSizeInBytes,
                        gas.ptr(),
                        buffer_size.outputSizeInBytes,
                        &mut accel,
                        std::ptr::null(),
                        0,
                    )
                    .check()
                    .unwrap();
                // TODO: Async construction needs to keep gas_tmp and gas buffers arround
                stream.synchronize().unwrap();
                (accel, gas)
            }
        };

        let mut blaccels = vec![];
        let mut buffers: Vec<Arc<dyn backend::Buffer>> = vec![];
        for geometry in desc.geometries.iter() {
            let build_inputs = match geometry {
                backend::GeometryDesc::Triangles { vertices, indices } => {
                    buffers.push((*vertices).clone()); // Keep references to buffers
                    buffers.push((*indices).clone());

                    let vertex_buffer = vertices.as_any().downcast_ref::<Buffer>().unwrap();
                    let indices_buffer = indices.as_any().downcast_ref::<Buffer>().unwrap();

                    let flags = [OptixGeometryFlags::OPTIX_GEOMETRY_FLAG_DISABLE_ANYHIT];
                    let triangle_array = OptixBuildInputTriangleArray {
                        vertexBuffers: &vertex_buffer.ptr(),
                        numVertices: (vertex_buffer.size() / (4 * 3)) as _,
                        vertexFormat: OptixVertexFormat::OPTIX_VERTEX_FORMAT_FLOAT3,
                        // vertexStrideInBytes: 0,
                        indexBuffer: indices_buffer.ptr(),
                        indexFormat: OptixIndicesFormat::OPTIX_INDICES_FORMAT_UNSIGNED_INT3,
                        numIndexTriplets: (indices_buffer.size() / (4 * 3)) as _,
                        flags: &flags as *const _ as *const _,
                        numSbtRecords: 1,
                        ..Default::default()
                    };
                    let mut build_input = OptixBuildInput {
                        type_: OptixBuildInputType::OPTIX_BUILD_INPUT_TYPE_TRIANGLES,
                        __bindgen_anon_1: OptixBuildInput__bindgen_ty_1::default(),
                    };
                    unsafe {
                        *build_input.__bindgen_anon_1.triangleArray.as_mut() = triangle_array;
                    }
                    vec![build_input]
                }
            };

            blaccels.push(build_accel(&build_inputs));
        }

        let instances = desc
            .instances
            .iter()
            .enumerate()
            .map(|(i, inst)| OptixInstance {
                transform: inst.transform,
                instanceId: i as _,
                sbtOffset: 0,
                visibilityMask: 255,
                flags: OptixInstanceFlags::OPTIX_INSTANCE_FLAG_DISABLE_TRIANGLE_FACE_CULLING as _,
                traversableHandle: blaccels[inst.geometry].0,
                ..Default::default()
            })
            .collect::<Vec<_>>();

        dbg!(&instances);

        let instance_buf = unsafe {
            Buffer::from_slice(
                device.cuda_device(),
                std::slice::from_raw_parts(
                    instances.as_ptr() as *const _,
                    std::mem::size_of::<OptixInstance>() * instances.len(),
                ),
            )
        };

        let mut build_input = OptixBuildInput {
            type_: OptixBuildInputType::OPTIX_BUILD_INPUT_TYPE_INSTANCES,
            __bindgen_anon_1: Default::default(),
        };
        unsafe {
            *build_input.__bindgen_anon_1.instanceArray.as_mut() = OptixBuildInputInstanceArray {
                instances: instance_buf.ptr(),
                numInstances: instances.len() as _,
            }
        };

        let tlas = build_accel(&[build_input]);

        Ok(Self {
            device: device.clone(),
            blaccels,
            tlas,
            buffers,
        })
    }
}

impl backend::Accel for Accel {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl backend::CompileOptions {
    pub fn pco(&self) -> OptixPipelineCompileOptions {
        OptixPipelineCompileOptions {
            numAttributeValues: 2,
            pipelineLaunchParamsVariableName: b"params\0" as *const _ as *const _,
            exceptionFlags: optix_rs::OptixExceptionFlags::OPTIX_EXCEPTION_FLAG_NONE as _,
            numPayloadValues: self.num_payload_values,
            ..Default::default()
        }
    }
    pub fn mco(&self) -> OptixModuleCompileOptions {
        OptixModuleCompileOptions {
            optLevel: optix_rs::OptixCompileOptimizationLevel::OPTIX_COMPILE_OPTIMIZATION_LEVEL_0,
            debugLevel: optix_rs::OptixCompileDebugLevel::OPTIX_COMPILE_DEBUG_LEVEL_NONE,
            ..Default::default()
        }
    }
}

unsafe impl Sync for DeviceFuture {}
unsafe impl Send for DeviceFuture {}
#[derive(Debug)]
pub struct DeviceFuture {
    event: Event,
    params: Buffer,
}

impl backend::DeviceFuture for DeviceFuture {
    fn wait(&self) {
        self.event.synchronize();
    }
}

impl Drop for DeviceFuture {
    fn drop(&mut self) {
        self.event.synchronize();
    }
}
