import { useEffect, useRef } from "react";

/**
 * The prism as a real 3D shape: an icosahedron, which is what the logo shows seen face-on. It turns
 * in space with perspective. Lines and faces get lighter the further away they are, so near edges
 * are dark grey and far ones are light grey, and nothing is ever fully black.
 */

const PHI = (1 + Math.sqrt(5)) / 2;
const RAW: [number, number, number][] = [
  [0, 1, PHI], [0, -1, PHI], [0, 1, -PHI], [0, -1, -PHI],
  [1, PHI, 0], [-1, PHI, 0], [1, -PHI, 0], [-1, -PHI, 0],
  [PHI, 0, 1], [-PHI, 0, 1], [PHI, 0, -1], [-PHI, 0, -1],
];
const LENGTH = Math.hypot(...RAW[0]);
const VERTICES = RAW.map(([x, y, z]) => [x / LENGTH, y / LENGTH, z / LENGTH] as const);
const dist = (a: number, b: number) => Math.hypot(VERTICES[a][0] - VERTICES[b][0], VERTICES[a][1] - VERTICES[b][1], VERTICES[a][2] - VERTICES[b][2]);
const EDGE = 2 / LENGTH;
const EDGES: [number, number][] = [];
for (let i = 0; i < 12; i++) for (let j = i + 1; j < 12; j++) if (Math.abs(dist(i, j) - EDGE) < 1e-6) EDGES.push([i, j]);
const near = (a: number, b: number) => Math.abs(dist(a, b) - EDGE) < 1e-6;
const FACES: [number, number, number][] = [];
for (let i = 0; i < 12; i++) for (let j = i + 1; j < 12; j++) for (let k = j + 1; k < 12; k++) if (near(i, j) && near(j, k) && near(i, k)) FACES.push([i, j, k]);

const NEAR_GREY = [58, 62, 69]; // dark grey, for what is closest
const FAR_GREY = [200, 204, 210]; // light grey, for what is furthest
const TILT = -0.42; // radians: leaning back a little so the top is visible
const SECONDS_PER_TURN = 7;
const CAMERA = 3.4;
const SCALE = 36;

const mix = (t: number) => `rgb(${NEAR_GREY.map((n, i) => Math.round(FAR_GREY[i] + (n - FAR_GREY[i]) * t)).join(",")})`;

export function Prism3D({ size }: { size: number }) {
  const edgeNodes = useRef<(SVGLineElement | null)[]>([]);
  const faceNodes = useRef<(SVGPolygonElement | null)[]>([]);
  const faceGroup = useRef<SVGGElement>(null);
  const edgeGroup = useRef<SVGGElement>(null);

  useEffect(() => {
    const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    let frame = 0;
    const draw = (angle: number) => {
      const ca = Math.cos(angle), sa = Math.sin(angle), ct = Math.cos(TILT), st = Math.sin(TILT);
      const turned = VERTICES.map(([x, y, z]) => {
        const x1 = x * ca + z * sa, z1 = -x * sa + z * ca; // around the upright axis
        return { x: x1, y: y * ct - z1 * st, z: y * st + z1 * ct }; // then leaning back
      });
      const flat = turned.map(({ x, y, z }) => {
        const s = CAMERA / (CAMERA - z);
        return { x: 50 + x * s * SCALE, y: 50 - y * s * SCALE };
      });
      const faces = FACES.map((face, index) => {
        const [a, b, c] = face.map((v) => turned[v]);
        const cx = (a.x + b.x + c.x) / 3, cy = (a.y + b.y + c.y) / 3, cz = (a.z + b.z + c.z) / 3;
        let nx = (b.y - a.y) * (c.z - a.z) - (b.z - a.z) * (c.y - a.y);
        let ny = (b.z - a.z) * (c.x - a.x) - (b.x - a.x) * (c.z - a.z);
        let nz = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        if (nx * cx + ny * cy + nz * cz < 0) { nx = -nx; ny = -ny; nz = -nz; } // point outwards
        const length = Math.hypot(nx, ny, nz) || 1;
        return { index, z: cz, facing: nz / length };
      }).sort((p, q) => p.z - q.z);
      for (const { index, z, facing } of faces) {
        const node = faceNodes.current[index];
        if (!node) continue;
        const [a, b, c] = FACES[index];
        node.setAttribute("points", `${flat[a].x.toFixed(2)},${flat[a].y.toFixed(2)} ${flat[b].x.toFixed(2)},${flat[b].y.toFixed(2)} ${flat[c].x.toFixed(2)},${flat[c].y.toFixed(2)}`);
        node.setAttribute("fill", mix((z + 1) / 2));
        node.setAttribute("fill-opacity", (0.05 + 0.22 * Math.max(0, facing)).toFixed(3));
        faceGroup.current?.appendChild(node);
      }
      const edges = EDGES.map(([a, b], index) => ({ index, a, b, z: (turned[a].z + turned[b].z) / 2 })).sort((p, q) => p.z - q.z);
      for (const { index, a, b, z } of edges) {
        const node = edgeNodes.current[index];
        if (!node) continue;
        const t = Math.min(1, Math.max(0, (z + 1) / 2));
        node.setAttribute("x1", flat[a].x.toFixed(2));
        node.setAttribute("y1", flat[a].y.toFixed(2));
        node.setAttribute("x2", flat[b].x.toFixed(2));
        node.setAttribute("y2", flat[b].y.toFixed(2));
        node.setAttribute("stroke", mix(t));
        node.setAttribute("stroke-opacity", (0.45 + 0.55 * t).toFixed(3));
        node.setAttribute("stroke-width", (2.6 + 2.2 * t).toFixed(2));
        edgeGroup.current?.appendChild(node);
      }
    };
    if (still) {
      draw(0.5);
      return;
    }
    const started = performance.now();
    const tick = (now: number) => {
      draw(((now - started) / 1000 / SECONDS_PER_TURN) * Math.PI * 2 + 0.5);
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, []);

  return (
    <svg width={size} height={size} viewBox="0 0 100 100" fill="none" role="img" aria-label="Working" className="mark prism-3d">
      <g ref={faceGroup}>
        {FACES.map((_, i) => <polygon key={i} ref={(node) => { faceNodes.current[i] = node; }} />)}
      </g>
      <g ref={edgeGroup} strokeLinecap="round">
        {EDGES.map((_, i) => <line key={i} ref={(node) => { edgeNodes.current[i] = node; }} />)}
      </g>
    </svg>
  );
}
