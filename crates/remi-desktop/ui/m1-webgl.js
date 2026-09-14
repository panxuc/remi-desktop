// Probe: does a transparent WebGL canvas composite inside a transparent webview?
//
// Pass  = a spinning triangle over the desktop, its edges fading smoothly to nothing.
// Fail  = an opaque black (or white) 360x360 box behind it.

const canvas = document.getElementById("stage");
const gl = canvas.getContext("webgl2", {
  alpha: true,
  premultipliedAlpha: true, // so fragments below write premultiplied rgb
  antialias: true,
});

if (!gl) {
  document.body.innerHTML =
    '<p style="color:#f66;font:14px system-ui">no webgl2</p>';
  throw new Error("webgl2 unavailable");
}

const vert = `#version 300 es
in vec2 pos;
in vec3 rgb;
out vec3 v_rgb;
out vec2 v_pos;
uniform float t;
void main() {
  float c = cos(t), s = sin(t);
  vec2 p = mat2(c, -s, s, c) * pos;
  v_rgb = rgb;
  v_pos = pos;
  gl_Position = vec4(p, 0.0, 1.0);
}`;

// Alpha ramps to 0 toward the triangle's rim, so a clean gradient here is also the
// 8-bit-alpha check: 1-bit transparency cannot express it.
const frag = `#version 300 es
precision highp float;
in vec3 v_rgb;
in vec2 v_pos;
out vec4 color;
void main() {
  float a = clamp(1.0 - length(v_pos) * 1.4, 0.0, 1.0);
  color = vec4(v_rgb * a, a); // premultiplied
}`;

function compile(type, src) {
  const sh = gl.createShader(type);
  gl.shaderSource(sh, src);
  gl.compileShader(sh);
  if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
    throw new Error(gl.getShaderInfoLog(sh));
  }
  return sh;
}

const prog = gl.createProgram();
gl.attachShader(prog, compile(gl.VERTEX_SHADER, vert));
gl.attachShader(prog, compile(gl.FRAGMENT_SHADER, frag));
gl.linkProgram(prog);
if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
  throw new Error(gl.getProgramInfoLog(prog));
}
gl.useProgram(prog);

// x, y, r, g, b
const data = new Float32Array([
   0.0,  0.7,  0.98, 0.45, 0.62,
  -0.7, -0.5,  0.45, 0.72, 0.98,
   0.7, -0.5,  0.98, 0.85, 0.45,
]);
gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
gl.bufferData(gl.ARRAY_BUFFER, data, gl.STATIC_DRAW);

const stride = 5 * 4;
const posLoc = gl.getAttribLocation(prog, "pos");
gl.enableVertexAttribArray(posLoc);
gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, stride, 0);
const rgbLoc = gl.getAttribLocation(prog, "rgb");
gl.enableVertexAttribArray(rgbLoc);
gl.vertexAttribPointer(rgbLoc, 3, gl.FLOAT, false, stride, 2 * 4);

const tLoc = gl.getUniformLocation(prog, "t");

gl.enable(gl.BLEND);
gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA); // premultiplied source
gl.clearColor(0, 0, 0, 0);

function resize() {
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(canvas.clientWidth * dpr);
  canvas.height = Math.round(canvas.clientHeight * dpr);
  gl.viewport(0, 0, canvas.width, canvas.height);
}
window.addEventListener("resize", resize);
resize();

const start = performance.now();
function frame(now) {
  gl.clear(gl.COLOR_BUFFER_BIT);
  gl.uniform1f(tLoc, (now - start) / 1000);
  gl.drawArrays(gl.TRIANGLES, 0, 3);
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);
