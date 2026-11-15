//! GPU diagnostic: enumerate adapters and test device creation.
use std::time::Instant;

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .expect("tokio runtime");

    rt.block_on(async {
        println!("=== Creating wgpu instance (VULKAN) ===");
        let start = Instant::now();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            flags: wgpu::InstanceFlags::default(),
            backend_options: Default::default(),
            memory_budget_thresholds: Default::default(),
        });
        println!("Instance created in {:.2}s", start.elapsed().as_secs_f64());

        println!("\n=== Enumerating adapters ===");
        let adapters = instance.enumerate_adapters(wgpu::Backends::all());
        for (i, adapter) in adapters.iter().enumerate() {
            let info = adapter.get_info();
            println!("Adapter {}: {} ({:?}, {:?})", i, info.name, info.device_type, info.backend);
        }

        println!("\n=== Requesting adapter (HighPerformance) ===");
        let start = Instant::now();
        let maybe_adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await;
        
        match maybe_adapter {
            Ok(adapter) => {
                println!("Got adapter in {:.2}s: {}", start.elapsed().as_secs_f64(), adapter.get_info().name);

                println!("\n=== Requesting device ===");
                let start = Instant::now();
                match adapter.request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("diag"),
                        required_features: wgpu::Features::empty(),
                        required_limits: wgpu::Limits::default(),
                        memory_hints: Default::default(),
                        trace: wgpu::Trace::Off,
                    },
                ).await {
                    Ok((device, queue)) => {
                        println!("Device created in {:.2}s", start.elapsed().as_secs_f64());
                        println!("\n✅ SUCCESS");
                        drop(queue);
                        drop(device);
                    }
                    Err(e) => eprintln!("❌ Device creation failed: {}", e),
                }
            }
            Err(e) => eprintln!("❌ Adapter request failed after {:.2}s: {}", start.elapsed().as_secs_f64(), e),
        }
    });
}
