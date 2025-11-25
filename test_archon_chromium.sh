#!/bin/bash
# Archon Chromium Max Test Launch Script

set -e

echo "=== Archon Chromium Max Test Launcher ==="
echo

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

PROJECT_ROOT="/data/projects/archon"
CONFIG_DIR="$HOME/.config/Archon"
PROFILE_DIR="$HOME/.local/share/Archon/profiles/test"
EXTENSIONS_DIR="$PROJECT_ROOT/extensions"

# Create directories
mkdir -p "$CONFIG_DIR"
mkdir -p "$PROFILE_DIR"

echo "[1/8] Checking prerequisites..."

# Check for Chromium
if ! command -v chromium &> /dev/null; then
    echo -e "${RED}✗ Chromium not found${NC}"
    exit 1
fi
echo -e "${GREEN}✓ Chromium found: $(which chromium)${NC}"

# Check if archon-host binary exists
if [ ! -f "$PROJECT_ROOT/target/release/archon-host" ]; then
    echo -e "${YELLOW}⚠ archon-host not built in release mode${NC}"
    echo "  Building now..."
    cd "$PROJECT_ROOT"
    cargo build --release --bin archon-host
fi
echo -e "${GREEN}✓ archon-host binary ready${NC}"

echo
echo "[2/8] Starting Archon Host..."
# Kill any existing archon-host
pkill -f archon-host || true
sleep 1

# Start archon-host in background
"$PROJECT_ROOT/target/release/archon-host" --listen 127.0.0.1:8805 &
ARCHON_HOST_PID=$!
echo -e "${GREEN}✓ Archon Host started (PID: $ARCHON_HOST_PID)${NC}"

# Wait for host to be ready
echo "  Waiting for host to be ready..."
for i in {1..10}; do
    if curl -s http://127.0.0.1:8805/health > /dev/null 2>&1; then
        echo -e "${GREEN}✓ Archon Host is healthy${NC}"
        break
    fi
    if [ $i -eq 10 ]; then
        echo -e "${RED}✗ Archon Host failed to start${NC}"
        kill $ARCHON_HOST_PID 2>/dev/null || true
        exit 1
    fi
    sleep 1
done

echo
echo "[3/8] Checking extensions..."

# Package crypto-omnibox if needed
if [ ! -f "$EXTENSIONS_DIR/crypto-omnibox/manifest.json" ]; then
    echo -e "${RED}✗ crypto-omnibox extension not found${NC}"
    exit 1
fi
echo -e "${GREEN}✓ crypto-omnibox extension found${NC}"

# Check AI sidebar
if [ ! -f "$EXTENSIONS_DIR/archon-sidebar/manifest.json" ]; then
    echo -e "${YELLOW}⚠ archon-sidebar extension not found${NC}"
else
    echo -e "${GREEN}✓ archon-sidebar extension found${NC}"
fi

echo
echo "[4/8] Detecting GPU and display..."

# Detect compositor
if [ -n "$WAYLAND_DISPLAY" ]; then
    DISPLAY_TYPE="wayland"
    OZONE_FLAG="--ozone-platform=wayland"
    echo -e "${GREEN}✓ Wayland detected${NC}"
elif [ -n "$DISPLAY" ]; then
    DISPLAY_TYPE="x11"
    OZONE_FLAG="--ozone-platform=x11"
    echo -e "${YELLOW}⚠ X11 detected (Wayland preferred)${NC}"
else
    echo -e "${RED}✗ No display server detected${NC}"
    kill $ARCHON_HOST_PID 2>/dev/null || true
    exit 1
fi

# Detect GPU
if lspci | grep -i nvidia > /dev/null 2>&1; then
    GPU_TYPE="nvidia"
    echo -e "${GREEN}✓ NVIDIA GPU detected${NC}"
elif lspci | grep -i amd > /dev/null 2>&1; then
    GPU_TYPE="amd"
    echo -e "${GREEN}✓ AMD GPU detected${NC}"
else
    GPU_TYPE="intel"
    echo -e "${GREEN}✓ Intel GPU detected${NC}"
fi

echo
echo "[5/8] Building Chromium flags..."

CHROMIUM_FLAGS=(
    # Display
    "$OZONE_FLAG"
    "--use-gl=egl"
    
    # Profile
    "--user-data-dir=$PROFILE_DIR"
    
    # Extensions
    "--load-extension=$EXTENSIONS_DIR/crypto-omnibox,$EXTENSIONS_DIR/archon-sidebar"
    
    # GPU acceleration
    "--enable-gpu-rasterization"
    "--enable-zero-copy"
    "--enable-features=VaapiVideoDecoder,VaapiVideoEncoder,CanvasOopRasterization,RawDraw,UseSkiaRenderer"
    
    # Privacy/Security
    "--disable-background-networking"
    "--disable-breakpad"
    "--disable-crash-reporter"
    "--disable-sync"
    "--disable-translate"
    
    # Performance
    "--enable-quic"
    "--process-per-site"
    
    # WebGPU (safe for testing)
    "--enable-unsafe-webgpu"
    
    # Useful for debugging
    "--enable-logging=stderr"
    "--v=1"
)

echo -e "${GREEN}✓ Chromium flags configured${NC}"
echo "  Display: $DISPLAY_TYPE"
echo "  GPU: $GPU_TYPE"
echo "  Profile: $PROFILE_DIR"

echo
echo "[6/8] Setting environment..."

export ARCHON_HOST_URL="http://127.0.0.1:8805"

# GPU-specific environment
if [ "$GPU_TYPE" == "nvidia" ]; then
    export NVIDIA_DRIVER_CAPABILITIES=all
    export __GL_THREADED_OPTIMIZATIONS=1
    export __GL_SHADER_DISK_CACHE=1
    echo -e "${GREEN}✓ NVIDIA environment configured${NC}"
fi

echo
echo "[7/8] Launching Chromium Max..."
echo
echo -e "${YELLOW}Extensions to test:${NC}"
echo "  1. Type 'crypto vitalik.eth' in omnibox"
echo "  2. Click Archon Sidebar icon for AI chat"
echo "  3. Navigate to test pages for crypto domains"
echo
echo -e "${YELLOW}Press Ctrl+C to stop all services${NC}"
echo

# Trap to cleanup on exit
cleanup() {
    echo
    echo "Cleaning up..."
    kill $ARCHON_HOST_PID 2>/dev/null || true
    echo "Done!"
}
trap cleanup EXIT

# Launch Chromium
chromium "${CHROMIUM_FLAGS[@]}" "about:blank" &
CHROMIUM_PID=$!

echo
echo "[8/8] Chromium launched (PID: $CHROMIUM_PID)"
echo -e "${GREEN}✓ All services running${NC}"
echo

# Wait for Chromium to exit
wait $CHROMIUM_PID

