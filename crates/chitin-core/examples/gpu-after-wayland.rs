//! Test GPU init after creating a Wayland socket (mimics compositor startup).
use std::time::Instant;

fn main() {
    // Create a Wayland display first (like the compositor does)
    println!("Creating Wayland display...");
    let wl_display = wayland_server::Display::<()>::new().expect("wl display");
    let _socket = wl_display.add_socket_auto().expect("socket");
    println!("Wayland display created: {}", std::env::var("WAYLAND_DISPLAY").unwrap_or_default());

    // Now do GPU init (like the compositor's spawned thread)
    println!("\nStarting GPU init in spawned thread...");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("tokio runtime");

        rt.block_on(async {
            println!("[gpu] Creating instance...");
            let start = Instant::now();
            let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
                backends: wgpu::Backends::VULKAN | wgpu::Backends::GL,
                flags: wgpu::InstanceFlags::default(),
                backend_options: Default::default(),
                memory_budget_thresholds: Default::default(),
            });
            println!("[gpu] Instance created in {:.2}s", start.elapsed().as_secs_f64());

            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                })
                .await
                .expect("no adapter");
            println!("[gpu] Adapter: {}", adapter.get_info().name);

            let (device, queue) = adapter.request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("test"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: Default::default(),
                    trace: wgpu::Trace::Off,
                },
            ).await.expect("no device");
            println!("[gpu] Device created ✅");
            drop(queue);
            drop(device);
        });
        
        let _ = tx.send("done");
    });

    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(_) => println!("\n✅ GPU init completed"),
        Err(e) => println!("\n❌ Timeout: {}", e),
    }
}
