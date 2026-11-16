#version 100
precision highp float;

// Ported 1:1 from Northern lights.jsx (FeralUI Aurora export).
// STOPS: #EAFFF4, #4BE8A0, #2E6E80, #16224D
// START_T = 27.742078400000008, DEFAULT_SPEED = 26, SCALE = 24, DISTORTION = 27, SWIRL = 18, DIRECTION = 0, GRAIN = 6

/** @resolution */
uniform vec2 u_resolution;
/** @time */
uniform float u_time;
/**
 * @label Animation speed
 * @default 26
 * @range 0, 100
 */
uniform float u_speed;
/**
 * @label Scale
 * @default 24
 * @range 0, 100
 */
uniform float u_scale;
/**
 * @label Distortion
 * @default 27
 * @range 0, 100
 */
uniform float u_distortion;
/**
 * @label Swirl
 * @default 18
 * @range 0, 100
 */
uniform float u_swirl;
/**
 * @label Direction quarter-turn
 * @default 0
 * @range 0, 3
 */
uniform float u_direction;
/**
 * @label Grain
 * @default 6
 * @range 0, 100
 */
uniform float u_grain;
/**
 * @label Phase offset
 * @default 0
 * @range -30, 30
 */
uniform float u_phase;
/**
 * @label Curtain elevation
 * @default 0
 * @range -0.25, 0.25
 */
uniform float u_elevation;

const float START_T = 27.7420784;
const float FLOW_SPEED_RATE = 1.2;

const vec3 U_RIM     = vec3(0.91764706, 1.00000000, 0.95686275);
const vec3 U_GLOW    = vec3(0.29411765, 0.90980392, 0.62745098);
const vec3 U_HORIZON = vec3(0.18039216, 0.43137255, 0.50196078);
const vec3 U_DEEP    = vec3(0.08627451, 0.13333333, 0.30196078);

float hash21(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453123);
}

float noise2(vec2 p) {
    vec2 ip = floor(p);
    vec2 u = fract(p);
    u = u * u * (3.0 - 2.0 * u);
    return mix(
        mix(hash21(ip), hash21(ip + vec2(1.0, 0.0)), u.x),
        mix(hash21(ip + vec2(0.0, 1.0)), hash21(ip + vec2(1.0, 1.0)), u.x),
        u.y
    );
}

float fbm3(vec2 p) {
    float v = 0.0;
    float a = 0.5;
    mat2 rot = mat2(cos(0.5), sin(0.5), -sin(0.5), cos(0.5));
    for (int i = 0; i < 3; i++) {
        v += a * noise2(p);
        p = rot * p * 2.02 + vec2(37.0, 17.0);
        a *= 0.5;
    }
    return v;
}

float fbm2(vec2 p) {
    float v = 0.0;
    float a = 0.5;
    mat2 rot = mat2(cos(0.5), sin(0.5), -sin(0.5), cos(0.5));
    for (int i = 0; i < 2; i++) {
        v += a * noise2(p);
        p = rot * p * 2.02 + vec2(37.0, 17.0);
        a *= 0.5;
    }
    return v;
}

float texelRand256(vec2 cell) {
    vec2 wrapped = mod(cell, 256.0);
    vec3 p3 = fract(vec3(wrapped.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

void main() {
    float u_t = START_T + u_phase + u_time * (clamp(u_speed, 0.0, 100.0) / 100.0) * FLOW_SPEED_RATE;
    float u_freq = 1.0 + (1.0 - clamp(u_scale, 0.0, 100.0) / 100.0) * 2.8;
    float u_fold = clamp(u_distortion, 0.0, 100.0) / 100.0;
    float u_drift = 0.15 + (clamp(u_swirl, 0.0, 100.0) / 100.0) * 1.1;
    float dirAngle = mod(floor(u_direction + 0.5), 4.0) * 1.57079632679;
    vec2 u_dirv = vec2(cos(dirAngle), sin(dirAngle));

    vec2 st = gl_FragCoord.xy / u_resolution - 0.5;
    st.x *= u_resolution.x / u_resolution.y;
    st = mat2(u_dirv.x, u_dirv.y, -u_dirv.y, u_dirv.x) * st;
    float x = st.x;
    float hv = st.y + 0.5;
    float time = u_t * 0.35;

    float hzp = 1.0 - smoothstep(0.0, 0.22, hv);
    vec3 sky = mix(U_DEEP, U_HORIZON, hzp * hzp * 0.18);

    float amp = 0.05 + u_fold * 0.22;
    float tiltG = (noise2(vec2(time * 0.11, 71.3)) - 0.5) * 0.9;
    float tiltR = (noise2(vec2(time * 0.07, 41.7)) - 0.5) * 0.35;
    float wob = (fbm2(vec2(x * u_freq * 0.6 + time * u_drift * 0.3, time * 0.09)) - 0.5) * 1.6;
    float ph = time * u_drift * 0.7 + wob;

    float sf = u_freq * u_freq * 0.65;
    float th = x * sf + ph;
    float th2 = (x + 0.012) * sf + ph;
    float wv = 0.5 + 0.5 * (0.78 * sin(th) + 0.22 * sin(2.0 * th + 2.1));
    float wv2 = 0.5 + 0.5 * (0.78 * sin(th2) + 0.22 * sin(2.0 * th2 + 2.1));
    float wave = (wv - 0.5) * 2.0 + x * tiltR;
    float slope = abs(wv2 - wv) / 0.012;

    vec3 aur = vec3(0.0);
    float cover = 0.0;
    for (int i = 0; i < 2; i++) {
        float fi = float(i);
        float freq = u_freq * (1.0 + fi * 0.15);
        float drift = u_drift * (0.75 + fi * 0.25);
        float nr = smoothstep(0.35, 0.65, fbm2(vec2(x * freq * 0.5 + time * drift * 0.4 + fi * 21.3, fi * 3.77 + time * 0.07)));
        float wig = fbm3(vec2(x * freq * 2.6 + time * drift + fi * 3.7, fi * 7.31 + time * 0.13)) - 0.5;
        float spine = 0.31 + u_elevation + fi * 0.12 + amp * wave;

        float m = slope * amp * 2.0;
        float widen = sqrt(1.0 + min(m * m * 2.5, 5.0));
        float s = (0.17 + u_fold * 0.07) * (0.7 + nr * 0.85) * (1.0 - fi * 0.2) * widen;
        float knotK = 1.0 + (0.25 + 1.15 * u_fold) * smoothstep(1.1, 3.0, slope * amp * 6.0);
        float surge = 0.78 + 0.22 * noise2(vec2(x * 1.1 - time * 1.3, 5.5 + fi * 2.2));
        float L = (0.6 + 0.4 * nr) * knotK * surge;
        float u = (hv - spine) / s;

        float xr = x * freq * (4.5 + 2.0 * nr) + wig * 2.2 + time * drift * 1.6 + max(u, 0.0) * s * (tiltG + x * 0.22) * 3.0;
        float yr = hv * 0.8 + fi * 9.13;
        float v = 0.5 * noise2(vec2(xr, yr))
                + 0.28 * noise2(vec2(xr * 2.7 + 13.7, yr * 1.7 + 5.2))
                + 0.14 * noise2(vec2(xr * 6.1 + 31.9, yr * 2.3 + 11.4))
                + 0.08 * noise2(vec2(xr * 12.3 + 57.1, yr * 2.9 + 23.7));

        float soft = 0.55 + 0.45 * smoothstep(0.3, 0.7, v);
        float hard = 0.1 + 0.9 * smoothstep(0.38, 0.6, v);
        float bn = noise2(vec2(x * freq * 1.4 + time * drift * 0.8 + fi * 17.3, fi * 4.77));
        float flick = 0.85 + 0.15 * noise2(vec2(x * freq * 3.1 + time * 2.8, 7.7 + fi * 3.0));
        float rayK = mix(soft, hard, u_fold) * (0.45 + 0.55 * smoothstep(0.26, 0.72, bn)) * flick;
        float kBody = 1.0 - 0.7 * fi;

        float d = u - 0.15;
        float sig = d < 0.0 ? 0.26 : 0.55;
        float beam = exp(-(d * d) / (2.0 * sig * sig));
        float body = beam * (0.28 + 0.72 * rayK);
        float k = kBody * L;

        float peakK = beam * beam * 0.75;
        vec3 gcol = mix(U_GLOW, U_RIM, peakK);
        gcol = mix(gcol, U_DEEP, smoothstep(0.35, 1.05, u) * 0.5);
        gcol = mix(gcol, U_HORIZON, (1.0 - smoothstep(-0.38, -0.12, d)) * 0.3);

        float gB = body * (0.88 + u_fold * 0.36) * k;
        aur += gcol * gB;
        cover += gB;
    }

    vec2 gp = vec2(x, hv) * 60.0;
    vec2 cell = floor(gp);
    float hct = hash21(cell);
    float starI = 0.0;
    if (hct > 0.96) {
        vec2 spos = vec2(hash21(cell + 11.3), hash21(cell + 27.7)) * 0.6 + 0.2;
        float r = length(fract(gp) - spos);
        float tw = 0.6 + 0.4 * sin(u_t * (2.0 + hct * 6.0) + hct * 100.0);
        float bmag = (hct - 0.96) / 0.04;
        starI = pow(max(0.0, 1.0 - r / 0.14), 4.0) * (0.3 + 0.7 * bmag) * tw;
        starI *= smoothstep(0.03, 0.12, hv) * clamp(1.0 - cover * 2.2, 0.0, 1.0);
        float skyL = dot(sky, vec3(0.299, 0.587, 0.114));
        starI *= 1.0 - smoothstep(0.25, 0.6, skyL);
    }

    vec3 col = clamp(sky + aur + vec3(0.95, 0.97, 1.0) * starI, 0.0, 1.0);

    vec2 tilePx = mod(floor(gl_FragCoord.xy), 256.0);
    float gr1 = texelRand256(tilePx);
    float gr2 = texelRand256(tilePx + vec2(37.0, 17.0));
    float grain = 0.5 * (gr1 + gr2);
    vec3 overlay = mix(2.0 * col * grain, 1.0 - 2.0 * (1.0 - col) * (1.0 - grain), step(vec3(0.5), col));

    gl_FragColor = vec4(mix(col, overlay, clamp(u_grain, 0.0, 100.0) / 200.0), 1.0);
}
