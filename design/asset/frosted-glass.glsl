#version 100
precision highp float;

// Physical frosted glass shader for CST Pilot Desktop overlays & popovers.
// Uses an unjittered, properly-tapered 11x11 Gaussian kernel so thin high-contrast
// lines blur into a smooth continuous wash without box-cutoff echoes or stipple noise.

/** @resolution */
uniform vec2 u_resolution;

/** @sdf */
uniform sampler2D u_sdf;

/**
 * @label Background
 * @mipmap
 */
uniform sampler2D u_bg;

/**
 * @label Blur Radius
 * @default 8.0
 * @range 0.0, 24.0
 */
uniform float u_blur;

/**
 * @label Edge Refraction
 * @default 0.0
 * @range 0.0, 8.0
 */
uniform float u_refract;

/**
 * @label Vibrancy
 * @default 1.06
 * @range 0.8, 1.5
 */
uniform float u_vibrancy;

/**
 * @label Tint Color
 * @color
 * @default #ffffff
 */
uniform vec3 u_tint;

/**
 * @label Tint Opacity
 * @default 0.92
 * @range 0.0, 1.0
 */
uniform float u_tintOpacity;

/**
 * @label Specular Strength
 * @default 0.16
 * @range 0.0, 1.0
 */
uniform float u_specular;

/**
 * @label Frost Grain
 * @default 0.0
 * @range 0.0, 0.05
 */
uniform float u_grain;

float hash21(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

void main() {
    vec2 res = max(u_resolution, vec2(1.0));
    vec2 uv = gl_FragCoord.xy / res;

    vec4 sdf = texture2D(u_sdf, uv);
    float dist = max(sdf.r, 0.0);
    vec2 grad = sdf.gb;
    float glen = length(grad);
    vec2 normalIn = glen > 1e-4 ? grad / glen : vec2(0.0, -1.0);

    vec2 refractUv = vec2(0.0);
    if (u_refract > 0.001) {
        float bevel = exp(-dist * 0.28);
        refractUv = -normalIn * (u_refract * bevel) / res;
    }

    // Smooth, unjittered 13x13 Gaussian kernel with texel-overlapping step (<= 1.35px)
    // so thin 1.5px lines never fall between taps or form grid stripes.
    float radius = max(u_blur, 0.0);
    float stepPx = min(radius * 0.14, 1.35);

    vec3 acc = vec3(0.0);
    float wSum = 0.0;
    for (int iy = -6; iy <= 6; iy++) {
        for (int ix = -6; ix <= 6; ix++) {
            float fx = float(ix);
            float fy = float(iy);
            float d2 = fx * fx + fy * fy;
            float w = exp(-0.11 * d2);
            vec2 offset = vec2(fx, fy) * stepPx / res;
            vec2 suv = clamp(uv + refractUv + offset, vec2(0.001), vec2(0.999));
            vec4 s = texture2D(u_bg, suv);
            vec3 sRgb = s.a > 1e-4 ? s.rgb / s.a : u_tint;
            acc += sRgb * w;
            wSum += w;
        }
    }
    vec3 blurred = acc / max(wSum, 1e-4);

    // Vibrancy boost prevents desaturated gray muddiness
    float luma = dot(blurred, vec3(0.2126, 0.7152, 0.0722));
    vec3 vibrant = clamp(mix(vec3(luma), blurred, u_vibrancy), 0.0, 1.0);

    // Translucent glass body tint
    vec3 glass = mix(vibrant, u_tint, clamp(u_tintOpacity, 0.0, 1.0));

    // Adaptive overhead sheen + top-lit Fresnel bevel highlight (no milky wash in dark mode)
    float tintLuma = dot(u_tint, vec3(0.2126, 0.7152, 0.0722));
    float bodySheenScale = smoothstep(0.25, 0.85, tintLuma);
    float rimScale = mix(0.22, 1.0, smoothstep(0.08, 0.85, tintLuma));
    glass += vec3(u_specular * bodySheenScale * 0.04 * smoothstep(0.35, 1.0, uv.y));
    float rim = 1.0 - smoothstep(0.25, 1.75, dist);
    float topBias = clamp(0.5 + 0.5 * (-normalIn.y * 0.85 - normalIn.x * 0.25), 0.15, 1.0);
    glass = mix(glass, vec3(1.0), rim * topBias * u_specular * rimScale * 0.45);

    if (u_grain > 0.0001) {
        vec2 px = floor(gl_FragCoord.xy);
        float n = (hash21(px) + hash21(px + vec2(37.0, 17.0))) - 1.0;
        glass = clamp(glass + vec3(n * u_grain), 0.0, 1.0);
    }

    gl_FragColor = vec4(glass, 1.0);
}
