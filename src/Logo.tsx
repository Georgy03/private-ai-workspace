// Black-and-white node-network mark. Keep in step with public/favicon.svg.
export function Logo({ size = 36 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 40 40" fill="none" role="img" aria-label="Private AI logo">
      <rect width="40" height="40" rx="10" fill="#000" />
      <g stroke="#fff" strokeLinecap="round">
        <path d="M20 20 12 12M20 20 28 10M20 20 31 22M20 20 22 31M20 20 10 27" strokeWidth="1.5" />
        <path d="M12 12 28 10 31 22 22 31 10 27Z" strokeWidth="1" opacity=".45" strokeLinejoin="round" />
      </g>
      <g fill="#fff">
        <circle cx="20" cy="20" r="3.6" />
        <circle cx="12" cy="12" r="2.4" />
        <circle cx="28" cy="10" r="2" />
        <circle cx="31" cy="22" r="2.6" />
        <circle cx="22" cy="31" r="2.2" />
        <circle cx="10" cy="27" r="2" />
      </g>
    </svg>
  );
}
