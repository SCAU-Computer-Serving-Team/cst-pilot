#version 100
precision highp float;

/** @resolution */
uniform vec2 u_resolution;
/** @time */
uniform float u_time;
/** @label Animation speed
 * @default 30
 * @range 0, 100
 */
uniform float u_speed;
/** @label Timeline offset
 * @default 0
 * @range 0, 120
 */
uniform float u_phase;
/** @label Drift amplitude
 * @default 1
 * @range 0, 2
 */
uniform float u_drift;
/** @label Softness
 * @default 24
 * @range 0, 48
 */
uniform float u_softness;
/** @label Grain
 * @default 0.003
 * @range 0, 0.05
 */
uniform float u_grain;
/** @label Homepage palette */
uniform sampler2D u_palette;

vec3 palette(float position) {
    return texture2D(u_palette, vec2((clamp(position, 0.0, 1.0) * 255.0 + 0.5) / 256.0, 0.5)).rgb;
}

// Independent AIR-inspired implementation; see blue-hour-air.md.
// Positions and radii are normalized to the canvas, not viewport pixels.
vec2 drift(float phase, float t) {
    return u_drift * vec2(
        0.065 * (sin(t + phase) - sin(phase)),
        0.045 * (cos(t * 0.83 + phase) - cos(phase))
    );
}

float cloud(vec2 uv, vec2 center, vec2 radius) {
    float distanceFromCenter = length((uv - center) / radius);
    return 1.0 - clamp((distanceFromCenter - 0.22) / 0.50, 0.0, 1.0);
}

float field(vec2 uv, float t) {
    // Blend palette coordinates, not distant RGB endpoints. Every transition
    // passes through the same blue/periwinkle stops as the homepage.
    float tone = 0.10;
    tone = mix(tone, 0.70, cloud(uv, vec2(0.50, 0.93) + drift(3.7, t), vec2(0.90, 0.76)));
    tone = mix(tone, 0.30, cloud(uv, vec2(0.24, 0.16) + drift(5.1, t), vec2(0.50, 0.44)));
    tone = mix(tone, 0.50, cloud(uv, vec2(0.88, 0.84) + drift(2.6, t), vec2(0.78, 0.62)));
    tone = mix(tone, 0.10, cloud(uv, vec2(0.78, 0.26) + drift(1.3, t), vec2(0.56, 0.72)));
    tone = mix(tone, 0.90, cloud(uv, vec2(0.16, 0.82) + drift(0.0, t), vec2(0.72, 0.58)));
    return tone;
}

void main() {
    vec2 uv = vec2(gl_FragCoord.x / u_resolution.x, 1.0 - gl_FragCoord.y / u_resolution.y);
    float t = u_phase + u_time * u_speed * 0.002;
    vec2 softness = vec2(u_softness * max(u_resolution.x, u_resolution.y) / 900.0) / u_resolution;
    // Smooth the scalar field before color lookup, never blur RGB colors.
    float tone = field(uv, t) * 0.25;
    tone += field(uv + vec2(softness.x, 0.0), t) * 0.125;
    tone += field(uv - vec2(softness.x, 0.0), t) * 0.125;
    tone += field(uv + vec2(0.0, softness.y), t) * 0.125;
    tone += field(uv - vec2(0.0, softness.y), t) * 0.125;
    tone += field(uv + softness, t) * 0.0625;
    tone += field(uv - softness, t) * 0.0625;
    tone += field(uv + vec2(softness.x, -softness.y), t) * 0.0625;
    tone += field(uv + vec2(-softness.x, softness.y), t) * 0.0625;
    // Keep footer contrast without a separate desaturating RGB overlay.
    tone = mix(tone, 0.90, smoothstep(0.70, 0.93, uv.y));
    vec3 color = palette(tone);
    float grain = fract(sin(dot(floor(gl_FragCoord.xy), vec2(12.9898, 78.233))) * 43758.5453);
    vec3 overlay = mix(2.0 * color * grain, 1.0 - 2.0 * (1.0 - color) * (1.0 - grain), step(vec3(0.5), color));
    color = mix(color, overlay, u_grain);
    gl_FragColor = vec4(clamp(color, 0.0, 1.0), 1.0);
}
