import { useId } from "react";
import { Prism3D } from "./Prism3D";

/**
 * The Prism mark: a white wire-frame crystal on a dark tile. Keep in step with public/favicon.svg
 * and src-tauri/icons/app-icon.svg. With `thinking` it breathes, which is how the app shows that
 * the AI is working.
 */
export function Logo({ size = 36, thinking = false, mark = false }: { size?: number; thinking?: boolean; mark?: boolean }) {
  const id = useId().replace(/:/g, "");
  if (mark) return <Mark size={size} thinking={thinking} />;
  return (
    <svg width={size} height={size} viewBox="25 20 1045 1045" fill="none" role="img" aria-label="Prism Workspace logo" className={thinking ? "logo-thinking" : undefined}>
      <defs>
        <linearGradient id={`${id}up`} x1="0" y1="0" x2="0" y2="1"><stop offset="0" stopColor="#7a808b" /><stop offset="1" stopColor="#3b404b" /></linearGradient>
        <linearGradient id={`${id}dn`} x1="0" y1="0" x2="1" y2="0"><stop offset="0" stopColor="#6b717c" /><stop offset=".6" stopColor="#9aa0aa" /><stop offset="1" stopColor="#f1f3f6" /></linearGradient>
        <linearGradient id={`${id}st`} x1="0" y1="1" x2="1" y2="0"><stop offset="0" stopColor="#fff" stopOpacity="0" /><stop offset="1" stopColor="#fff" stopOpacity=".8" /></linearGradient>
      </defs>
      <rect x="25" y="20" width="1045" height="1045" rx="235" fill="#1a1d26" />
      <polygon points="549,184 872,716 685,657 440,657 226,716" fill={`url(#${id}up)`} opacity=".8" />
      <polygon points="222,372 549,495 876,372 549,902" fill={`url(#${id}dn)`} opacity=".8" />
      <polygon points="222,716 440,657 685,657 876,716 876,724 549,902 222,724" fill="#3c414c" opacity=".85" />
      <g stroke="#f4f6fa" strokeWidth="28" strokeLinejoin="round" strokeLinecap="round" fill="none">
        <path d="M549 184 876 366 876 724 549 902 222 724 222 366Z" />
        <path d="M549 184 222 724M549 184 876 724M549 902 222 366M549 902 876 366" />
        <path d="M222 372 549 495 876 372" />
        <path d="M222 716 440 657 685 657 876 716" />
      </g>
      <path d="M772 520 858 424" stroke={`url(#${id}st)`} strokeWidth="9" strokeLinecap="round" />
    </svg>
  );
}

const OUTLINES = [
  "M549 184 876 366 876 724 549 902 222 724 222 366Z",
  "M549 184 222 724M549 184 876 724M549 902 222 366M549 902 876 366",
  "M222 372 549 495 876 372",
  "M222 716 440 657 685 657 876 716",
];

/**
 * The crystal on its own, on a clear background, for the AI's avatar and its loading icon. At rest
 * it is the flat logo in dark grey; while `thinking` it becomes a real 3D prism turning in space
 * (see Prism3D).
 */
function Mark({ size, thinking }: { size: number; thinking: boolean }) {
  if (thinking) return <Prism3D size={size} />;
  return (
    <svg width={size} height={size} viewBox="174 168 750 750" fill="none" role="img" aria-label="Prism Workspace logo" className="mark">
      <Faces />
      <g stroke="#43474e" strokeWidth="28" strokeLinejoin="round" strokeLinecap="round" fill="none">
        {OUTLINES.map((d) => <path key={d} d={d} />)}
      </g>
    </svg>
  );
}

function Faces() {
  return (
    <>
      <polygon points="549,184 872,716 685,657 440,657 226,716" fill="#43474e" opacity=".13" />
      <polygon points="222,372 549,495 876,372 549,902" fill="#43474e" opacity=".13" />
      <polygon points="222,716 440,657 685,657 876,716 876,724 549,902 222,724" fill="#43474e" opacity=".24" />
    </>
  );
}
