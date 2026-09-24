import { useEffect, useRef } from "react";
import homeShader from "../../../design/asset/blue-hour.glsl?raw";
import airShader from "../../../design/asset/blue-hour-air.glsl?raw";
import lightPalette from "../../../design/asset/blue-hour-palette.png?url";
import darkPalette from "../../../design/asset/blue-hour-dark-palette.png?url";
import airDarkPalette from "../../../design/asset/blue-hour-air-dark-palette.png?url";

const vertexShader = `attribute vec2 a_position;
void main() { gl_Position = vec4(a_position, 0.0, 1.0); }`;

type BackgroundKind = "home" | "air";

function compile(gl: WebGLRenderingContext, type: number, source: string): WebGLShader | null {
  const shader = gl.createShader(type);
  if (!shader) return null;
  gl.shaderSource(shader, source.replace(/^#version 100\s*/, ""));
  gl.compileShader(shader);
  if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
  console.error("Blue hour shader 编译失败：", gl.getShaderInfoLog(shader));
  gl.deleteShader(shader);
  return null;
}

function prefersDark(): boolean {
  const theme = document.documentElement.dataset.theme;
  return theme === "dark" || (theme !== "light" && window.matchMedia("(prefers-color-scheme: dark)").matches);
}

export function BlueHour({ kind }: { kind: BackgroundKind }) {
  const ref = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    const gl = canvas.getContext("webgl", { alpha: false, antialias: false });
    if (!gl) return;
    const vertex = compile(gl, gl.VERTEX_SHADER, vertexShader);
    const fragment = compile(gl, gl.FRAGMENT_SHADER, kind === "home" ? homeShader : airShader);
    if (!vertex || !fragment) return;
    const program = gl.createProgram();
    if (!program) return;
    gl.attachShader(program, vertex);
    gl.attachShader(program, fragment);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      console.error("Blue hour shader 链接失败：", gl.getProgramInfoLog(program));
      return;
    }
    gl.useProgram(program);
    const buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
    const position = gl.getAttribLocation(program, "a_position");
    gl.enableVertexAttribArray(position);
    gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);
    const palette = gl.createTexture();
    if (!palette) return;
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, palette);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.uniform1i(gl.getUniformLocation(program, "u_palette"), 0);
    const set = (key: string, value: number) => gl.uniform1f(gl.getUniformLocation(program, key), value);
    const dark = prefersDark();
    set("u_speed", 30);
    set("u_phase", kind === "home" ? 20.75 : 0);
    set("u_grain", kind === "home" ? 0.015 : dark ? 0.015 : 0.003);
    if (kind === "home") {
      set("u_count", 11);
      set("u_height", dark ? 70 : 60);
      set("u_focus", dark ? 63 : 50);
      set("u_position", dark ? 64 : 50);
      set("u_blend", dark ? 81 : 65);
      set("u_facets", 35);
    } else {
      set("u_drift", 1);
      set("u_softness", 24);
    }
    const image = new Image();
    let ready = false;
    image.onload = () => {
      gl.bindTexture(gl.TEXTURE_2D, palette);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB, gl.RGB, gl.UNSIGNED_BYTE, image);
      ready = true;
      cancelAnimationFrame(frame);
      draw(performance.now());
    };
    image.onerror = () => console.error("Blue hour 色表加载失败：", image.src);
    image.src = kind === "air" ? (dark ? airDarkPalette : lightPalette) : (dark ? darkPalette : lightPalette);
    const motion = window.matchMedia("(prefers-reduced-motion: reduce)");
    let frame = 0;
    let last = performance.now();
    let elapsed = 0;
    let live = true;
    let painted = false;
    const context = gl;
    function draw(now: number) {
      if (!live) return;
      if (ready && (!document.hidden || !painted)) {
        const width = Math.max(1, Math.round(canvas!.clientWidth * Math.min(devicePixelRatio, 2)));
        const height = Math.max(1, Math.round(canvas!.clientHeight * Math.min(devicePixelRatio, 2)));
        if (canvas!.width !== width || canvas!.height !== height) {
          canvas!.width = width;
          canvas!.height = height;
          context.viewport(0, 0, width, height);
        }
        if (!motion.matches && !document.hidden) elapsed += Math.min(now - last, 100);
        context.uniform2f(context.getUniformLocation(program!, "u_resolution"), width, height);
        set("u_time", elapsed / 1000);
        context.drawArrays(context.TRIANGLE_STRIP, 0, 4);
        canvas!.style.opacity = "1";
        painted = true;
      }
      last = now;
      frame = requestAnimationFrame(draw);
    }
    frame = requestAnimationFrame(draw);
    return () => {
      live = false;
      cancelAnimationFrame(frame);
      image.onload = null;
      gl.deleteTexture(palette);
      gl.deleteBuffer(buffer);
      gl.deleteProgram(program);
      gl.deleteShader(vertex);
      gl.deleteShader(fragment);
    };
  }, [kind]);

  return <canvas aria-hidden="true" className={`blue-hour blue-hour--${kind}`} ref={ref} />;
}
