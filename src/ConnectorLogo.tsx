/** The service's own mark with no tile around it. Nothing for a service it does not know. */
export function ConnectorMark({ kind, size }: { kind: string; size: number }) {
  if (kind === "google_drive") {
    return (
      <svg width={size} height={size * 0.9} viewBox="0 0 87.3 78" role="img" aria-label="Google Drive">
        <path d="m6.6 66.85 3.85 6.65c.8 1.4 1.95 2.5 3.3 3.3l13.75-23.8h-27.5c0 1.55.4 3.1 1.2 4.5z" fill="#0066da" />
        <path d="m43.65 25-13.75-23.8c-1.35.8-2.5 1.9-3.3 3.3l-25.4 44a9.06 9.06 0 0 0-1.2 4.5h27.5z" fill="#00ac47" />
        <path d="m73.55 76.8c1.35-.8 2.5-1.9 3.3-3.3l1.6-2.75 7.65-13.25c.8-1.4 1.2-2.95 1.2-4.5h-27.502l5.852 11.5z" fill="#ea4335" />
        <path d="m43.65 25 13.75-23.8c-1.35-.8-2.9-1.2-4.5-1.2h-18.5c-1.6 0-3.15.45-4.5 1.2z" fill="#00832d" />
        <path d="m59.8 53h-32.3l-13.75 23.8c1.35.8 2.9 1.2 4.5 1.2h50.8c1.6 0 3.15-.45 4.5-1.2z" fill="#2684fc" />
        <path d="m73.4 26.5-12.7-22c-.8-1.4-1.95-2.5-3.3-3.3l-13.75 23.8 16.15 28h27.45c0-1.55-.4-3.1-1.2-4.5z" fill="#ffba00" />
      </svg>
    );
  }
  if (kind === "dropbox") {
    return (
      <svg width={size} height={size} viewBox="0 0 24 24" role="img" aria-label="Dropbox">
        <path fill="#0061FF" d="M6 1.807L0 5.629l6 3.822 6.001-3.822L6 1.807zM18 1.807l-6 3.822 6 3.822 6-3.822-6-3.822zM0 13.274l6 3.822 6.001-3.822L6 9.452l-6 3.822zM18 9.452l-6 3.822 6 3.822 6-3.822-6-3.822zM6 18.371l6.001 3.822 6-3.822-6-3.822L6 18.371z" />
      </svg>
    );
  }
  if (kind === "docusign") {
    // DocuSign's mark: a coral rounded "D", a violet corner, and a black square where they meet.
    return (
      <svg width={size * 0.8} height={size * 0.8} viewBox="0 0 1546 1546" role="img" aria-label="DocuSign">
        <path fill="#ff5555" d="M470 0H1050C1330 0 1546 260 1546 560C1546 880 1360 1113 1113 1113H432V38A38 38 0 0 1 470 0Z" />
        <path fill="#4c00ff" d="M38 433H432V1075A38 38 0 0 0 470 1113H1113V1508A38 38 0 0 1 1075 1546H38A38 38 0 0 1 0 1508V471A38 38 0 0 1 38 433Z" />
        <path fill="#000" d="M432 433H850A38 38 0 0 1 877 444L1100 663A38 38 0 0 1 1113 693V1113H470A38 38 0 0 1 432 1075Z" />
      </svg>
    );
  }
  if (kind === "zoom") {
    return (
      <svg width={size} height={size * 0.62} viewBox="0 0 64 40" role="img" aria-label="Zoom">
        <rect width="64" height="40" rx="10" fill="#2d8cff" />
        <rect x="9" y="11" width="30" height="18" rx="5" fill="#fff" />
        <path d="M41 17.5 53 11v18l-12-6.5z" fill="#fff" />
      </svg>
    );
  }
  if (kind === "quickbooks") {
    return (
      <svg width={size} height={size} viewBox="0 0 32 32" role="img" aria-label="QuickBooks">
        <circle cx="16" cy="16" r="16" fill="#2ca01c" />
        <path d="M9 11.5h3.2a4.5 4.5 0 0 1 0 9H11" stroke="#fff" strokeWidth="2.4" strokeLinecap="round" fill="none" />
        <path d="M23 20.5h-3.2a4.5 4.5 0 0 1 0-9H21" stroke="#fff" strokeWidth="2.4" strokeLinecap="round" fill="none" />
      </svg>
    );
  }
  if (kind === "microsoft365" || kind === "outlook_mail") {
    return (
      <svg width={size * 0.85} height={size * 0.85} viewBox="0 0 23 23" role="img" aria-label="Microsoft 365">
        <path fill="#f25022" d="M1 1h10v10H1z" />
        <path fill="#7fba00" d="M12 1h10v10H12z" />
        <path fill="#00a4ef" d="M1 12h10v10H1z" />
        <path fill="#ffb900" d="M12 12h10v10H12z" />
      </svg>
    );
  }
  return null;
}

/** The service's own mark on a tile. Unknown services get their first letter. */
export function ConnectorLogo({ kind, name, small = false }: { kind: string; name: string; small?: boolean }) {
  const size = small ? 32 : 40;
  const mark = <ConnectorMark kind={kind} size={size * 0.65} />;
  return (
    <div className={`connector-logo ${small ? "small " : ""}${kind}`} aria-hidden="true">
      {["google_drive", "quickbooks", "zoom", "microsoft365", "dropbox", "docusign", "outlook_mail"].includes(kind) ? mark : name.slice(0, 1)}
    </div>
  );
}
