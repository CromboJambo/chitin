//! Test if GPU init is slow when run in a spawned thread.
use std::time::Instant;

fn init_gpu() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .expect("tokio runtime");

    rt.block_on(async {
        println!("[thread] Creating instance...");
        let start = Instant::now();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
            flags: wgpu::InstanceFlags::default(),
            backend_options: Default::default(),
            memory_budget_thresholds: Default::default(),
        });
        println!("[thread] Instance created in {:.2}s", start.elapsed().as_secs_f64());

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
            })
            .await
            .expect("no adapter");
        println!("[thread] Adapter: {}", adapter.get_info().name);

        let (device, queue) = adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
            },
        ).await.expect("no device");
        println!("[thread] Device created ✅");
        drop(queue);
        drop(device);
    });
}

fn main() {
    // Test 1: on main thread
    println!("=== Test 1: main thread ===");
    let start = Instant::now();
    init_gpu();
    println!("[main] Total: {:.2}s\n", start.elapsed().as_secs_f64());

    // Test 2: in spawned thread with timeout (like the compositor)
    println!("=== Test 2: spawned thread ===");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        init_gpu();
        let _ = tx.send("done");
    });
    
    let start = Instant::now();
    match rx.recv_timeout(std::time::Duration::from_secs(15)) {
        Ok(_) => println!("[main] Thread finished in {:.2}s", start.elapsed().as_secs_f64()),
        Err(e) => println!("[main] Timeout: {}", e),
    }
}
