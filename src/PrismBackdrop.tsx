/**
 * A soft prism of glass-like facets in the top right corner, behind the page. It is only decoration:
 * it sits underneath everything, takes no clicks, and fades out toward the middle of the page. The
 * facets drift and breathe slowly, and a band of light passes over the glass now and then.
 */
export function PrismBackdrop() {
  return (
    <svg className="prism-backdrop" viewBox="0 0 560 260" preserveAspectRatio="xMaxYMin slice" aria-hidden="true" focusable="false">
      <defs>
        <linearGradient id="pbLavender" x1="0" y1="1" x2="1" y2="0">
          <stop offset="0" stopColor="#e7defa" stopOpacity=".15" />
          <stop offset=".55" stopColor="#d3c4f5" stopOpacity=".7" />
          <stop offset="1" stopColor="#c6b7f1" stopOpacity=".85" />
        </linearGradient>
        <linearGradient id="pbBlue" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#ddd6f8" stopOpacity=".7" />
          <stop offset=".6" stopColor="#b9c8f5" stopOpacity=".8" />
          <stop offset="1" stopColor="#a8bdf1" stopOpacity=".7" />
        </linearGradient>
        <linearGradient id="pbCyan" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#d8f4f7" stopOpacity=".95" />
          <stop offset="1" stopColor="#bfd9f7" stopOpacity=".8" />
        </linearGradient>
        <linearGradient id="pbDeep" x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#b6c0f2" stopOpacity=".55" />
          <stop offset="1" stopColor="#a4b4ee" stopOpacity=".8" />
        </linearGradient>
        <linearGradient id="pbSweep" x1="0" y1="0" x2="1" y2="0">
          <stop offset="0" stopColor="#ffffff" stopOpacity="0" />
          <stop offset=".5" stopColor="#ffffff" stopOpacity=".7" />
          <stop offset="1" stopColor="#ffffff" stopOpacity="0" />
        </linearGradient>
        <linearGradient id="pbShine" x1="0" y1="1" x2="1" y2="0">
          <stop offset="0" stopColor="#ffffff" stopOpacity="0" />
          <stop offset=".55" stopColor="#d9ccff" stopOpacity=".1" />
          <stop offset="1" stopColor="#ffffff" stopOpacity=".55" />
        </linearGradient>
        <radialGradient id="pbSpark">
          <stop offset="0" stopColor="#ffffff" stopOpacity="1" />
          <stop offset=".25" stopColor="#e6dcff" stopOpacity=".7" />
          <stop offset="1" stopColor="#a78bfa" stopOpacity="0" />
        </radialGradient>
        <clipPath id="pbGlass">
          <polygon points="372,0 22,206 290,150 382,108" />
          <polygon points="372,0 382,108 560,186 560,0" />
          <polygon points="382,108 560,186 560,212 300,212 290,150" />
          <polygon points="290,150 252,212 300,212" />
        </clipPath>
        <radialGradient id="pbHaze" cx=".78" cy=".05" r=".8">
          <stop offset="0" stopColor="#e6d9fa" stopOpacity=".75" />
          <stop offset=".6" stopColor="#e4e3fb" stopOpacity=".3" />
          <stop offset="1" stopColor="#ffffff" stopOpacity="0" />
        </radialGradient>
      </defs>
      <rect className="pb-haze" width="560" height="260" fill="url(#pbHaze)" />
      <polygon className="pb-facet pb-a" points="372,0 22,206 290,150 382,108" fill="url(#pbLavender)" />
      <polygon className="pb-facet pb-b" points="372,0 382,108 560,186 560,0" fill="url(#pbBlue)" />
      <polygon className="pb-facet pb-c" points="382,108 560,186 560,212 300,212 290,150" fill="url(#pbCyan)" opacity=".85" />
      <polygon className="pb-facet pb-d" points="290,150 252,212 300,212" fill="url(#pbDeep)" />
      {/* Gloss: only drawn in dark mode, where it gives the glass a lit, shiny face. */}
      <g className="pb-gloss" aria-hidden="true">
        <polygon points="372,0 22,206 290,150 382,108" fill="url(#pbShine)" />
        <polygon points="372,0 382,108 560,186 560,0" fill="url(#pbShine)" />
        <polygon points="382,108 560,186 560,212 300,212 290,150" fill="url(#pbShine)" />
        <circle cx="372" cy="2" r="34" fill="url(#pbSpark)" />
        <circle cx="382" cy="108" r="22" fill="url(#pbSpark)" />
        <circle cx="290" cy="150" r="16" fill="url(#pbSpark)" />
        <circle cx="560" cy="186" r="14" fill="url(#pbSpark)" />
      </g>
      <g clipPath="url(#pbGlass)">
        <rect className="pb-sweep" x="-160" y="-60" width="130" height="380" fill="url(#pbSweep)" transform="skewX(-24)" />
      </g>
      <g className="pb-ridges" stroke="#ffffff" strokeLinecap="round" fill="none" opacity=".85">
        <path d="M372 0 382 108" strokeWidth="1.4" />
        <path d="M382 108 290 150" strokeWidth="1.2" />
        <path d="M382 108 560 186" strokeWidth="1.2" />
        <path d="M372 0 252 212" strokeWidth="1" opacity=".7" />
        <path d="M372 0 22 206" strokeWidth="1" opacity=".5" />
      </g>
    </svg>
  );
}
