export const repo = 'https://github.com/ahonn/markraft';
export const appStoreUrl = 'https://apps.apple.com/app/id6819551024';
// The release workflow uploads the newest disk image under this fixed name.
export const downloadUrl = `${repo}/releases/latest/download/Markraft.dmg`;

// Keep the existing event name and position property for analytics continuity.
// These classes count outbound clicks, not completed downloads or purchases.
export const downloadEvent = (position: 'header' | 'hero' | 'footer', channel: 'app-store' | 'direct') =>
  `plausible-event-name=Download plausible-event-position=${position} plausible-event-channel=${channel}`;
