#version 100
precision highp float;

// Independently implemented PRISM2 slant/expand field for Pen.
// Algorithm reference and differences: blue-hour.md.

/** @resolution */
uniform vec2 u_resolution;
/** @time */
uniform float u_time;
/** Region fade progress; 0 leaves the background unchanged. */
uniform float u_exit_progress;
/** Initial lane offset, held while the animated edges continue moving. */
uniform float u_exit_origin;
uniform float u_css_width;
/**
 * @label Animation speed (0 = still)
 * @default 30
 * @range 0, 100
 */
uniform float u_speed;
/**
 * @label Timeline offset
 * @default 20.75
 * @range 20.75, 120
 */
uniform float u_phase;
/**
 * @label Lanes per half
 * @default 11
 * @range 3, 16
 */
uniform float u_count;
/**
 * @label Gradient height
 * @default 60
 * @range 0, 100
 */
uniform float u_height;
/**
 * @label Horizontal focus
 * @default 50
 * @range 0, 100
 */
uniform float u_focus;
/**
 * @label Vertical position
 * @default 50
 * @range 0, 100
 */
uniform float u_position;
/**
 * @label Blend
 * @default 65
 * @range 0, 100
 */
uniform float u_blend;
/**
 * @label Facet variation
 * @default 35
 * @range 0, 100
 */
uniform float u_facets;
/**
 * @label Film grain
 * @default 0.015
 * @range 0, 0.05
 */
uniform float u_grain;
/**
 * @label Theme palette
 */
uniform sampler2D u_palette;

vec3 palette(float position) {
    return texture2D(u_palette, vec2((clamp(position, 0.0, 1.0) * 255.0 + 0.5) / 256.0, 0.5)).rgb;
}

// The original Canvas renderer samples each vertical gradient at 96 positions.
vec3 gradientSample(float y, float ridge, float facet) {
    float softness = 0.22 + u_blend * 0.008;
    float shaped = smoothstep(0.0, 1.0, (y - ridge + 0.22) / softness);
    return palette(clamp(0.02 + 0.96 * shaped + facet, 0.0, 1.0));
}

float bandOpacity(float order, float count) {
    float delay = clamp(order, 0.0, count) / count * 0.45;
    return 1.0 - smoothstep(0.0, 1.0, (u_exit_progress - delay) / 0.55);
}

void main() {
    vec2 uv = vec2(gl_FragCoord.x / u_resolution.x, 1.0 - gl_FragCoord.y / u_resolution.y);
    float time = u_phase + u_time * (u_speed / 100.0) * 1.2;
    float count = clamp(floor(u_count + 0.5), 3.0, 16.0);
    float travel = time * 0.48;
    float fromCenter = abs(uv.x - 0.5) * 2.0;
    float lane = floor(fromCenter * count - travel);
    float inner = max(0.0, (lane + travel) / count);
    float outer = min(1.0, (lane + 1.0 + travel) / count);
    float center = 0.5 + (uv.x < 0.5 ? -1.0 : 1.0) * (inner + outer) * 0.25;

    // Slant profile, size=100, direction=0, expand motion.
    float envelope = smoothstep(0.0, 1.0, center + 0.5 - u_focus / 100.0);
    float ridge = 0.28 + envelope * (u_height * 0.006)
                + (u_position - 50.0) * 0.009 + 0.015 * sin(time * 0.32);
    float facet = (0.025 * sin(lane * 1.8) + 0.012 * sin(lane * 0.73 + time * 0.24)) * (u_facets / 35.0);
    float sampleY = clamp(uv.y, 0.0, 1.0) * 95.0;
    float lower = floor(sampleY);
    vec3 color = mix(gradientSample(lower / 95.0, ridge, facet),
                     gradientSample(min(lower + 1.0, 95.0) / 95.0, ridge, facet), fract(sampleY));

    // Static grain: no flicker. Matches the SVG's overlay opacity, not its noise bitmap.
    float grain = fract(sin(dot(floor(gl_FragCoord.xy), vec2(12.9898, 78.233))) * 43758.5453);
    vec3 overlay = mix(2.0 * color * grain, 1.0 - 2.0 * (1.0 - color) * (1.0 - grain), step(vec3(0.5), color));
    float alpha = 1.0;
    if (u_exit_progress > 0.0) {
        float order = lane + u_exit_origin;
        alpha = bandOpacity(order, count);
        float cssWidth = u_css_width > 0.0 ? u_css_width : u_resolution.x;
        float feather = min(4.0 / max(1.0, cssWidth), 0.2 / count);
        if (inner > 0.0 && fromCenter - inner < feather) {
            float previous = bandOpacity(order - 1.0, count);
            alpha = mix((previous + alpha) * 0.5, alpha, smoothstep(0.0, feather, fromCenter - inner));
        } else if (outer < 1.0 && outer - fromCenter < feather) {
            float next = bandOpacity(order + 1.0, count);
            alpha = mix((alpha + next) * 0.5, alpha, smoothstep(0.0, feather, outer - fromCenter));
        }
        // Overlap a continuous radial envelope with the low-weight animated bands.
        float envelopeAlpha = 1.0 - smoothstep(0.0, 1.0, (u_exit_progress - fromCenter * 0.45) / 0.55);
        alpha = mix(envelopeAlpha, alpha, 0.2);
    }
    gl_FragColor = vec4(mix(color, overlay, u_grain), alpha);
}
