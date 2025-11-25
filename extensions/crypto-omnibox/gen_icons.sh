#!/bin/bash
# Generate placeholder icons using ImageMagick

if ! command -v convert &> /dev/null; then
    echo "ImageMagick not found, creating SVG placeholders instead"
    # Already have icon.svg, just note it
    echo "Using icon.svg as base"
    exit 0
fi

# Create PNG icons from SVG if possible
for size in 16 48 128; do
    convert -background none -density 300 icon.svg -resize ${size}x${size} icon${size}.png 2>/dev/null || {
        # Fallback: create simple gradient icons
        convert -size ${size}x${size} gradient:'#2dd4bf-#7f5af0' \
            -gravity center -pointsize $((size/2)) -fill white -annotate +0+0 'Ξ' \
            icon${size}.png
    }
    echo "Created icon${size}.png"
done
