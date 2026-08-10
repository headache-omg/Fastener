#[cfg(feature = "gpu")]
use anyhow::Context;
use anyhow::{Result, bail};

const ANALYSIS_BLOCK: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnalysisBackend {
    Cpu,
    Hybrid(String),
}

impl std::fmt::Display for AnalysisBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cpu => write!(f, "CPU自動境界解析"),
            Self::Hybrid(name) => write!(f, "CPU+GPU複合境界解析 ({name})"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnalysisReport {
    pub backend: AnalysisBackend,
    pub boundaries: Vec<usize>,
    pub analysis_blocks: usize,
}

pub fn analyze(data: &[u8], target: usize, _total_input_size: usize) -> Result<AnalysisReport> {
    if target < ANALYSIS_BLOCK * 2 {
        bail!(
            "target chunk size must be at least {} bytes",
            ANALYSIS_BLOCK * 2
        );
    }

    if data.is_empty() {
        return Ok(AnalysisReport {
            backend: AnalysisBackend::Cpu,
            boundaries: vec![0],
            analysis_blocks: 0,
        });
    }

    #[cfg(feature = "gpu")]
    {
        match hybrid_scores(data) {
            HybridScores::Cpu(scores) => Ok(AnalysisReport {
                backend: AnalysisBackend::Cpu,
                boundaries: select_boundaries(data.len(), target, &scores),
                analysis_blocks: scores.len(),
            }),
            HybridScores::CpuAndGpu(scores, name) => Ok(AnalysisReport {
                backend: AnalysisBackend::Hybrid(name),
                boundaries: select_boundaries(data.len(), target, &scores),
                analysis_blocks: scores.len(),
            }),
        }
    }

    #[cfg(not(feature = "gpu"))]
    {
        let scores = cpu_scores(data);
        Ok(AnalysisReport {
            backend: AnalysisBackend::Cpu,
            boundaries: select_boundaries(data.len(), target, &scores),
            analysis_blocks: scores.len(),
        })
    }
}

#[cfg(feature = "gpu")]
enum HybridScores {
    Cpu(Vec<u32>),
    CpuAndGpu(Vec<u32>, String),
}

#[cfg(feature = "gpu")]
fn hybrid_scores(data: &[u8]) -> HybridScores {
    let (cpu, gpu) = std::thread::scope(|scope| {
        let cpu_worker = scope.spawn(|| cpu_scores_with_sample_stride(data, 0, 2));
        let gpu = gpu_scores_odd_samples(data);
        let cpu = cpu_worker
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (cpu, gpu)
    });

    let Ok((gpu, name)) = gpu else {
        return HybridScores::Cpu(cpu_scores(data));
    };
    let scores = cpu
        .into_iter()
        .zip(gpu)
        .map(|(cpu, gpu)| cpu + gpu)
        .collect();
    HybridScores::CpuAndGpu(scores, name)
}

fn cpu_scores(data: &[u8]) -> Vec<u32> {
    cpu_scores_with_sample_stride(data, 0, 1)
}

fn cpu_scores_with_sample_stride(data: &[u8], first_sample: usize, stride: usize) -> Vec<u32> {
    debug_assert!(stride > 0);
    let block_count = data.len().div_ceil(ANALYSIS_BLOCK);
    (0..block_count)
        .map(|block| {
            if block == 0 {
                return 0;
            }
            let current = block * ANALYSIS_BLOCK;
            let previous = current - ANALYSIS_BLOCK;
            let available = (data.len() - current).min(ANALYSIS_BLOCK);
            let samples = available.div_ceil(16).min(256);
            let mut score = 0u32;
            for sample in (first_sample..samples).step_by(stride) {
                let offset = sample * 16;
                let now = data[current + offset];
                let before = data[previous + offset];
                score += now.abs_diff(before) as u32;
                if offset >= 16 {
                    score += (now != data[current + offset - 16]) as u32 * 16;
                }
            }
            score
        })
        .collect()
}

fn select_boundaries(len: usize, target: usize, scores: &[u32]) -> Vec<usize> {
    if len == 0 {
        return vec![0];
    }
    let min_size = (target / 2).max(ANALYSIS_BLOCK);
    let max_size = target.saturating_mul(2).max(min_size);
    let search_radius = (target / 4).max(ANALYSIS_BLOCK);
    let mut boundaries = vec![0];
    let mut start = 0usize;

    while len - start > max_size {
        let ideal = start.saturating_add(target);
        let low = start
            .saturating_add(min_size)
            .max(ideal.saturating_sub(search_radius));
        let high = start
            .saturating_add(max_size)
            .min(ideal.saturating_add(search_radius))
            .min(len);
        let low_block = low.div_ceil(ANALYSIS_BLOCK);
        let high_block = high / ANALYSIS_BLOCK;

        let best_block = (low_block..=high_block)
            .filter(|&block| block < scores.len())
            .max_by_key(|&block| scores[block])
            .unwrap_or_else(|| ideal.div_ceil(ANALYSIS_BLOCK));
        let boundary = (best_block * ANALYSIS_BLOCK).clamp(start + min_size, len);
        boundaries.push(boundary);
        start = boundary;
    }

    if *boundaries.last().unwrap() != len {
        boundaries.push(len);
    }
    boundaries
}

#[cfg(feature = "gpu")]
struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    name: String,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

#[cfg(feature = "gpu")]
fn gpu_context() -> Result<&'static GpuContext> {
    static CONTEXT: std::sync::OnceLock<Result<GpuContext, String>> = std::sync::OnceLock::new();
    CONTEXT
        .get_or_init(|| initialize_gpu_context().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))
}

#[cfg(feature = "gpu")]
fn initialize_gpu_context() -> Result<GpuContext> {
    const SHADER: &str = r#"
@group(0) @binding(0) var<storage, read> input_words: array<u32>;
@group(0) @binding(1) var<storage, read_write> scores: array<u32>;

fn byte_at(index: u32) -> u32 {
    let word = input_words[index / 4u];
    return (word >> ((index % 4u) * 8u)) & 255u;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let block = gid.x;
    if (block >= arrayLength(&scores)) { return; }
    if (block == 0u) { scores[0] = 0u; return; }
    let current = block * 4096u;
    let previous = current - 4096u;
    let byte_length = arrayLength(&input_words) * 4u;
    var score = 0u;
    var sample = 1u;
    loop {
        if (sample >= 256u) { break; }
        let offset = sample * 16u;
        if (current + offset >= byte_length) { break; }
        let now = byte_at(current + offset);
        let before = byte_at(previous + offset);
        score += select(before - now, now - before, now >= before);
        if (sample > 0u && now != byte_at(current + offset - 16u)) {
            score += 16u;
        }
        sample += 2u;
    }
    scores[block] = score;
}
"#;

    pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("no compatible GPU adapter was found")?;
        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Fastener automatic analysis device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                ..Default::default()
            })
            .await
            .context("could not create a GPU device")?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Fastener boundary shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Fastener analysis layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Fastener pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Fastener analysis pipeline"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Ok(GpuContext {
            device,
            queue,
            name: info.name,
            layout,
            pipeline,
        })
    })
}

#[cfg(feature = "gpu")]
fn gpu_scores_odd_samples(data: &[u8]) -> Result<(Vec<u32>, String)> {
    use wgpu::util::DeviceExt;

    let mut words = vec![0u32; data.len().div_ceil(4)];
    for (index, byte) in data.iter().enumerate() {
        words[index / 4] |= (*byte as u32) << ((index % 4) * 8);
    }
    let block_count = data.len().div_ceil(ANALYSIS_BLOCK);
    let output_size = (block_count * std::mem::size_of::<u32>()) as u64;
    let context = gpu_context()?;
    let input = context
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Fastener input"),
            contents: bytemuck::cast_slice(&words),
            usage: wgpu::BufferUsages::STORAGE,
        });
    let output = context.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Fastener scores"),
        size: output_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = context.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Fastener score readback"),
        size: output_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = context
        .device
        .create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Fastener analysis bind group"),
            layout: &context.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
            ],
        });
    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Fastener analysis encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("Fastener analysis pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&context.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups((block_count as u32).div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, output_size);
    context.queue.submit(Some(encoder.finish()));

    let slice = staging.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    context.device.poll(wgpu::PollType::wait_indefinitely())?;
    receiver
        .recv()
        .context("GPU readback callback was lost")??;
    let mapped = slice
        .get_mapped_range()
        .context("could not access GPU readback memory")?;
    let scores = bytemuck::cast_slice::<u8, u32>(&mapped).to_vec();
    drop(mapped);
    staging.unmap();
    Ok((scores, context.name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundaries_cover_input_and_stay_ordered() {
        let data = vec![b'a'; 5 * 1024 * 1024];
        let report = analyze(&data, 1024 * 1024, data.len()).unwrap();
        assert!(matches!(
            report.backend,
            AnalysisBackend::Cpu | AnalysisBackend::Hybrid(_)
        ));
        assert_eq!(report.boundaries.first(), Some(&0));
        assert_eq!(report.boundaries.last(), Some(&data.len()));
        assert!(report.boundaries.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn split_cpu_samples_recombine_to_full_score() {
        let data: Vec<u8> = (0..128 * 1024)
            .map(|index| ((index * 29 + index / 97) % 251) as u8)
            .collect();
        let expected = cpu_scores(&data);
        let even = cpu_scores_with_sample_stride(&data, 0, 2);
        let odd = cpu_scores_with_sample_stride(&data, 1, 2);
        let combined: Vec<_> = even
            .into_iter()
            .zip(odd)
            .map(|(even, odd)| even + odd)
            .collect();
        assert_eq!(combined, expected);
    }

    #[cfg(feature = "gpu")]
    #[test]
    fn hybrid_gpu_score_matches_full_cpu_score_when_available() {
        let data: Vec<u8> = (0..128 * 1024)
            .map(|index| ((index * 31 + index / 89) % 253) as u8)
            .collect();
        if let HybridScores::CpuAndGpu(scores, _) = hybrid_scores(&data) {
            assert_eq!(scores, cpu_scores(&data));
        }
    }
}
