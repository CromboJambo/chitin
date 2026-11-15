#!/bin/bash
# Test script: run compositor with frame saving, connect test client, verify PNG output.
set -e

cd /home/crombo/projects/active/chitin

# Clean up previous frames
rm -f /tmp/chitin-frame-*.png

echo "=== Starting chitin-compositor ==="
CHITIN_SAVE_FRAME=/tmp/chitin-frame RUST_LOG="chitin_core=debug,wgpu_hal=error,naga=error" \
    ./target/debug/chitin-compositor &
COMPOSITOR_PID=$!
sleep 3

if ! kill -0 $COMPOSITOR_PID 2>/dev/null; then
    echo "ERROR: compositor died within 3s"
    exit 1
fi
echo "Compositor running (PID $COMPOSITOR_PID)"

echo ""
echo "=== Running test client ==="
WAYLAND_DISPLAY=wayland-0 RUST_LOG="info" \
    cargo run --example test-client 2>&1 | grep -E "SUCCESS|TIMEOUT|ERROR|configure" || true

sleep 2

echo ""
echo "=== Checking for saved frames ==="
ls -la /tmp/chitin-frame-*.png 2>/dev/null || echo "NO FRAMES SAVED"

# Kill compositor
kill $COMPOSITOR_PID 2>/dev/null || true
wait $COMPOSITOR_PID 2>/dev/null || true

echo ""
echo "=== Done ==="
