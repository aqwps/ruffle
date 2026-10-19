//! GPU regression probe: compare eager/deferred layer images and checkout peaks.
//! Run with `RUFFLE_GPU_EAGER_LAYERS=1` for the old scheduler.
//! Arguments: layer-count width height frames output-directory-or-"-" mode.
//! Modes: layers, masks, fallback, mixed, dual-pool.
//! `compare before after frames` checks images.
//! Elapsed time includes startup/warm-up and optional readback; it is not game FPS.

#[cfg(not(target_family = "wasm"))]
fn main() {
    use ruffle_render::backend::RenderBackend;
    use ruffle_render::commands::{CommandHandler, CommandList, RenderBlendMode};
    use ruffle_render::matrix::Matrix;
    use ruffle_render::quality::StageQuality;
    use ruffle_render_wgpu::backend::WgpuRenderBackend;
    use ruffle_render_wgpu::wgpu;
    use std::time::{Duration, Instant};
    use swf::{BlendMode, Color, Twips};

    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "compare") {
        for frame in 0..args[4].parse::<usize>().unwrap() {
            let a = image::open(format!("{}/{frame:03}.png", args[2]))
                .unwrap()
                .into_rgba8();
            let b = image::open(format!("{}/{frame:03}.png", args[3]))
                .unwrap()
                .into_rgba8();
            assert_eq!(a.dimensions(), b.dimensions());
            let changed = a.pixels().zip(b.pixels()).filter(|(a, b)| a != b).count();
            println!("frame={frame} changed_pixels={changed}");
            assert_eq!(changed, 0);
        }
        return;
    }
    let count = args.get(1).map_or(16, |v| v.parse::<usize>().unwrap());
    let width = args.get(2).map_or(640, |v| v.parse::<u32>().unwrap());
    let height = args.get(3).map_or(400, |v| v.parse::<u32>().unwrap());
    let frames = args.get(4).map_or(10, |v| v.parse::<usize>().unwrap());
    let output = args.get(5).map(String::as_str).unwrap_or("-");
    let mode = args.get(6).map(String::as_str).unwrap_or("layers");
    let mut renderer = WgpuRenderBackend::for_offscreen(
        (width, height),
        wgpu::Backends::VULKAN,
        wgpu::PowerPreference::HighPerformance,
    )
    .unwrap();
    renderer.set_quality(StageQuality::High);
    println!("{}", renderer.debug_info());
    if output != "-" {
        std::fs::create_dir_all(output).unwrap();
    }
    let start = Instant::now();
    let offscreen = (mode == "dual-pool").then(|| {
        renderer
            .create_empty_texture(
                std::num::NonZeroU32::new((width / 2).max(1)).unwrap(),
                std::num::NonZeroU32::new((height / 2).max(1)).unwrap(),
            )
            .unwrap()
    });
    for frame in 0..frames {
        if let Some(handle) = &offscreen {
            let mut draws = CommandList::new();
            draws.draw_rect(Color::WHITE, Matrix::scale(50.0, 50.0));
            drop(renderer.render_offscreen(
                handle.clone(),
                draws,
                StageQuality::High,
                ruffle_render::bitmap::PixelRegion::for_whole_size(
                    (width / 2).max(1),
                    (height / 2).max(1),
                ),
            ));
        }
        let mut commands = CommandList::new();
        commands.draw_rect(
            Color {
                r: 40,
                g: 70,
                b: 95,
                a: 255,
            },
            Matrix::scale(width as f32, height as f32),
        );
        for layer in 0..count {
            let mut child = CommandList::new();
            let matrix = Matrix::create_box_with_rotation(
                70.0 + (layer % 3) as f32 * 7.0,
                65.0,
                (frame as f32 * 0.03) + layer as f32 * 0.02,
                Twips::from_pixels((layer * 31 % width as usize) as f64 - 10.0),
                Twips::from_pixels((layer * 23 % height as usize) as f64 + 5.0),
            );
            child.draw_rect(
                Color {
                    r: (layer * 37 % 255) as u8,
                    g: 150,
                    b: 210,
                    a: 180,
                },
                matrix,
            );
            if mode == "masks" {
                let mut mask = CommandList::new();
                mask.draw_rect(Color::WHITE, matrix * Matrix::scale(0.8, 0.75));
                commands.render_alpha_mask(child, mask);
            } else {
                if matches!(mode, "fallback" | "dual-pool") || (mode == "mixed" && layer % 3 == 0) {
                    let mut alpha = CommandList::new();
                    alpha.draw_rect(
                        Color {
                            r: 255,
                            g: 255,
                            b: 255,
                            a: 100,
                        },
                        matrix,
                    );
                    child.blend(alpha, RenderBlendMode::Builtin(BlendMode::Alpha));
                }
                let modes = [
                    BlendMode::Layer,
                    BlendMode::Add,
                    BlendMode::Screen,
                    BlendMode::Multiply,
                    BlendMode::Overlay,
                ];
                let blend = if matches!(mode, "fallback" | "mixed" | "dual-pool") {
                    BlendMode::Layer
                } else {
                    modes[layer % modes.len()]
                };
                commands.blend(child, RenderBlendMode::Builtin(blend));
            }
        }
        renderer.submit_frame(Color::BLACK, commands, vec![]);
        let (main, offscreen) = renderer.scratch_texture_stats();
        assert!(main.peak_checked_out_bytes >= u64::from(width) * u64::from(height) * 4);
        let budget = renderer.scratch_texture_budget();
        assert!(
            main.owned_bytes.saturating_sub(main.checked_out_bytes)
                + offscreen
                    .owned_bytes
                    .saturating_sub(offscreen.checked_out_bytes)
                <= budget,
            "Idle textures exceeded the shared budget"
        );
        println!(
            "frame={frame} peak_bytes={} owned_bytes={} created={} created_bytes={} deferred={} fallback={}",
            main.peak_checked_out_bytes,
            main.owned_bytes,
            main.created_textures + offscreen.created_textures,
            main.created_bytes + offscreen.created_bytes,
            main.deferred_layers,
            main.fallback_layers
        );
        if mode == "dual-pool" {
            println!(
                "offscreen_peak={} offscreen_owned={} offscreen_created={}",
                offscreen.peak_checked_out_bytes, offscreen.owned_bytes, offscreen.created_textures,
            );
            if frame > 1 && budget / 2 >= offscreen.peak_checked_out_bytes {
                assert_eq!(
                    offscreen.created_textures, 0,
                    "Main pool starved offscreen reuse"
                );
            }
        }
        if matches!(mode, "layers" | "masks")
            && std::env::var_os("RUFFLE_GPU_EAGER_LAYERS").is_none()
        {
            assert!(
                main.peak_checked_out_bytes <= u64::from(width) * u64::from(height) * 4 * 12,
                "Independent sibling textures accumulated"
            );
            assert_eq!(main.deferred_layers, count as u64);
            if frame > 1 && budget >= u64::from(width) * u64::from(height) * 4 * 12 {
                assert_eq!(
                    main.created_textures, 0,
                    "Warm scratch targets were recreated"
                );
            }
        }
        if output != "-" {
            renderer
                .capture_frame()
                .unwrap()
                .save(format!("{output}/{frame:03}.png"))
                .unwrap();
        }
    }
    renderer
        .device()
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .unwrap();
    println!("elapsed_ms={:.3}", start.elapsed().as_secs_f64() * 1000.0);
    if let Some(report) = renderer.device().generate_allocator_report() {
        println!("allocator={report:.8?}");
    }
}

#[cfg(target_family = "wasm")]
fn main() {}
